use serde_json::Value;

use crate::types::ParsedMessage;
use crate::types::RequestKind;

#[derive(Debug, Clone, Default)]
pub struct ParsedTrace {
    pub request_kind: RequestKind,
    pub model: Option<String>,
    pub messages: Vec<ParsedMessage>,
    pub session_key_hint: Option<String>,
    pub response: ResponseDetails,
}

mod response;
pub use response::ResponseDetails;

pub fn parse_trace(uri: &str, request_body: &[u8], response_body: &[u8]) -> ParsedTrace {
    let request_json = serde_json::from_slice::<Value>(request_body).ok();
    let response_json = serde_json::from_slice::<Value>(response_body).ok();

    let request_kind = classify(uri, request_json.as_ref());
    let response = ResponseDetails::parse(response_body, response_json.as_ref(), request_kind);
    let model = response.model.clone().or_else(|| {
        request_json
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
            })
    });

    let mut messages = Vec::new();
    if let Some(value) = request_json.as_ref() {
        collect_request_messages(&mut messages, request_kind, value);
    }
    if let Some(value) = response_json.as_ref() {
        collect_response_messages(&mut messages, value);
    } else if !response.text.is_empty() {
        messages.push(ParsedMessage {
            content_truncated: false,
            role: "assistant".to_string(),
            content: response.text.clone(),
        });
    }
    for tool in &response.tool_calls {
        messages.push(ParsedMessage {
            content_truncated: false,
            role: "tool_call".to_string(),
            content: serde_json::to_string(tool).unwrap_or_default(),
        });
    }

    let session_key_hint = request_json
        .as_ref()
        .and_then(|value| {
            value
                .pointer("/metadata/session_id")
                .or_else(|| value.pointer("/metadata/conversation_id"))
                .or_else(|| value.pointer("/conversation/id"))
                .or_else(|| value.get("conversation"))
        })
        .and_then(Value::as_str)
        .map(str::to_string);

    ParsedTrace {
        request_kind,
        model,
        messages,
        session_key_hint,
        response,
    }
}

fn classify(uri: &str, request_json: Option<&Value>) -> RequestKind {
    let path = uri.split('?').next().unwrap_or(uri);
    if path.ends_with("/chat/completions") {
        RequestKind::OpenAiChatCompletions
    } else if path.ends_with("/responses") {
        RequestKind::OpenAiResponses
    } else if path.ends_with("/messages")
        && request_json.and_then(|v| v.get("max_tokens")).is_some()
    {
        RequestKind::AnthropicMessages
    } else if request_json.is_some() {
        RequestKind::GenericJson
    } else {
        RequestKind::GenericHttp
    }
}

fn collect_request_messages(messages: &mut Vec<ParsedMessage>, kind: RequestKind, value: &Value) {
    if let Some(content) =
        stringify_content(value.get("system").or_else(|| value.get("instructions")))
    {
        messages.push(ParsedMessage {
            content_truncated: false,
            role: "system".to_string(),
            content,
        });
    }
    if let Some(items) = value.get("messages").and_then(Value::as_array) {
        for item in items {
            let role = item
                .get("role")
                .and_then(Value::as_str)
                .unwrap_or("user")
                .to_string();
            if let Some(content) = stringify_content(item.get("content")) {
                messages.push(ParsedMessage {
                    content_truncated: false,
                    role,
                    content,
                });
            }
            if let Some(tools) = item.get("tool_calls").and_then(Value::as_array) {
                for tool in tools {
                    messages.push(ParsedMessage {
                        content_truncated: false,
                        role: "tool_call".to_string(),
                        content: tool.to_string(),
                    });
                }
            }
        }
        return;
    }

    if kind == RequestKind::OpenAiResponses
        && let Some(input) = value.get("input")
    {
        for message in responses_input_messages(input) {
            messages.push(message);
        }
    }
}

fn collect_response_messages(messages: &mut Vec<ParsedMessage>, value: &Value) {
    if let Some(content) = value
        .pointer("/choices/0/message/content")
        .and_then(|value| stringify_content(Some(value)))
    {
        messages.push(ParsedMessage {
            content_truncated: false,
            role: "assistant".to_string(),
            content,
        });
        return;
    }

    if let Some(content) = value.get("output_text").and_then(Value::as_str) {
        messages.push(ParsedMessage {
            content_truncated: false,
            role: "assistant".to_string(),
            content: content.to_string(),
        });
        return;
    }

    // OpenAI Responses API: the assistant text lives under /output[].content[]
    // as {"type": "output_text", "text": "..."}. Non-text output items are skipped.
    if let Some(items) = value.get("output").and_then(Value::as_array) {
        let content = items
            .iter()
            .filter_map(|item| item.get("content").and_then(Value::as_array))
            .flatten()
            .filter_map(|part| {
                let kind = part.get("type").and_then(Value::as_str);
                if kind.is_some_and(|k| k != "output_text" && k != "text") {
                    return None;
                }
                part.get("text").and_then(Value::as_str).map(str::to_string)
            })
            .collect::<Vec<_>>()
            .join("");
        if !content.is_empty() {
            messages.push(ParsedMessage {
                content_truncated: false,
                role: "assistant".to_string(),
                content,
            });
        }
        return;
    }

    // Anthropic Messages API: assistant text is under /content[] as
    // {"type": "text", "text": "..."}.
    if let Some(items) = value.get("content").and_then(Value::as_array) {
        let content = items
            .iter()
            .filter_map(|item| item.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("");
        if !content.is_empty() {
            messages.push(ParsedMessage {
                content_truncated: false,
                role: "assistant".to_string(),
                content,
            });
        }
    }
}

// The OpenAI Responses API accepts `input` as a plain string, a single input
// item, or an array of input items. Each item may itself be a string, a message
// object with a `role` and `content`, or a structured output item. We extract
// whatever human-readable text we can from each shape.
fn responses_input_messages(input: &Value) -> Vec<ParsedMessage> {
    let mut messages = Vec::new();
    let items: Vec<&Value> = match input {
        Value::Array(items) => items.iter().collect(),
        Value::String(_) | Value::Object(_) => vec![input],
        _ => return messages,
    };

    for item in items {
        match item {
            Value::String(text) => messages.push(ParsedMessage {
                content_truncated: false,
                role: "user".to_string(),
                content: text.clone(),
            }),
            Value::Object(_) => {
                if matches!(
                    item.get("type").and_then(Value::as_str),
                    Some("function_call" | "function_call_output")
                ) {
                    messages.push(ParsedMessage {
                        content_truncated: false,
                        role: if item["type"] == "function_call" {
                            "tool_call"
                        } else {
                            "tool"
                        }
                        .to_string(),
                        content: item.to_string(),
                    });
                    continue;
                }
                let role = item
                    .get("role")
                    .and_then(Value::as_str)
                    .unwrap_or("user")
                    .to_string();
                if let Some(content) = stringify_content(item.get("content")) {
                    if !content.is_empty() {
                        messages.push(ParsedMessage {
                            content_truncated: false,
                            role,
                            content,
                        });
                    }
                } else if let Some(text) = item.get("text").and_then(Value::as_str) {
                    // Structured input items such as {"type":"input_text","text":...}.
                    if !text.is_empty() {
                        messages.push(ParsedMessage {
                            content_truncated: false,
                            role,
                            content: text.to_string(),
                        });
                    }
                }
            }
            _ => {}
        }
    }

    messages
}

fn stringify_content(value: Option<&Value>) -> Option<String> {
    match value? {
        Value::Null => None,
        Value::String(text) => Some(text.clone()),
        Value::Array(items) => {
            if items.iter().any(|item| {
                matches!(
                    item.get("type").and_then(Value::as_str),
                    Some("tool_use" | "tool_result")
                )
            }) {
                return serde_json::to_string(items).ok();
            }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::RequestKind;

    fn messages(parsed: &ParsedTrace) -> Vec<(String, String)> {
        parsed
            .messages
            .iter()
            .map(|message| (message.role.clone(), message.content.clone()))
            .collect()
    }

    #[test]
    fn chat_completions_non_stream_records_user_and_assistant() {
        let request = br#"{"model":"gpt-4o-mini","messages":[{"role":"user","content":"hi"}]}"#;
        let response = br#"{"choices":[{"message":{"role":"assistant","content":"hello"}}]}"#;
        let parsed = parse_trace("/v1/chat/completions", request, response);
        assert_eq!(parsed.request_kind, RequestKind::OpenAiChatCompletions);
        assert_eq!(
            messages(&parsed),
            vec![
                ("user".to_string(), "hi".to_string()),
                ("assistant".to_string(), "hello".to_string()),
            ]
        );
    }

    #[test]
    fn chat_completions_stream_assembles_delta_content() {
        let request =
            br#"{"model":"gpt-4o-mini","messages":[{"role":"user","content":"hi"}],"stream":true}"#;
        let response = b"data: {\"choices\":[{\"delta\":{\"content\":\"hel\"}}]}\n\ndata: {\"choices\":[{\"delta\":{\"content\":\"lo\"}}]}\n\ndata: [DONE]\n\n";
        let parsed = parse_trace("/v1/chat/completions", request, response);
        assert_eq!(
            messages(&parsed),
            vec![
                ("user".to_string(), "hi".to_string()),
                ("assistant".to_string(), "hello".to_string()),
            ]
        );
    }

    #[test]
    fn responses_non_stream_records_string_input_and_output_text() {
        let request = br#"{"model":"gpt-4o-mini","input":"hi","max_output_tokens":10}"#;
        let response = br#"{"output":[{"type":"message","role":"assistant","content":[{"type":"output_text","text":"hello"}]}]}"#;
        let parsed = parse_trace("/v1/responses", request, response);
        assert_eq!(parsed.request_kind, RequestKind::OpenAiResponses);
        assert_eq!(
            messages(&parsed),
            vec![
                ("user".to_string(), "hi".to_string()),
                ("assistant".to_string(), "hello".to_string()),
            ]
        );
    }

    #[test]
    fn responses_non_stream_records_structured_input_items() {
        let request = br#"{"model":"gpt-4o-mini","input":[{"role":"user","content":[{"type":"input_text","text":"structured hi"}]}]}"#;
        let response = br#"{"output_text":"structured hello"}"#;
        let parsed = parse_trace("/v1/responses", request, response);
        assert_eq!(
            messages(&parsed),
            vec![
                ("user".to_string(), "structured hi".to_string()),
                ("assistant".to_string(), "structured hello".to_string()),
            ]
        );
    }

    #[test]
    fn responses_stream_assembles_output_text_deltas() {
        let request = br#"{"model":"gpt-4o-mini","input":"hi","stream":true}"#;
        let response = b"data: {\"type\":\"response.output_text.delta\",\"delta\":\"hel\"}\n\ndata: {\"type\":\"response.output_text.delta\",\"delta\":\"lo\"}\n\ndata: {\"type\":\"response.output_text.done\",\"text\":\"hello\"}\n\ndata: [DONE]\n\n";
        let parsed = parse_trace("/v1/responses", request, response);
        assert_eq!(
            messages(&parsed),
            vec![
                ("user".to_string(), "hi".to_string()),
                ("assistant".to_string(), "hello".to_string()),
            ]
        );
    }

    #[test]
    fn anthropic_messages_non_stream_records_user_and_assistant() {
        let request = br#"{"model":"claude-3-haiku","max_tokens":10,"messages":[{"role":"user","content":"hi"}]}"#;
        let response = br#"{"content":[{"type":"text","text":"hello"}]}"#;
        let parsed = parse_trace("/v1/messages", request, response);
        assert_eq!(parsed.request_kind, RequestKind::AnthropicMessages);
        assert_eq!(
            messages(&parsed),
            vec![
                ("user".to_string(), "hi".to_string()),
                ("assistant".to_string(), "hello".to_string()),
            ]
        );
    }

    #[test]
    fn anthropic_messages_stream_assembles_content_block_deltas() {
        let request = br#"{"model":"claude-3-haiku","max_tokens":10,"messages":[{"role":"user","content":"hi"}],"stream":true}"#;
        let response = b"event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"delta\":{\"type\":\"text_delta\",\"text\":\"hel\"}}\n\nevent: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"delta\":{\"type\":\"text_delta\",\"text\":\"lo\"}}\n\ndata: [DONE]\n\n";
        let parsed = parse_trace("/v1/messages", request, response);
        assert_eq!(
            messages(&parsed),
            vec![
                ("user".to_string(), "hi".to_string()),
                ("assistant".to_string(), "hello".to_string()),
            ]
        );
    }

    #[test]
    fn anthropic_messages_without_max_tokens_is_not_misclassified() {
        // A `/messages` request without `max_tokens` should fall back to generic
        // JSON rather than being mislabeled as Anthropic messages.
        let request = br#"{"model":"x","foo":"bar"}"#;
        let response = b"{}";
        let parsed = parse_trace("/v1/messages", request, response);
        assert_eq!(parsed.request_kind, RequestKind::GenericJson);
    }

    #[test]
    fn chat_completions_multimodal_content_array_is_stringified() {
        let request = br#"{"model":"gpt-4o-mini","messages":[{"role":"user","content":[{"type":"text","text":"describe this"},{"type":"image_url","image_url":{"url":"data:..."}}]}]}"#;
        let response = b"{}";
        let parsed = parse_trace("/v1/chat/completions", request, response);
        assert_eq!(
            messages(&parsed),
            vec![("user".to_string(), "describe this".to_string())]
        );
    }
}
