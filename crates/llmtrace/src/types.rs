use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum RequestKind {
    #[serde(rename = "openai_chat_completions")]
    OpenAiChatCompletions,
    #[serde(rename = "openai_responses")]
    OpenAiResponses,
    #[serde(rename = "anthropic_messages")]
    AnthropicMessages,
    #[serde(rename = "websocket")]
    WebSocket,
    #[serde(rename = "generic_json")]
    GenericJson,
    #[serde(rename = "generic_http")]
    #[default]
    GenericHttp,
}

impl RequestKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::OpenAiChatCompletions => "openai_chat_completions",
            Self::OpenAiResponses => "openai_responses",
            Self::AnthropicMessages => "anthropic_messages",
            Self::WebSocket => "websocket",
            Self::GenericJson => "generic_json",
            Self::GenericHttp => "generic_http",
        }
    }
}

impl fmt::Display for RequestKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PluginHook {
    #[serde(rename = "on_request_start")]
    RequestStart,
    #[serde(rename = "on_response_headers")]
    ResponseHeaders,
    #[serde(rename = "on_response_end")]
    ResponseEnd,
}

impl PluginHook {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::RequestStart => "on_request_start",
            Self::ResponseHeaders => "on_response_headers",
            Self::ResponseEnd => "on_response_end",
        }
    }
}

impl fmt::Display for PluginHook {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum LoginMethod {
    #[serde(rename = "local")]
    Local,
    #[serde(rename = "oauth")]
    OAuth,
}

impl LoginMethod {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Local => "local",
            Self::OAuth => "oauth",
        }
    }
}

impl FromStr for LoginMethod {
    type Err = anyhow::Error;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "local" => Ok(Self::Local),
            "oauth" => Ok(Self::OAuth),
            other => anyhow::bail!("unknown login method {other}"),
        }
    }
}

impl fmt::Display for LoginMethod {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ParsedMessage {
    #[serde(default)]
    pub content_truncated: bool,
    pub role: String,
    pub content: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TokenUsage {
    /// Total input, including cache reads and cache writes for all providers.
    pub input_tokens: Option<i64>,
    pub output_tokens: Option<i64>,
    pub cached_input_tokens: Option<i64>,
    pub cache_creation_input_tokens: Option<i64>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: String,
}
