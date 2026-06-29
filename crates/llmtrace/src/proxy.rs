use std::pin::Pin;
use std::sync::{Arc, Mutex as StdMutex};
use std::task::{Context, Poll};
use std::time::Instant;

use axum::body::Body;
use axum::extract::State;
use axum::extract::ws::rejection::WebSocketUpgradeRejection;
use axum::extract::ws::{Message as AxumWsMessage, WebSocket, WebSocketUpgrade};
use axum::http::{HeaderMap, HeaderName, Request, Response, StatusCode, Uri, header};
use axum::response::IntoResponse;
use bytes::Bytes;
use futures_util::{SinkExt, Stream, StreamExt};
use serde_json::{Value, json};
use tokio::sync::Mutex as AsyncMutex;
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Message as TungsteniteMessage;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use url::Url;
use uuid::Uuid;

use crate::redaction::{headers_to_json, redact_headers};
use crate::state::AppState;
use crate::trace::{self, TraceEvent};
use crate::types::RequestKind;

const HOP_BY_HOP_HEADERS: &[&str] = &[
    "connection",
    "keep-alive",
    "proxy-authenticate",
    "proxy-authorization",
    "te",
    "trailer",
    "transfer-encoding",
    "upgrade",
];

pub async fn proxy(
    State(state): State<AppState>,
    ws: Result<WebSocketUpgrade, WebSocketUpgradeRejection>,
    request: Request<Body>,
) -> axum::response::Response {
    if is_websocket(request.headers()) {
        return match ws {
            Ok(ws) => proxy_websocket(state, ws, request).await,
            Err(error) => (
                StatusCode::BAD_REQUEST,
                axum::Json(json!({"error": error.to_string()})),
            )
                .into_response(),
        };
    }

    match proxy_http(state, request).await {
        Ok(response) => response,
        Err(error) => {
            tracing::warn!(%error, "proxy request failed");
            (
                StatusCode::BAD_GATEWAY,
                axum::Json(json!({"error": "upstream request failed"})),
            )
                .into_response()
        }
    }
}

async fn proxy_http(
    state: AppState,
    request: Request<Body>,
) -> anyhow::Result<axum::response::Response> {
    let trace_id = Uuid::new_v4();
    let started_at = chrono::Utc::now();
    let started = Instant::now();
    let (parts, body) = request.into_parts();
    let method = parts.method.clone();
    let original_uri = parts.uri.to_string();
    let upstream_url = resolve_upstream(&state, &parts.uri, &parts.headers)?;
    enforce_upstream_allowlist(&state, &upstream_url)?;
    let upstream_host = upstream_url.host_str().map(str::to_string);
    let capture_limit = state.config.proxy.max_body_capture_bytes;

    let request_capture = SharedBodyCapture::default();
    let redacted_request_headers = redact_headers(&parts.headers, &state.config.redaction);
    let plugin_request_headers = headers_to_json(&parts.headers);
    let request_secret_hash = redacted_request_headers.first_secret_hash.clone();
    let request_body = RequestCaptureStream::new(body, request_capture.clone(), capture_limit);

    let mut upstream_request = state
        .http
        .request(
            reqwest::Method::from_bytes(method.as_str().as_bytes())?,
            upstream_url.clone(),
        )
        .body(reqwest::Body::wrap_stream(request_body));
    for (name, value) in parts.headers.iter() {
        if should_forward_header(name) {
            upstream_request = upstream_request.header(name.as_str(), value.as_bytes());
        }
    }

    let upstream_response = match upstream_request.send().await {
        Ok(response) => response,
        Err(error) => {
            let request = request_capture.snapshot();
            state.traces.record(TraceEvent {
                completed_at: Some(chrono::Utc::now()),
                method: method.to_string(),
                original_uri,
                upstream_url: upstream_url.to_string(),
                upstream_host,
                error: Some(error.to_string()),
                api_key_hash: request_secret_hash,
                duration_ms: Some(started.elapsed().as_millis() as i64),
                request_body_bytes: request.total_bytes,
                request_headers: redacted_request_headers.json,
                plugin_request_headers,
                request_body: request.bytes,
                request_body_truncated: request.truncated,
                run_plugins: true,
                ..TraceEvent::base(trace_id, started_at)
            });
            anyhow::bail!(error);
        }
    };

    let status = upstream_response.status();
    let response_headers = upstream_response.headers().clone();
    let redacted_response_headers = redact_headers(&response_headers, &state.config.redaction);
    let content_type = response_headers
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    let plugin_response_headers = headers_to_json(&response_headers);

    let event = TraceEvent {
        method: method.to_string(),
        original_uri,
        upstream_url: upstream_url.to_string(),
        upstream_host,
        status: Some(status.as_u16() as i32),
        api_key_hash: request_secret_hash,
        request_headers: redacted_request_headers.json,
        response_headers: redacted_response_headers.json,
        plugin_request_headers,
        plugin_response_headers,
        content_type,
        run_plugins: true,
        ..TraceEvent::base(trace_id, started_at)
    };

    let mut response_builder = Response::builder().status(status);
    for (name, value) in response_headers.iter() {
        if should_forward_response_header(name) {
            response_builder = response_builder.header(name, value);
        }
    }
    let response_body = ResponseCaptureStream::new(
        upstream_response.bytes_stream(),
        state.traces.clone(),
        request_capture,
        event,
        started,
        capture_limit,
    );
    Ok(response_builder.body(Body::from_stream(response_body))?)
}

#[derive(Clone, Default)]
struct SharedBodyCapture {
    inner: Arc<StdMutex<BodyCapture>>,
}

#[derive(Debug, Clone, Default)]
struct BodyCapture {
    bytes: Vec<u8>,
    total_bytes: i64,
    truncated: bool,
}

impl SharedBodyCapture {
    fn push(&self, chunk: &[u8], limit: usize) {
        let mut capture = self
            .inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        capture.total_bytes += chunk.len() as i64;

        if capture.bytes.len() < limit {
            let remaining = limit - capture.bytes.len();
            let captured = remaining.min(chunk.len());
            capture.bytes.extend_from_slice(&chunk[..captured]);
            capture.truncated |= captured < chunk.len();
        } else {
            capture.truncated |= !chunk.is_empty();
        }
    }

    fn snapshot(&self) -> BodyCapture {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }
}

struct RequestCaptureStream {
    inner: Pin<Box<dyn Stream<Item = Result<Bytes, axum::Error>> + Send>>,
    capture: SharedBodyCapture,
    limit: usize,
}

impl RequestCaptureStream {
    fn new(body: Body, capture: SharedBodyCapture, limit: usize) -> Self {
        Self {
            inner: Box::pin(body.into_data_stream()),
            capture,
            limit,
        }
    }
}

impl Stream for RequestCaptureStream {
    type Item = Result<Bytes, axum::Error>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        match self.inner.as_mut().poll_next(cx) {
            Poll::Ready(Some(Ok(chunk))) => {
                self.capture.push(&chunk, self.limit);
                Poll::Ready(Some(Ok(chunk)))
            }
            other => other,
        }
    }
}

struct ResponseCaptureStream {
    inner: Pin<Box<dyn Stream<Item = reqwest::Result<Bytes>> + Send>>,
    recorder: trace::TraceRecorder,
    request_capture: SharedBodyCapture,
    event: Option<TraceEvent>,
    started: Instant,
    capture: BodyCapture,
    first_byte_ms: Option<i64>,
    limit: usize,
}

impl ResponseCaptureStream {
    fn new(
        inner: impl Stream<Item = reqwest::Result<Bytes>> + Send + 'static,
        recorder: trace::TraceRecorder,
        request_capture: SharedBodyCapture,
        event: TraceEvent,
        started: Instant,
        limit: usize,
    ) -> Self {
        Self {
            inner: Box::pin(inner),
            recorder,
            request_capture,
            event: Some(event),
            started,
            capture: BodyCapture::default(),
            first_byte_ms: None,
            limit,
        }
    }

    fn finish(&mut self, error: Option<String>) {
        let Some(mut event) = self.event.take() else {
            return;
        };

        let request = self.request_capture.snapshot();
        event.completed_at = Some(chrono::Utc::now());
        event.duration_ms = Some(self.started.elapsed().as_millis() as i64);
        event.ttft_ms = self.first_byte_ms;
        event.error = error;
        event.request_body = request.bytes;
        event.request_body_bytes = request.total_bytes;
        event.request_body_truncated = request.truncated;
        event.response_body = std::mem::take(&mut self.capture.bytes);
        event.response_body_bytes = self.capture.total_bytes;
        event.response_body_truncated = self.capture.truncated;
        self.recorder.record(event);
    }
}

impl Stream for ResponseCaptureStream {
    type Item = Result<Bytes, std::io::Error>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        match self.inner.as_mut().poll_next(cx) {
            Poll::Ready(Some(Ok(chunk))) => {
                if self.first_byte_ms.is_none() {
                    self.first_byte_ms = Some(self.started.elapsed().as_millis() as i64);
                }
                let limit = self.limit;
                self.capture.total_bytes += chunk.len() as i64;
                if self.capture.bytes.len() < limit {
                    let remaining = limit - self.capture.bytes.len();
                    let captured = remaining.min(chunk.len());
                    self.capture.bytes.extend_from_slice(&chunk[..captured]);
                    self.capture.truncated |= captured < chunk.len();
                } else {
                    self.capture.truncated |= !chunk.is_empty();
                }
                Poll::Ready(Some(Ok(chunk)))
            }
            Poll::Ready(Some(Err(error))) => {
                let message = error.to_string();
                self.finish(Some(message.clone()));
                Poll::Ready(Some(Err(std::io::Error::other(message))))
            }
            Poll::Ready(None) => {
                self.finish(None);
                Poll::Ready(None)
            }
            Poll::Pending => Poll::Pending,
        }
    }
}

impl Drop for ResponseCaptureStream {
    fn drop(&mut self) {
        self.finish(Some(
            "response stream dropped before completion".to_string(),
        ));
    }
}

async fn proxy_websocket(
    state: AppState,
    ws: WebSocketUpgrade,
    request: Request<Body>,
) -> axum::response::Response {
    let trace_id = Uuid::new_v4();
    let started_at = chrono::Utc::now();
    let (parts, _) = request.into_parts();
    let original_uri = parts.uri.to_string();
    let upstream_url = match resolve_upstream(&state, &parts.uri, &parts.headers)
        .and_then(http_to_ws_url)
        .and_then(|url| {
            enforce_upstream_allowlist(&state, &url)?;
            Ok(url)
        }) {
        Ok(url) => url,
        Err(error) => {
            tracing::warn!(%error, "websocket upstream resolution failed");
            return (
                StatusCode::BAD_GATEWAY,
                axum::Json(json!({"error": "failed to resolve websocket upstream"})),
            )
                .into_response();
        }
    };
    let redacted_headers = redact_headers(&parts.headers, &state.config.redaction);
    let upstream_host = upstream_url.host_str().map(str::to_string);

    ws.on_upgrade(move |socket| async move {
        handle_websocket(
            state,
            socket,
            trace_id,
            started_at,
            original_uri,
            upstream_url,
            upstream_host,
            redacted_headers.json,
            redacted_headers.first_secret_hash,
            parts.headers,
        )
        .await;
    })
}

#[allow(clippy::too_many_arguments)]
async fn handle_websocket(
    state: AppState,
    socket: WebSocket,
    trace_id: Uuid,
    started_at: chrono::DateTime<chrono::Utc>,
    original_uri: String,
    upstream_url: Url,
    upstream_host: Option<String>,
    request_headers: Value,
    api_key_hash: Option<String>,
    original_headers: HeaderMap,
) {
    let started = Instant::now();
    let mut upstream_request = match upstream_url.to_string().into_client_request() {
        Ok(request) => request,
        Err(error) => {
            persist_ws_error(
                &state,
                trace_id,
                started_at,
                original_uri,
                upstream_url,
                upstream_host,
                request_headers,
                api_key_hash,
                error.to_string(),
                started.elapsed().as_millis() as i64,
            )
            .await;
            return;
        }
    };
    for (name, value) in original_headers.iter() {
        if should_forward_header(name)
            && let Ok(header_name) = tokio_tungstenite::tungstenite::http::HeaderName::from_bytes(
                name.as_str().as_bytes(),
            )
            && let Ok(header_value) =
                tokio_tungstenite::tungstenite::http::HeaderValue::from_bytes(value.as_bytes())
        {
            upstream_request
                .headers_mut()
                .insert(header_name, header_value);
        }
    }

    let upstream = connect_async(upstream_request).await;
    let (upstream_socket, upstream_response) = match upstream {
        Ok(value) => value,
        Err(error) => {
            persist_ws_error(
                &state,
                trace_id,
                started_at,
                original_uri,
                upstream_url,
                upstream_host,
                request_headers,
                api_key_hash,
                error.to_string(),
                started.elapsed().as_millis() as i64,
            )
            .await;
            return;
        }
    };

    let response_headers = headers_to_json_axum_compat(upstream_response.headers());
    let stats = Arc::new(AsyncMutex::new(WsStats::default()));
    let (mut client_tx, mut client_rx) = socket.split();
    let (mut upstream_tx, mut upstream_rx) = upstream_socket.split();
    let c2u_stats = stats.clone();
    let u2c_stats = stats.clone();

    let client_to_upstream = async move {
        while let Some(message) = client_rx.next().await {
            let message = message?;
            {
                let mut stats = c2u_stats.lock().await;
                stats.client_frames += 1;
                stats.bytes_in += ws_message_size(&message) as i64;
                capture_ws_message(&mut stats.frames, "client", &message);
            }
            if let Some(message) = axum_to_tungstenite(message) {
                upstream_tx.send(message).await?;
            }
        }
        anyhow::Ok(())
    };

    let upstream_to_client = async move {
        while let Some(message) = upstream_rx.next().await {
            let message = message?;
            {
                let mut stats = u2c_stats.lock().await;
                stats.upstream_frames += 1;
                stats.bytes_out += tungstenite_message_size(&message) as i64;
                capture_tungstenite_message(&mut stats.frames, "upstream", &message);
            }
            if let Some(message) = tungstenite_to_axum(message) {
                client_tx.send(message).await?;
            }
        }
        anyhow::Ok(())
    };

    let (left, right) = tokio::join!(client_to_upstream, upstream_to_client);
    let error = left
        .err()
        .or_else(|| right.err())
        .map(|error| error.to_string());
    let stats = stats.lock().await.clone();
    let duration_ms = started.elapsed().as_millis() as i64;
    let session_key =
        trace::fallback_session_key(upstream_host.as_deref(), api_key_hash.as_deref());
    state.traces.record(TraceEvent {
        completed_at: Some(chrono::Utc::now()),
        method: "GET".to_string(),
        original_uri,
        upstream_url: upstream_url.to_string(),
        upstream_host,
        status: Some(101),
        error,
        request_kind: Some(RequestKind::WebSocket),
        api_key_hash,
        session_key,
        duration_ms: Some(duration_ms),
        request_body_bytes: stats.bytes_in,
        response_body_bytes: stats.bytes_out,
        request_headers,
        response_headers: response_headers.clone(),
        plugin_response_headers: response_headers,
        response_body: json!(stats.frames).to_string().into_bytes(),
        response_body_truncated: stats.frames_truncated,
        content_type: Some("websocket".to_string()),
        plugin_metadata: json!({
            "websocket": {
                "client_frames": stats.client_frames,
                "upstream_frames": stats.upstream_frames
            }
        }),
        tags: vec!["websocket".to_string()],
        ..TraceEvent::base(trace_id, started_at)
    });
}

#[allow(clippy::too_many_arguments)]
async fn persist_ws_error(
    state: &AppState,
    trace_id: Uuid,
    started_at: chrono::DateTime<chrono::Utc>,
    original_uri: String,
    upstream_url: Url,
    upstream_host: Option<String>,
    request_headers: Value,
    api_key_hash: Option<String>,
    error: String,
    duration_ms: i64,
) {
    state.traces.record(TraceEvent {
        completed_at: Some(chrono::Utc::now()),
        method: "GET".to_string(),
        original_uri,
        upstream_url: upstream_url.to_string(),
        upstream_host,
        error: Some(error),
        request_kind: Some(RequestKind::WebSocket),
        api_key_hash,
        duration_ms: Some(duration_ms),
        request_headers,
        content_type: Some("websocket".to_string()),
        tags: vec!["websocket".to_string()],
        ..TraceEvent::base(trace_id, started_at)
    });
}

#[derive(Debug, Clone, Default)]
struct WsStats {
    client_frames: i64,
    upstream_frames: i64,
    bytes_in: i64,
    bytes_out: i64,
    frames: Vec<Value>,
    frames_truncated: bool,
}

fn capture_ws_message(frames: &mut Vec<Value>, direction: &str, message: &AxumWsMessage) {
    if frames.len() >= 200 {
        return;
    }
    match message {
        AxumWsMessage::Text(text) => {
            frames.push(json!({"direction": direction, "type": "text", "text": text.to_string()}))
        }
        AxumWsMessage::Binary(bytes) => {
            frames.push(json!({"direction": direction, "type": "binary", "bytes": bytes.len()}))
        }
        AxumWsMessage::Ping(bytes) => {
            frames.push(json!({"direction": direction, "type": "ping", "bytes": bytes.len()}))
        }
        AxumWsMessage::Pong(bytes) => {
            frames.push(json!({"direction": direction, "type": "pong", "bytes": bytes.len()}))
        }
        AxumWsMessage::Close(_) => frames.push(json!({"direction": direction, "type": "close"})),
    }
}

fn capture_tungstenite_message(
    frames: &mut Vec<Value>,
    direction: &str,
    message: &TungsteniteMessage,
) {
    if frames.len() >= 200 {
        return;
    }
    match message {
        TungsteniteMessage::Text(text) => {
            frames.push(json!({"direction": direction, "type": "text", "text": text.to_string()}))
        }
        TungsteniteMessage::Binary(bytes) => {
            frames.push(json!({"direction": direction, "type": "binary", "bytes": bytes.len()}))
        }
        TungsteniteMessage::Ping(bytes) => {
            frames.push(json!({"direction": direction, "type": "ping", "bytes": bytes.len()}))
        }
        TungsteniteMessage::Pong(bytes) => {
            frames.push(json!({"direction": direction, "type": "pong", "bytes": bytes.len()}))
        }
        TungsteniteMessage::Close(_) => {
            frames.push(json!({"direction": direction, "type": "close"}))
        }
        TungsteniteMessage::Frame(_) => {
            frames.push(json!({"direction": direction, "type": "frame"}))
        }
    }
}

fn axum_to_tungstenite(message: AxumWsMessage) -> Option<TungsteniteMessage> {
    match message {
        AxumWsMessage::Text(text) => Some(TungsteniteMessage::Text(text.to_string().into())),
        AxumWsMessage::Binary(bytes) => Some(TungsteniteMessage::Binary(bytes)),
        AxumWsMessage::Ping(bytes) => Some(TungsteniteMessage::Ping(bytes)),
        AxumWsMessage::Pong(bytes) => Some(TungsteniteMessage::Pong(bytes)),
        AxumWsMessage::Close(_) => Some(TungsteniteMessage::Close(None)),
    }
}

fn tungstenite_to_axum(message: TungsteniteMessage) -> Option<AxumWsMessage> {
    match message {
        TungsteniteMessage::Text(text) => Some(AxumWsMessage::Text(text.to_string().into())),
        TungsteniteMessage::Binary(bytes) => Some(AxumWsMessage::Binary(bytes)),
        TungsteniteMessage::Ping(bytes) => Some(AxumWsMessage::Ping(bytes)),
        TungsteniteMessage::Pong(bytes) => Some(AxumWsMessage::Pong(bytes)),
        TungsteniteMessage::Close(_) => Some(AxumWsMessage::Close(None)),
        TungsteniteMessage::Frame(_) => None,
    }
}

fn ws_message_size(message: &AxumWsMessage) -> usize {
    match message {
        AxumWsMessage::Text(text) => text.len(),
        AxumWsMessage::Binary(bytes) | AxumWsMessage::Ping(bytes) | AxumWsMessage::Pong(bytes) => {
            bytes.len()
        }
        AxumWsMessage::Close(_) => 0,
    }
}

fn tungstenite_message_size(message: &TungsteniteMessage) -> usize {
    match message {
        TungsteniteMessage::Text(text) => text.len(),
        TungsteniteMessage::Binary(bytes)
        | TungsteniteMessage::Ping(bytes)
        | TungsteniteMessage::Pong(bytes) => bytes.len(),
        TungsteniteMessage::Close(_) | TungsteniteMessage::Frame(_) => 0,
    }
}

fn resolve_upstream(state: &AppState, uri: &Uri, headers: &HeaderMap) -> anyhow::Result<Url> {
    if let Some(value) = headers.get(state.config.proxy.upstream_header.as_str()) {
        let value = value.to_str()?;
        let mut url = Url::parse(value)?;
        if url.path() == "/"
            && let Some(path_and_query) = uri.path_and_query()
        {
            url.set_path(path_and_query.path());
            url.set_query(path_and_query.query());
        }
        return Ok(url);
    }

    if uri.scheme().is_some() && uri.authority().is_some() {
        return Ok(Url::parse(&uri.to_string())?);
    }

    let mut base = Url::parse(&state.config.proxy.default_upstream)?;
    if let Some(path_and_query) = uri.path_and_query() {
        base.set_path(path_and_query.path());
        base.set_query(path_and_query.query());
    }
    Ok(base)
}

fn enforce_upstream_allowlist(state: &AppState, upstream_url: &Url) -> anyhow::Result<()> {
    if state.upstream_allowlist.allows(upstream_url) {
        Ok(())
    } else {
        anyhow::bail!("upstream {upstream_url} is not allowed")
    }
}

fn http_to_ws_url(mut url: Url) -> anyhow::Result<Url> {
    let scheme = match url.scheme() {
        "http" => "ws",
        "https" => "wss",
        "ws" | "wss" => return Ok(url),
        other => anyhow::bail!("cannot proxy websocket to scheme {other}"),
    };
    url.set_scheme(scheme)
        .map_err(|_| anyhow::anyhow!("failed to set websocket scheme"))?;
    Ok(url)
}

fn should_forward_header(name: &HeaderName) -> bool {
    let name = name.as_str().to_ascii_lowercase();
    !HOP_BY_HOP_HEADERS.contains(&name.as_str()) && !name.starts_with("x-llmtrace-")
}

fn should_forward_response_header(name: &HeaderName) -> bool {
    let name = name.as_str().to_ascii_lowercase();
    !HOP_BY_HOP_HEADERS.contains(&name.as_str())
}

fn is_websocket(headers: &HeaderMap) -> bool {
    headers
        .get(header::UPGRADE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.eq_ignore_ascii_case("websocket"))
}

fn headers_to_json_axum_compat(headers: &tokio_tungstenite::tungstenite::http::HeaderMap) -> Value {
    let mut map = serde_json::Map::new();
    for (name, value) in headers {
        let value = value
            .to_str()
            .map(str::to_string)
            .unwrap_or_else(|_| "<non-utf8>".to_string());
        map.insert(name.as_str().to_ascii_lowercase(), json!(value));
    }
    Value::Object(map)
}
