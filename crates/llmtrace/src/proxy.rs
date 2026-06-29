use std::pin::Pin;
use std::sync::{Arc, Mutex as StdMutex};
use std::task::{Context, Poll};
use std::time::{Duration, Instant};
use std::{fmt, io};

use axum::body::Body;
use axum::extract::State;
use axum::extract::ws::rejection::WebSocketUpgradeRejection;
use axum::extract::ws::{Message as AxumWsMessage, WebSocket, WebSocketUpgrade};
use axum::http::{HeaderMap, HeaderName, HeaderValue, Request, Response, StatusCode, Uri, header};
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

const REQUEST_BODY_LIMIT_ERROR: &str = "request body exceeds configured limit";
const TRACE_ID_HEADER: &str = "x-llmtrace-trace-id";

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
    let max_request_body_bytes = state.config.proxy.max_request_body_bytes;

    let request_capture = SharedBodyCapture::default();
    let redacted_request_headers = redact_headers(&parts.headers, &state.config.redaction);
    let plugin_request_headers = headers_to_json(&parts.headers);
    let request_secret_hash = redacted_request_headers.first_secret_hash.clone();

    if content_length_exceeds(&parts.headers, max_request_body_bytes) {
        let message = request_body_limit_message(max_request_body_bytes);
        state.traces.record(TraceEvent {
            completed_at: Some(chrono::Utc::now()),
            method: method.to_string(),
            original_uri,
            upstream_url: upstream_url.to_string(),
            upstream_host,
            status: Some(StatusCode::PAYLOAD_TOO_LARGE.as_u16() as i32),
            error: Some(message),
            api_key_hash: request_secret_hash,
            duration_ms: Some(started.elapsed().as_millis() as i64),
            request_headers: redacted_request_headers.json,
            plugin_request_headers,
            run_plugins: false,
            ..TraceEvent::base(trace_id, started_at)
        });
        return Ok(payload_too_large_response(max_request_body_bytes, trace_id));
    }

    let request_body = RequestCaptureStream::new(
        body,
        request_capture.clone(),
        capture_limit,
        max_request_body_bytes,
    );

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
            let request_limit_exceeded = request.limit_exceeded;
            let error_message = if request_limit_exceeded {
                request_body_limit_message(max_request_body_bytes)
            } else {
                error.to_string()
            };
            state.traces.record(TraceEvent {
                completed_at: Some(chrono::Utc::now()),
                method: method.to_string(),
                original_uri,
                upstream_url: upstream_url.to_string(),
                upstream_host,
                status: request_limit_exceeded
                    .then_some(StatusCode::PAYLOAD_TOO_LARGE.as_u16() as i32),
                error: Some(error_message),
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
            if request_limit_exceeded {
                return Ok(payload_too_large_response(max_request_body_bytes, trace_id));
            }
            tracing::warn!(%trace_id, %error, "upstream request failed");
            return Ok(upstream_error_response(trace_id));
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
    let mut response = response_builder.body(Body::from_stream(response_body))?;
    set_trace_id_header(response.headers_mut(), trace_id);
    Ok(response)
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
    limit_exceeded: bool,
}

impl SharedBodyCapture {
    fn push_request(&self, chunk: &[u8], capture_limit: usize, live_limit: usize) -> bool {
        let mut capture = self
            .inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        push_body_capture(&mut capture, chunk, capture_limit);
        if (capture.total_bytes as u128) > (live_limit as u128) {
            capture.limit_exceeded = true;
        }
        capture.limit_exceeded
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
    capture_limit: usize,
    live_limit: usize,
}

impl RequestCaptureStream {
    fn new(
        body: Body,
        capture: SharedBodyCapture,
        capture_limit: usize,
        live_limit: usize,
    ) -> Self {
        Self {
            inner: Box::pin(body.into_data_stream()),
            capture,
            capture_limit,
            live_limit,
        }
    }
}

impl Stream for RequestCaptureStream {
    type Item = Result<Bytes, io::Error>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        match self.inner.as_mut().poll_next(cx) {
            Poll::Ready(Some(Ok(chunk))) => {
                let limit_exceeded =
                    self.capture
                        .push_request(&chunk, self.capture_limit, self.live_limit);
                if limit_exceeded {
                    Poll::Ready(Some(Err(request_body_limit_io_error(self.live_limit))))
                } else {
                    Poll::Ready(Some(Ok(chunk)))
                }
            }
            Poll::Ready(Some(Err(error))) => Poll::Ready(Some(Err(io::Error::other(error)))),
            Poll::Ready(None) => Poll::Ready(None),
            Poll::Pending => Poll::Pending,
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

    let mut response = ws.on_upgrade(move |socket| async move {
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
    });
    set_trace_id_header(response.headers_mut(), trace_id);
    response
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

    let timeout_secs = state.config.proxy.timeout_secs;
    let upstream = match tokio::time::timeout(
        Duration::from_secs(timeout_secs),
        connect_async(upstream_request),
    )
    .await
    {
        Ok(result) => result.map_err(|error| error.to_string()),
        Err(_) => {
            tracing::warn!(
                %trace_id,
                timeout_secs,
                "websocket upstream connection timed out"
            );
            Err(websocket_connect_timeout_message(timeout_secs))
        }
    };
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
                error,
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
    let max_ws_message_bytes = state.config.proxy.max_websocket_message_bytes;
    let max_ws_session_bytes = state.config.proxy.max_websocket_session_bytes;

    let client_to_upstream = async move {
        while let Some(message) = client_rx.next().await {
            let message = message?;
            {
                let mut stats = c2u_stats.lock().await;
                let message_size = ws_message_size(&message);
                stats.bytes_in = add_ws_bytes(
                    "client",
                    stats.bytes_in,
                    message_size,
                    max_ws_message_bytes,
                    max_ws_session_bytes,
                )?;
                stats.client_frames += 1;
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
                let message_size = tungstenite_message_size(&message);
                stats.bytes_out = add_ws_bytes(
                    "upstream",
                    stats.bytes_out,
                    message_size,
                    max_ws_message_bytes,
                    max_ws_session_bytes,
                )?;
                stats.upstream_frames += 1;
                capture_tungstenite_message(&mut stats.frames, "upstream", &message);
            }
            if let Some(message) = tungstenite_to_axum(message) {
                client_tx.send(message).await?;
            }
        }
        anyhow::Ok(())
    };

    tokio::pin!(client_to_upstream);
    tokio::pin!(upstream_to_client);
    let error = tokio::select! {
        result = &mut client_to_upstream => websocket_bridge_error(result),
        result = &mut upstream_to_client => websocket_bridge_error(result),
    };
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

fn push_body_capture(capture: &mut BodyCapture, chunk: &[u8], limit: usize) {
    capture.total_bytes = capture.total_bytes.saturating_add(chunk.len() as i64);

    if capture.bytes.len() < limit {
        let remaining = limit - capture.bytes.len();
        let captured = remaining.min(chunk.len());
        capture.bytes.extend_from_slice(&chunk[..captured]);
        capture.truncated |= captured < chunk.len();
    } else {
        capture.truncated |= !chunk.is_empty();
    }
}

fn content_length_exceeds(headers: &HeaderMap, limit: usize) -> bool {
    headers
        .get(header::CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u128>().ok())
        .is_some_and(|content_length| content_length > limit as u128)
}

fn payload_too_large_response(limit: usize, trace_id: Uuid) -> axum::response::Response {
    let mut response = (
        StatusCode::PAYLOAD_TOO_LARGE,
        axum::Json(json!({
            "error": REQUEST_BODY_LIMIT_ERROR,
            "max_request_body_bytes": limit,
        })),
    )
        .into_response();
    set_trace_id_header(response.headers_mut(), trace_id);
    response
}

fn upstream_error_response(trace_id: Uuid) -> axum::response::Response {
    let mut response = (
        StatusCode::BAD_GATEWAY,
        axum::Json(json!({"error": "upstream request failed"})),
    )
        .into_response();
    set_trace_id_header(response.headers_mut(), trace_id);
    response
}

fn set_trace_id_header(headers: &mut HeaderMap, trace_id: Uuid) {
    let value = trace_id.to_string();
    if let Ok(value) = HeaderValue::from_str(&value) {
        headers.insert(HeaderName::from_static(TRACE_ID_HEADER), value);
    }
}

fn request_body_limit_message(limit: usize) -> String {
    format!("{REQUEST_BODY_LIMIT_ERROR} of {limit} bytes")
}

fn request_body_limit_io_error(limit: usize) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        RequestBodyLimitExceeded { limit },
    )
}

fn websocket_connect_timeout_message(timeout_secs: u64) -> String {
    format!("websocket upstream connection timed out after {timeout_secs} seconds")
}

fn websocket_bridge_error(result: anyhow::Result<()>) -> Option<String> {
    result.err().map(|error| error.to_string())
}

#[derive(Debug)]
struct RequestBodyLimitExceeded {
    limit: usize,
}

impl fmt::Display for RequestBodyLimitExceeded {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&request_body_limit_message(self.limit))
    }
}

impl std::error::Error for RequestBodyLimitExceeded {}

fn add_ws_bytes(
    direction: &str,
    current: i64,
    message_size: usize,
    max_message_bytes: usize,
    max_session_bytes: usize,
) -> anyhow::Result<i64> {
    if message_size > max_message_bytes {
        anyhow::bail!(
            "websocket {direction} message exceeds configured limit of {max_message_bytes} bytes"
        );
    }
    let next = current.saturating_add(message_size as i64);
    if (next as u128) > (max_session_bytes as u128) {
        anyhow::bail!(
            "websocket {direction} session exceeds configured limit of {max_session_bytes} bytes"
        );
    }
    Ok(next)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_length_exceeds_detects_oversized_request() {
        let mut headers = HeaderMap::new();
        headers.insert(header::CONTENT_LENGTH, "1025".parse().unwrap());

        assert!(content_length_exceeds(&headers, 1024));
        assert!(!content_length_exceeds(&headers, 1025));
    }

    #[test]
    fn request_capture_marks_live_limit_exceeded_without_overcapturing() {
        let capture = SharedBodyCapture::default();

        assert!(!capture.push_request(b"abc", 4, 5));
        assert!(capture.push_request(b"def", 4, 5));

        let snapshot = capture.snapshot();
        assert_eq!(snapshot.bytes, b"abcd");
        assert_eq!(snapshot.total_bytes, 6);
        assert!(snapshot.truncated);
        assert!(snapshot.limit_exceeded);
    }

    #[test]
    fn payload_too_large_response_includes_trace_id_header() {
        let trace_id = Uuid::new_v4();
        let trace_id_string = trace_id.to_string();

        let response = payload_too_large_response(1024, trace_id);

        assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
        assert_eq!(
            response
                .headers()
                .get(TRACE_ID_HEADER)
                .and_then(|value| value.to_str().ok()),
            Some(trace_id_string.as_str())
        );
    }

    #[test]
    fn upstream_error_response_includes_trace_id_header() {
        let trace_id = Uuid::new_v4();
        let trace_id_string = trace_id.to_string();

        let response = upstream_error_response(trace_id);

        assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
        assert_eq!(
            response
                .headers()
                .get(TRACE_ID_HEADER)
                .and_then(|value| value.to_str().ok()),
            Some(trace_id_string.as_str())
        );
    }

    #[test]
    fn set_trace_id_header_writes_uuid_value() {
        let trace_id = Uuid::new_v4();
        let trace_id_string = trace_id.to_string();
        let mut headers = HeaderMap::new();

        set_trace_id_header(&mut headers, trace_id);

        assert_eq!(
            headers
                .get(TRACE_ID_HEADER)
                .and_then(|value| value.to_str().ok()),
            Some(trace_id_string.as_str())
        );
    }

    #[test]
    fn add_ws_bytes_rejects_message_over_limit() {
        let error = add_ws_bytes("client", 0, 11, 10, 100)
            .unwrap_err()
            .to_string();

        assert!(error.contains("message exceeds configured limit"));
    }

    #[test]
    fn add_ws_bytes_rejects_session_over_limit() {
        let error = add_ws_bytes("upstream", 95, 6, 10, 100)
            .unwrap_err()
            .to_string();

        assert!(error.contains("session exceeds configured limit"));
    }

    #[test]
    fn add_ws_bytes_returns_updated_total_within_limits() {
        let total = add_ws_bytes("client", 4, 6, 10, 20).unwrap();

        assert_eq!(total, 10);
    }

    #[test]
    fn websocket_connect_timeout_message_reports_configured_limit() {
        assert_eq!(
            websocket_connect_timeout_message(12),
            "websocket upstream connection timed out after 12 seconds"
        );
    }

    #[test]
    fn websocket_bridge_error_reports_first_direction_error() {
        assert_eq!(
            websocket_bridge_error(Err(anyhow::anyhow!("client frame error"))),
            Some("client frame error".to_string())
        );
    }

    #[test]
    fn websocket_bridge_error_is_none_for_clean_direction_end() {
        assert_eq!(websocket_bridge_error(Ok(())), None);
    }
}
