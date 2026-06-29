use serde_json::Value;

use crate::storage::ParsedMessage;
use crate::types::RequestKind;

#[derive(Debug, Clone, Default)]
pub struct ParsedTrace {
    pub request_kind: RequestKind,
    pub model: Option<String>,
    pub messages: Vec<ParsedMessage>,
    pub session_key_hint: Option<String>,
}

pub fn parse_trace(uri: &str, request_body: &[u8], response_body: &[u8]) -> ParsedTrace {
    let request_json = serde_json::from_slice::<Value>(request_body).ok();
    let response_json = serde_json::from_slice::<Value>(response_body).ok();

    let request_kind = classify(uri, request_json.as_ref());
    let model = request_json
        .as_ref()
        .and_then(|value| value.get("model"))
        .and_then(Value::as_str)
        .map(str::to_string)
        .or_else(|| {
            response_json
                .as_ref()
                .and_then(|value| value.get("model"))
                .and_then(Value::as_str)
                .map(str::to_string)
        });

    let mut messages = Vec::new();
    if let Some(value) = request_json.as_ref() {
        collect_request_messages(&mut messages, request_kind, value);
    }
    if let Some(value) = response_json.as_ref() {
        collect_response_messages(&mut messages, value);
    } else if let Some(text) = parse_sse_text(response_body) {
        messages.push(ParsedMessage {
            role: "assistant".to_string(),
            content: text,
        });
    }

    let session_key_hint = request_json
        .as_ref()
        .and_then(|value| {
            value
                .pointer("/metadata/session_id")
                .or_else(|| value.pointer("/metadata/conversation_id"))
                .or_else(|| value.get("conversation"))
        })
        .and_then(Value::as_str)
        .map(str::to_string);

    ParsedTrace {
        request_kind,
        model,
        messages,
        session_key_hint,
    }
}

fn classify(uri: &str, request_json: Option<&Value>) -> RequestKind {
    if uri.contains("/chat/completions") {
        RequestKind::OpenAiChatCompletions
    } else if uri.contains("/responses") {
        RequestKind::OpenAiResponses
    } else if uri.contains("/messages") && request_json.and_then(|v| v.get("max_tokens")).is_some()
    {
        RequestKind::AnthropicMessages
    } else if request_json.is_some() {
        RequestKind::GenericJson
    } else {
        RequestKind::GenericHttp
    }
}

fn collect_request_messages(messages: &mut Vec<ParsedMessage>, kind: RequestKind, value: &Value) {
    if let Some(items) = value.get("messages").and_then(Value::as_array) {
        for item in items {
            let role = item
                .get("role")
                .and_then(Value::as_str)
                .unwrap_or("user")
                .to_string();
            if let Some(content) = stringify_content(item.get("content")) {
                messages.push(ParsedMessage { role, content });
            }
        }
        return;
    }

    if kind == RequestKind::OpenAiResponses
        && let Some(content) = stringify_content(value.get("input"))
    {
        messages.push(ParsedMessage {
            role: "user".to_string(),
            content,
        });
    }
}

fn collect_response_messages(messages: &mut Vec<ParsedMessage>, value: &Value) {
    if let Some(content) = value
        .pointer("/choices/0/message/content")
        .and_then(|value| stringify_content(Some(value)))
    {
        messages.push(ParsedMessage {
            role: "assistant".to_string(),
            content,
        });
        return;
    }

    if let Some(content) = value.get("output_text").and_then(Value::as_str) {
        messages.push(ParsedMessage {
            role: "assistant".to_string(),
            content: content.to_string(),
        });
        return;
    }

    if let Some(items) = value.get("content").and_then(Value::as_array) {
        let content = items
            .iter()
            .filter_map(|item| item.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("");
        if !content.is_empty() {
            messages.push(ParsedMessage {
                role: "assistant".to_string(),
                content,
            });
        }
    }
}

fn stringify_content(value: Option<&Value>) -> Option<String> {
    match value? {
        Value::String(text) => Some(text.clone()),
        Value::Array(items) => {
            let parts = items
                .iter()
                .filter_map(|item| {
                    item.get("text")
                        .or_else(|| item.get("content"))
                        .and_then(Value::as_str)
                        .map(str::to_string)
                })
                .collect::<Vec<_>>();
            if parts.is_empty() {
                serde_json::to_string(value?).ok()
            } else {
                Some(parts.join(""))
            }
        }
        other => serde_json::to_string(other).ok(),
    }
}

fn parse_sse_text(body: &[u8]) -> Option<String> {
    let text = std::str::from_utf8(body).ok()?;
    if !text.contains("data:") {
        return None;
    }

    let mut output = String::new();
    for line in text.lines() {
        let Some(data) = line.strip_prefix("data:") else {
            continue;
        };
        let data = data.trim();
        if data == "[DONE]" || data.is_empty() {
            continue;
        }
        if let Ok(value) = serde_json::from_str::<Value>(data) {
            if let Some(delta) = value
                .pointer("/choices/0/delta/content")
                .and_then(Value::as_str)
            {
                output.push_str(delta);
            } else if let Some(delta) = value.get("delta").and_then(Value::as_str) {
                output.push_str(delta);
            } else if let Some(text) = value
                .pointer("/content_block/delta/text")
                .and_then(Value::as_str)
            {
                output.push_str(text);
            }
        }
    }

    if output.is_empty() {
        None
    } else {
        Some(output)
    }
}
