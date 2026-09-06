use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::TokenUsage;
use crate::types::RequestKind;

const MAX_TOOLS: usize = 128;
const MAX_TOOL_TEXT_BYTES: usize = 16 * 1024;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: String,
}

#[derive(Debug, Clone, Default)]
pub struct ResponseDetails {
    pub usage: TokenUsage,
    pub usage_complete: bool,
    pub tool_calls: Vec<ToolCall>,
    pub tool_calls_truncated: bool,
    pub error: Option<String>,
    pub model: Option<String>,
    pub text: String,
    /// Exclusive byte offset of the first SSE event carrying generated output.
    pub first_output_offset: Option<usize>,
    pub stream_complete: Option<bool>,
}

impl ResponseDetails {
    pub fn parse(body: &[u8], json: Option<&Value>, kind: RequestKind) -> Self {
        let mut parser = ResponseParser {
            kind,
            ..ResponseParser::default()
        };
        if let Some(value) = json {
            parser.observe(value);
            parser.collect_tools(value);
            parser.terminal = true;
            parser.final_usage_seen = true;
        } else {
            // Decode one event at a time; a truncated UTF-8 tail must not discard
            // earlier complete usage, messages, or errors.
            let mut data = Vec::new();
            let mut offset = 0;
            for line in body.split_inclusive(|byte| *byte == b'\n') {
                offset += line.len();
                let line = line.strip_suffix(b"\n").unwrap_or(line);
                let line = line.strip_suffix(b"\r").unwrap_or(line);
                if line.is_empty() {
                    if data == b"[DONE]" {
                        parser.terminal = true;
                    } else if let Ok(value) = serde_json::from_slice::<Value>(&data) {
                        parser.event(&value, offset);
                    }
                    data.clear();
                } else if let Some(line) = line.strip_prefix(b"data:") {
                    if !data.is_empty() {
                        data.push(b'\n');
                    }
                    data.extend_from_slice(line.strip_prefix(b" ").unwrap_or(line));
                }
            }
            parser.details.stream_complete = Some(parser.terminal);
        }
        parser.finish()
    }
}

#[derive(Default)]
struct ResponseParser {
    kind: RequestKind,
    details: ResponseDetails,
    tools: BTreeMap<String, ToolCall>,
    texts: BTreeMap<(i64, i64), String>,
    terminal: bool,
    final_usage_seen: bool,
}

impl ResponseParser {
    fn observe(&mut self, value: &Value) {
        if let Some(model) = value.get("model").and_then(Value::as_str) {
            self.details.model = Some(model.to_string());
        }
        if let Some(error) = value.get("error").filter(|error| !error.is_null()) {
            let message = error
                .get("message")
                .and_then(Value::as_str)
                .or_else(|| error.get("type").and_then(Value::as_str))
                .unwrap_or("upstream reported an error");
            self.details.error = Some(bounded(message, &mut false));
        }
        if matches!(
            value.get("status").and_then(Value::as_str),
            Some("failed" | "incomplete" | "cancelled")
        ) {
            self.details
                .error
                .get_or_insert_with(|| format!("upstream response {}", value["status"]));
        }
        if let Some(usage) = value.get("usage").filter(|usage| usage.is_object()) {
            let tokens = &mut self.details.usage;
            update(
                &mut tokens.input_tokens,
                count(usage, "input_tokens").or_else(|| count(usage, "prompt_tokens")),
            );
            update(
                &mut tokens.output_tokens,
                count(usage, "output_tokens").or_else(|| count(usage, "completion_tokens")),
            );
            update(
                &mut tokens.cached_input_tokens,
                count(usage, "cache_read_input_tokens")
                    .or_else(|| {
                        usage
                            .pointer("/input_tokens_details/cached_tokens")
                            .and_then(nonnegative)
                    })
                    .or_else(|| {
                        usage
                            .pointer("/prompt_tokens_details/cached_tokens")
                            .and_then(nonnegative)
                    }),
            );
            update(
                &mut tokens.cache_creation_input_tokens,
                count(usage, "cache_creation_input_tokens"),
            );
        }
    }

    fn tool(&mut self, key: String, value: &Value, append: bool) {
        if !self.tools.contains_key(&key) && self.tools.len() >= MAX_TOOLS {
            self.details.tool_calls_truncated = true;
            return;
        }
        let tool = self.tools.entry(key).or_default();
        let truncated = &mut self.details.tool_calls_truncated;
        if let Some(id) = value
            .get("call_id")
            .or_else(|| value.get("id"))
            .and_then(Value::as_str)
        {
            tool.id = bounded(id, truncated);
        }
        let function = value.get("function").unwrap_or(value);
        if let Some(name) = function.get("name").and_then(Value::as_str) {
            if append {
                append_bounded(&mut tool.name, name, truncated);
            } else {
                tool.name = bounded(name, truncated);
            }
        }
        if let Some(arguments) = function.get("arguments").and_then(Value::as_str) {
            if append {
                append_bounded(&mut tool.arguments, arguments, truncated);
            } else {
                tool.arguments = bounded(arguments, truncated);
            }
        } else if let Some(input) = value
            .get("input")
            .filter(|input| input.as_object().is_none_or(|input| !input.is_empty()))
        {
            tool.arguments = bounded(&input.to_string(), truncated);
        }
    }

    fn collect_tools(&mut self, value: &Value) {
        if let Some(choices) = value.get("choices").and_then(Value::as_array) {
            for (choice_index, choice) in choices.iter().enumerate() {
                if let Some(tools) = choice
                    .pointer("/message/tool_calls")
                    .and_then(Value::as_array)
                {
                    for (index, tool) in tools.iter().enumerate() {
                        self.tool(format!("chat:{choice_index}:{index}"), tool, false);
                    }
                }
            }
        }
        if let Some(items) = value.get("output").and_then(Value::as_array) {
            for (index, item) in items.iter().enumerate() {
                if item["type"] == "function_call" {
                    self.tool(format!("response:{index}"), item, false);
                }
            }
        }
        if let Some(items) = value.get("content").and_then(Value::as_array) {
            for (index, item) in items.iter().enumerate() {
                if matches!(item["type"].as_str(), Some("tool_use" | "server_tool_use")) {
                    self.tool(format!("anthropic:{index}"), item, false);
                }
            }
        }
    }

    fn event(&mut self, value: &Value, offset: usize) {
        self.observe(value);
        let event_type = value["type"].as_str().unwrap_or("");
        if (event_type == "message_delta" && count(&value["usage"], "output_tokens").is_some())
            || (value["choices"].is_array() && value["usage"].is_object())
        {
            self.final_usage_seen = true;
        }
        let output_index = value["output_index"].as_i64().unwrap_or(0);
        let content_index = value["content_index"].as_i64().unwrap_or(0);
        let index = value["index"].as_i64().unwrap_or(0);
        let mut generated = false;
        match event_type {
            "message_start" => self.observe(&value["message"]),
            "message_stop" => self.terminal = true,
            "response.completed" | "response.failed" | "response.incomplete" => {
                self.observe(&value["response"]);
                self.collect_tools(&value["response"]);
                self.terminal = true;
                self.final_usage_seen = value["response"]["usage"].is_object();
                // Some gateways emit only the final assembled response.
                if self.texts.is_empty() {
                    let mut messages = Vec::new();
                    super::collect_response_messages(&mut messages, &value["response"]);
                    self.texts
                        .insert((0, 0), messages.into_iter().map(|m| m.content).collect());
                }
            }
            "response.created" => self.observe(&value["response"]),
            "response.output_text.delta" => {
                generated = self.text_delta((output_index, content_index), value["delta"].as_str());
            }
            "response.output_text.done" => {
                self.texts
                    .entry((output_index, content_index))
                    .or_insert_with(|| value["text"].as_str().unwrap_or("").to_string());
            }
            "response.output_item.added" | "response.output_item.done" => {
                if value["item"]["type"] == "function_call" {
                    self.tool(format!("response:{output_index}"), &value["item"], false);
                }
            }
            "response.function_call_arguments.delta" => {
                let delta = value["delta"].as_str().unwrap_or("");
                generated = !delta.is_empty();
                self.tool(
                    format!("response:{output_index}"),
                    &serde_json::json!({"arguments": delta}),
                    true,
                );
            }
            "content_block_start" => {
                let block = &value["content_block"];
                if matches!(block["type"].as_str(), Some("tool_use" | "server_tool_use")) {
                    self.tool(format!("anthropic:{index}"), block, false);
                } else {
                    generated = self.text_delta((index, 0), block["text"].as_str());
                }
            }
            "content_block_delta" => {
                let delta = &value["delta"];
                generated = self.text_delta((index, 0), delta["text"].as_str());
                if let Some(arguments) = delta["partial_json"].as_str() {
                    generated |= !arguments.is_empty();
                    self.tool(
                        format!("anthropic:{index}"),
                        &serde_json::json!({"arguments": arguments}),
                        true,
                    );
                }
                generated |= delta["thinking"]
                    .as_str()
                    .is_some_and(|text| !text.is_empty());
            }
            _ => {}
        }
        if let Some(choices) = value.get("choices").and_then(Value::as_array) {
            for choice in choices {
                let choice_index = choice["index"].as_i64().unwrap_or(0);
                let delta = &choice["delta"];
                if choice_index == 0 {
                    generated |= self.text_delta((0, 0), delta["content"].as_str());
                }
                generated |= delta["refusal"]
                    .as_str()
                    .is_some_and(|text| !text.is_empty());
                if let Some(tools) = delta["tool_calls"].as_array() {
                    for tool in tools {
                        let index = tool["index"].as_i64().unwrap_or(0);
                        generated |= tool
                            .pointer("/function/arguments")
                            .and_then(Value::as_str)
                            .is_some_and(|text| !text.is_empty());
                        self.tool(format!("chat:{choice_index}:{index}"), tool, true);
                    }
                }
            }
        }
        if generated && self.details.first_output_offset.is_none() {
            self.details.first_output_offset = Some(offset);
        }
    }

    fn text_delta(&mut self, key: (i64, i64), text: Option<&str>) -> bool {
        if let Some(text) = text.filter(|text| !text.is_empty()) {
            self.texts.entry(key).or_default().push_str(text);
            true
        } else {
            false
        }
    }

    fn finish(mut self) -> ResponseDetails {
        let usage = &mut self.details.usage;
        if self.kind == RequestKind::AnthropicMessages {
            usage.input_tokens = usage.input_tokens.and_then(|input| {
                input
                    .checked_add(usage.cached_input_tokens.unwrap_or(0))?
                    .checked_add(usage.cache_creation_input_tokens.unwrap_or(0))
            });
        }
        self.details.usage_complete = self.terminal
            && self.final_usage_seen
            && usage.input_tokens.is_some()
            && usage.output_tokens.is_some();
        self.details.tool_calls = self.tools.into_values().collect();
        self.details.text = self.texts.into_values().collect();
        self.details
    }
}

fn nonnegative(value: &Value) -> Option<i64> {
    value.as_i64().filter(|value| *value >= 0)
}
fn count(value: &Value, key: &str) -> Option<i64> {
    value.get(key).and_then(nonnegative)
}
fn update(target: &mut Option<i64>, value: Option<i64>) {
    if value.is_some() {
        *target = value;
    }
}

fn bounded(value: &str, truncated: &mut bool) -> String {
    let mut result = String::new();
    append_bounded(&mut result, value, truncated);
    result
}

fn append_bounded(target: &mut String, value: &str, truncated: &mut bool) {
    let mut end = value
        .len()
        .min(MAX_TOOL_TEXT_BYTES.saturating_sub(target.len()));
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    *truncated |= end < value.len();
    target.push_str(&value[..end]);
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn stream(events: &[Value], done: bool, kind: RequestKind) -> ResponseDetails {
        let mut body = events
            .iter()
            .map(|event| format!("data: {event}\n\n"))
            .collect::<String>();
        if done {
            body.push_str("data: [DONE]\n\n");
        }
        ResponseDetails::parse(body.as_bytes(), None, kind)
    }

    #[test]
    fn chat_final_usage_is_cumulative_and_role_events_are_not_tokens() {
        let role = json!({"choices":[{"delta":{"role":"assistant","content":""}}]});
        let text = json!({"choices":[{"delta":{"content":"hi"}}]});
        let usage = json!({"choices":[],"usage":{"prompt_tokens":100,"completion_tokens":5,"prompt_tokens_details":{"cached_tokens":80}}});
        let parsed = stream(
            &[role.clone(), text.clone(), usage.clone(), usage],
            true,
            RequestKind::OpenAiChatCompletions,
        );
        assert_eq!(
            parsed.first_output_offset,
            Some(format!("data: {role}\n\ndata: {text}\n\n").len())
        );
        assert_eq!(parsed.text, "hi");
        assert_eq!(parsed.usage.input_tokens, Some(100));
        assert_eq!(parsed.usage.output_tokens, Some(5));
        assert_eq!(parsed.usage.cached_input_tokens, Some(80));
        assert!(parsed.usage_complete);
    }

    #[test]
    fn chat_without_usage_or_with_interrupted_stream_is_unknown() {
        let empty = stream(&[], true, RequestKind::OpenAiChatCompletions);
        assert!(!empty.usage_complete);
        assert_eq!(empty.usage.input_tokens, None);
        let partial = stream(
            &[json!({"usage":{"prompt_tokens":10,"completion_tokens":1}})],
            false,
            RequestKind::OpenAiChatCompletions,
        );
        assert!(!partial.usage_complete);
    }

    #[test]
    fn anthropic_cumulative_usage_preserves_input_and_normalizes_caches() {
        let parsed = stream(
            &[
                json!({"type":"message_start","message":{"model":"claude","usage":{"input_tokens":10,"output_tokens":1,"cache_read_input_tokens":100,"cache_creation_input_tokens":20}}}),
                json!({"type":"message_delta","usage":{"output_tokens":3}}),
                json!({"type":"message_delta","usage":{"output_tokens":7}}),
                json!({"type":"message_stop"}),
            ],
            false,
            RequestKind::AnthropicMessages,
        );
        assert_eq!(parsed.usage.input_tokens, Some(130));
        assert_eq!(parsed.usage.output_tokens, Some(7));
        assert!(parsed.usage_complete);
    }

    #[test]
    fn anthropic_start_usage_without_final_usage_is_partial() {
        let parsed = stream(
            &[
                json!({"type":"message_start","message":{"usage":{"input_tokens":10,"output_tokens":1}}}),
                json!({"type":"message_stop"}),
            ],
            false,
            RequestKind::AnthropicMessages,
        );
        assert!(!parsed.usage_complete);
    }

    #[test]
    fn chat_tool_argument_fragments_are_assembled_without_counting_history() {
        let parsed = stream(
            &[
                json!({"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"call-1","function":{"name":"search","arguments":"{\"q\":"}}]}}]}),
                json!({"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":"\"rust\"}"}}]}}]}),
            ],
            true,
            RequestKind::OpenAiChatCompletions,
        );
        assert_eq!(parsed.tool_calls.len(), 1);
        assert_eq!(parsed.tool_calls[0].name, "search");
        assert_eq!(parsed.tool_calls[0].arguments, "{\"q\":\"rust\"}");
        assert!(parsed.first_output_offset.is_some());
    }

    #[test]
    fn responses_final_tool_snapshot_does_not_duplicate_deltas() {
        let tool = json!({"type":"function_call","id":"item-1","call_id":"call-1","name":"search","arguments":"{}"});
        let parsed = stream(
            &[
                json!({"type":"response.output_item.added","output_index":0,"item":{"type":"function_call","name":"search","arguments":""}}),
                json!({"type":"response.function_call_arguments.delta","output_index":0,"delta":"{}"}),
                json!({"type":"response.output_item.done","output_index":0,"item":tool}),
                json!({"type":"response.completed","response":{"output":[tool],"usage":{"input_tokens":12,"output_tokens":5}}}),
            ],
            false,
            RequestKind::OpenAiResponses,
        );
        assert_eq!(parsed.tool_calls.len(), 1);
        assert_eq!(parsed.tool_calls[0].arguments, "{}");
        assert_eq!(parsed.tool_calls[0].id, "call-1");
        assert!(parsed.usage_complete);
    }

    #[test]
    fn anthropic_tool_json_fragments_and_error_events_are_preserved() {
        let parsed = stream(
            &[
                json!({"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":"tool-1","name":"weather","input":{}}}),
                json!({"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"{\"city\":\"Paris\"}"}}),
                json!({"type":"error","error":{"type":"overloaded_error","message":"Overloaded"}}),
            ],
            false,
            RequestKind::AnthropicMessages,
        );
        assert_eq!(parsed.tool_calls[0].arguments, "{\"city\":\"Paris\"}");
        assert_eq!(parsed.error.as_deref(), Some("Overloaded"));
    }

    #[test]
    fn sse_multiline_crlf_and_invalid_utf8_tail_keep_complete_events() {
        let mut body =
            b"data: {\"choices\":\r\ndata: [{\"delta\":{\"content\":\"hello\"}}]}\r\n\r\n".to_vec();
        body.extend_from_slice(b"data: {\"delta\":\"\xff");
        let parsed = ResponseDetails::parse(&body, None, RequestKind::OpenAiChatCompletions);
        assert_eq!(parsed.text, "hello");
    }

    #[test]
    fn failed_response_is_not_a_success_even_with_http_200() {
        for status in ["failed", "incomplete", "cancelled"] {
            let parsed = ResponseDetails::parse(
                b"",
                Some(&json!({"status": status})),
                RequestKind::OpenAiResponses,
            );
            assert!(parsed.error.is_some());
        }
    }

    #[test]
    fn invalid_usage_is_not_coerced_to_zero_or_wrapped() {
        let parsed = ResponseDetails::parse(
            b"",
            Some(&json!({"usage":{"input_tokens":-1,"output_tokens":18446744073709551615_u64}})),
            RequestKind::OpenAiResponses,
        );
        assert_eq!(parsed.usage.input_tokens, None);
        assert_eq!(parsed.usage.output_tokens, None);
        assert!(!parsed.usage_complete);
    }

    #[test]
    fn tool_previews_are_bounded_on_utf8_boundaries() {
        let tools = (0..MAX_TOOLS + 1).map(|i| json!({"type":"function_call","call_id":i.to_string(),"name":"f","arguments":"é".repeat(MAX_TOOL_TEXT_BYTES)})).collect::<Vec<_>>();
        let parsed = ResponseDetails::parse(
            b"",
            Some(&json!({"output":tools})),
            RequestKind::OpenAiResponses,
        );
        assert_eq!(parsed.tool_calls.len(), MAX_TOOLS);
        assert_eq!(parsed.tool_calls[0].arguments.len(), MAX_TOOL_TEXT_BYTES);
        assert!(parsed.tool_calls_truncated);
    }
}
