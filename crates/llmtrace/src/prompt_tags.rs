//! Extract configured bracketed tags from decoded, request-side text fields.
use std::collections::BTreeSet;
use std::sync::atomic::{AtomicBool, Ordering};

use serde::{Deserialize, Serialize};
use serde_json::Value;

const MAX_TAG_NAME_BYTES: usize = 32;

fn tag_name_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-')
}

pub(crate) fn validate_tag_names(names: &mut Vec<String>) -> Result<(), &'static str> {
    if names.len() > 32 {
        return Err("Choose at most 32 prompt tags");
    }
    for name in names.iter() {
        if name.len() > MAX_TAG_NAME_BYTES
            || !name.as_bytes().first().is_some_and(u8::is_ascii_alphabetic)
            || !name.bytes().all(tag_name_byte)
        {
            return Err(
                "Tag names must start with a letter and contain 1–32 ASCII letters, digits, underscores or hyphens",
            );
        }
    }
    names.sort_unstable();
    names.dedup();
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "i32", into = "i32")]
pub(crate) enum NoteIndexVersion {
    V1,
}

impl TryFrom<i32> for NoteIndexVersion {
    type Error = &'static str;
    fn try_from(value: i32) -> Result<Self, Self::Error> {
        match value {
            1 => Ok(Self::V1),
            _ => Err("unsupported prompt tag index version"),
        }
    }
}

impl From<NoteIndexVersion> for i32 {
    fn from(_: NoteIndexVersion) -> Self {
        1
    }
}

/// Coordinates in the decoded UTF-8 field, never offsets in raw JSON bytes.
pub(crate) struct DecodedTextSpan {
    start: usize,
    end: usize,
}

impl DecodedTextSpan {
    pub(crate) fn try_new(text: &str, start: usize, end: usize) -> Result<Self, &'static str> {
        if start > end || !text.is_char_boundary(start) || !text.is_char_boundary(end) {
            return Err("invalid decoded text span");
        }
        Ok(Self { start, end })
    }
    pub(crate) fn start(&self) -> usize {
        self.start
    }
    pub(crate) fn end(&self) -> usize {
        self.end
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ExtractionState {
    Ready,
    Partial,
    Unsupported,
    Failed,
}

impl ExtractionState {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Ready => "ready",
            Self::Partial => "partial",
            Self::Unsupported => "unsupported",
            Self::Failed => "failed",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum NoteDiagnostic {
    CaptureTruncated,
    BodyBudget,
    ScanBudget,
    FieldBudget,
    ValueBudget,
    CountBudget,
    TotalValueBudget,
    InvalidJson,
    InvalidUtf8,
    MissingRequestContentType,
    UnsupportedContentType,
    UnsupportedEncoding,
    UnsupportedProtocol,
    UnknownTextShape,
    UnknownRole,
    NestedMarker,
    UnclosedMarker,
    NulValue,
    ExternalHistory,
    Cancelled,
    MissingSource,
    UnreadableSource,
    RetryExhausted,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum NoteSourceKind {
    Message,
    System,
    Instructions,
    ToolResult,
    Prompt,
}

impl NoteSourceKind {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Message => "message",
            Self::System => "system",
            Self::Instructions => "instructions",
            Self::ToolResult => "tool_result",
            Self::Prompt => "prompt",
        }
    }
}

pub(crate) struct ExtractedTag {
    pub(crate) tag_name: String,
    pub(crate) value: String,
    pub(crate) source_path: String,
    pub(crate) source_kind: NoteSourceKind,
    pub(crate) role: String,
    pub(crate) field_ordinal: i32,
    pub(crate) match_ordinal: i32,
    pub(crate) span: DecodedTextSpan,
}

pub(crate) struct TagExtraction {
    pub(crate) state: ExtractionState,
    pub(crate) capture_complete: bool,
    pub(crate) external_history: bool,
    pub(crate) diagnostics: BTreeSet<NoteDiagnostic>,
    pub(crate) notes: Vec<ExtractedTag>,
}

impl TagExtraction {
    pub(crate) fn empty(capture_complete: bool) -> Self {
        Self {
            state: ExtractionState::Ready,
            capture_complete,
            external_history: false,
            diagnostics: BTreeSet::new(),
            notes: Vec::new(),
        }
    }

    pub(crate) fn unavailable(reason: NoteDiagnostic, capture_complete: bool) -> Self {
        Self {
            state: ExtractionState::Failed,
            capture_complete,
            external_history: false,
            diagnostics: BTreeSet::from([reason]),
            notes: Vec::new(),
        }
    }
}

#[derive(Clone, Copy)]
pub(crate) struct ExtractionLimits {
    max_body_bytes: usize,
    max_scan_bytes: usize,
    max_fields: usize,
    max_value_bytes: usize,
    max_notes: usize,
    max_total_value_bytes: usize,
}

impl ExtractionLimits {
    /// Validates hard memory/scan bounds before any body is parsed. A single
    /// prompt tag must fit the total value budget; zero never means unlimited.
    pub(crate) fn try_new(
        max_body_bytes: usize,
        max_scan_bytes: usize,
        max_fields: usize,
        max_value_bytes: usize,
        max_notes: usize,
        max_total_value_bytes: usize,
    ) -> anyhow::Result<Self> {
        for (name, value, maximum) in [
            ("max_body_bytes", max_body_bytes, 32 * 1024 * 1024),
            ("max_scan_bytes", max_scan_bytes, 32 * 1024 * 1024),
            ("max_fields", max_fields, 32_768),
            ("max_value_bytes", max_value_bytes, 4096),
            ("max_notes", max_notes, 256),
            ("max_total_value_bytes", max_total_value_bytes, 256 * 1024),
        ] {
            anyhow::ensure!(
                (1..=maximum).contains(&value),
                "prompt tag {name} must be 1..={maximum}"
            );
        }
        anyhow::ensure!(
            max_value_bytes <= max_total_value_bytes,
            "prompt tag single-value budget exceeds total budget"
        );
        Ok(Self {
            max_body_bytes,
            max_scan_bytes,
            max_fields,
            max_value_bytes,
            max_notes,
            max_total_value_bytes,
        })
    }

    pub(crate) fn max_body_bytes(self) -> usize {
        self.max_body_bytes
    }
    pub(crate) fn max_scan_bytes(self) -> usize {
        self.max_scan_bytes
    }
    pub(crate) fn max_fields(self) -> usize {
        self.max_fields
    }
    pub(crate) fn max_value_bytes(self) -> usize {
        self.max_value_bytes
    }
    pub(crate) fn max_notes(self) -> usize {
        self.max_notes
    }
    pub(crate) fn max_total_value_bytes(self) -> usize {
        self.max_total_value_bytes
    }
}

impl Default for ExtractionLimits {
    fn default() -> Self {
        Self {
            max_body_bytes: 8 * 1024 * 1024,
            max_scan_bytes: 8 * 1024 * 1024,
            max_fields: 8192,
            max_value_bytes: 4096,
            max_notes: 256,
            max_total_value_bytes: 256 * 1024,
        }
    }
}

pub(crate) struct RequestPromptMetadata<'a> {
    pub(crate) request_kind: &'a str,
    pub(crate) original_uri: &'a str,
    pub(crate) request_headers: &'a Value,
    pub(crate) capture_complete: bool,
}

enum Protocol {
    Chat,
    Responses,
    Anthropic,
    Completions,
}

struct Scanner<'a> {
    tags: &'a [String],
    limits: ExtractionLimits,
    stop: &'a AtomicBool,
    result: TagExtraction,
    scanned_bytes: usize,
    fields: usize,
    value_bytes: usize,
    exhausted: bool,
}

/// Owns only prompt tag values and locations in its result. The caller may move one
/// body into a blocking task and borrow it here without cloning JSON/body data.
pub(crate) fn extract(
    body: &[u8],
    metadata: RequestPromptMetadata<'_>,
    tags: &[String],
    limits: ExtractionLimits,
    stop: &AtomicBool,
) -> TagExtraction {
    let mut scanner = Scanner {
        tags,
        limits,
        stop,
        result: TagExtraction::empty(metadata.capture_complete),
        scanned_bytes: 0,
        fields: 0,
        value_bytes: 0,
        exhausted: false,
    };
    if scanner.stopped() {
        return scanner.result;
    }
    if !metadata.capture_complete {
        scanner.partial(NoteDiagnostic::CaptureTruncated);
    }
    let encoding = header(metadata.request_headers, "content-encoding");
    let encoding_present = metadata.request_headers.as_object().is_some_and(|headers| {
        headers
            .keys()
            .any(|key| key.eq_ignore_ascii_case("content-encoding"))
    });
    if (encoding_present && encoding.is_none())
        || encoding.is_some_and(|value| !value.eq_ignore_ascii_case("identity"))
    {
        return scanner.unsupported(NoteDiagnostic::UnsupportedEncoding);
    }
    let Some(content_type) = header(metadata.request_headers, "content-type") else {
        return scanner.unsupported(NoteDiagnostic::MissingRequestContentType);
    };
    let content_type = content_type
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    if content_type != "application/json" && !content_type.ends_with("+json") {
        return scanner.unsupported(NoteDiagnostic::UnsupportedContentType);
    }
    let path = metadata.original_uri.split('?').next().unwrap_or("");
    let protocol = match (metadata.request_kind, path) {
        ("websocket", _) => None,
        ("openai_chat_completions", _) => Some(Protocol::Chat),
        ("openai_responses", _) => Some(Protocol::Responses),
        ("anthropic_messages", _) => Some(Protocol::Anthropic),
        (_, path) if path.ends_with("/chat/completions") => Some(Protocol::Chat),
        (_, path) if path.ends_with("/responses") => Some(Protocol::Responses),
        (_, path) if path.ends_with("/messages") => Some(Protocol::Anthropic),
        (_, path) if path.ends_with("/completions") => Some(Protocol::Completions),
        _ => None,
    };
    let Some(protocol) = protocol else {
        return scanner.unsupported(NoteDiagnostic::UnsupportedProtocol);
    };
    if body.len() > limits.max_body_bytes {
        scanner.partial(NoteDiagnostic::BodyBudget);
        return scanner.result;
    }
    if std::str::from_utf8(body).is_err() {
        return scanner.unsupported(NoteDiagnostic::InvalidUtf8);
    }
    let value: Value = match serde_json::from_slice(body) {
        Ok(value) => value,
        Err(_) => {
            scanner.partial(NoteDiagnostic::InvalidJson);
            return scanner.result;
        }
    };
    if !value.is_object() {
        scanner.partial(NoteDiagnostic::UnknownTextShape);
        return scanner.result;
    }
    match protocol {
        Protocol::Chat => scanner.messages(value.get("messages"), "/messages", false),
        Protocol::Anthropic => {
            if metadata.request_kind != "anthropic_messages" && value.get("max_tokens").is_none() {
                return scanner.unsupported(NoteDiagnostic::UnsupportedProtocol);
            }
            if let Some(system) = value.get("system") {
                scanner.content(system, "/system", "system", NoteSourceKind::System, true);
            }
            scanner.messages(value.get("messages"), "/messages", true);
        }
        Protocol::Responses => scanner.responses(&value),
        Protocol::Completions => scanner.prompt(value.get("prompt")),
    }
    scanner.result
}

fn header<'a>(headers: &'a Value, name: &str) -> Option<&'a str> {
    let object = headers.as_object()?;
    let value = object
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case(name))?
        .1;
    value.as_str().or_else(|| {
        value
            .as_array()
            .filter(|items| items.len() == 1)?
            .first()?
            .as_str()
    })
}

impl Scanner<'_> {
    fn partial(&mut self, reason: NoteDiagnostic) {
        self.result.state = ExtractionState::Partial;
        self.result.diagnostics.insert(reason);
    }
    fn unsupported(mut self, reason: NoteDiagnostic) -> TagExtraction {
        self.result.state = ExtractionState::Unsupported;
        self.result.diagnostics.insert(reason);
        self.result
    }
    fn stopped(&mut self) -> bool {
        if self.stop.load(Ordering::Relaxed) {
            self.partial(NoteDiagnostic::Cancelled);
            self.exhausted = true;
        }
        self.exhausted
    }
    fn text(&mut self, text: &str, path: &str, role: &str, source_kind: NoteSourceKind) {
        if self.stopped() {
            return;
        }
        if self.fields >= self.limits.max_fields {
            self.partial(NoteDiagnostic::FieldBudget);
            self.exhausted = true;
            return;
        }
        let field_ordinal = self.fields as i32;
        self.fields += 1;
        if text.len()
            > self
                .limits
                .max_scan_bytes
                .saturating_sub(self.scanned_bytes)
        {
            self.partial(NoteDiagnostic::ScanBudget);
            self.exhausted = true;
            return;
        }
        self.scanned_bytes += text.len();
        let mut offset = 0;
        let mut match_ordinal = 0;
        while let Some(relative) = text[offset..].find('[') {
            if self.stopped() {
                break;
            }
            let start = offset + relative;
            offset = start + 1;
            let rest = &text[offset..];
            let name_len = rest
                .bytes()
                .take(MAX_TAG_NAME_BYTES)
                .take_while(|b| tag_name_byte(*b))
                .count();
            if rest.as_bytes().get(name_len) != Some(&b':') {
                continue;
            }
            let name = &rest[..name_len];
            if !self.tags.iter().any(|tag| tag == name) {
                continue;
            }
            let value_start = offset + name_len + 1;
            let Some(close) = text[value_start..].find(']') else {
                self.partial(NoteDiagnostic::UnclosedMarker);
                break;
            };
            let end = value_start + close;
            offset = end + 1;
            let raw_value = &text[value_start..end];
            let ordinal = match_ordinal;
            match_ordinal += 1;
            if raw_value.contains('[') {
                self.partial(NoteDiagnostic::NestedMarker);
                continue;
            }
            let value = raw_value.trim_matches(|ch: char| ch.is_ascii_whitespace());
            if value.contains('\0') {
                self.partial(NoteDiagnostic::NulValue);
                continue;
            }
            if value.len() > self.limits.max_value_bytes {
                self.partial(NoteDiagnostic::ValueBudget);
                continue;
            }
            if self.result.notes.len() >= self.limits.max_notes {
                self.partial(NoteDiagnostic::CountBudget);
                self.exhausted = true;
                break;
            }
            if value.len()
                > self
                    .limits
                    .max_total_value_bytes
                    .saturating_sub(self.value_bytes)
            {
                self.partial(NoteDiagnostic::TotalValueBudget);
                self.exhausted = true;
                break;
            }
            let Ok(span) = DecodedTextSpan::try_new(text, start, offset) else {
                self.partial(NoteDiagnostic::UnknownTextShape);
                continue;
            };
            self.value_bytes += value.len();
            self.result.notes.push(ExtractedTag {
                tag_name: name.to_owned(),
                value: value.to_owned(),
                source_path: path.to_owned(),
                source_kind,
                role: role.to_owned(),
                field_ordinal,
                match_ordinal: ordinal,
                span,
            });
        }
    }
    fn messages(&mut self, messages: Option<&Value>, path: &str, anthropic: bool) {
        let Some(messages) = messages.and_then(Value::as_array) else {
            self.partial(NoteDiagnostic::UnknownTextShape);
            return;
        };
        for (index, message) in messages.iter().enumerate() {
            if self.stopped() {
                break;
            }
            let Some(role @ ("system" | "developer" | "user" | "assistant" | "tool" | "function")) =
                message.get("role").and_then(Value::as_str)
            else {
                self.partial(NoteDiagnostic::UnknownRole);
                continue;
            };
            match message.get("content") {
                Some(content) => self.content(
                    content,
                    &format!("{path}/{index}/content"),
                    role,
                    NoteSourceKind::Message,
                    anthropic,
                ),
                None if message.get("tool_calls").is_some()
                    || message.get("function_call").is_some() => {}
                None => self.partial(NoteDiagnostic::UnknownTextShape),
            }
        }
    }
    fn content(
        &mut self,
        value: &Value,
        path: &str,
        role: &str,
        kind: NoteSourceKind,
        anthropic: bool,
    ) {
        if self.stopped() {
            return;
        }
        match value {
            Value::String(text) => self.text(text, path, role, kind),
            Value::Null => {}
            Value::Array(blocks) => {
                for (index, block) in blocks.iter().enumerate() {
                    if self.stopped() {
                        break;
                    }
                    let block_path = format!("{path}/{index}");
                    match block.get("type").and_then(Value::as_str) {
                        Some("text" | "input_text" | "output_text") => {
                            match block.get("text").and_then(Value::as_str) {
                                Some(text) => {
                                    self.text(text, &format!("{block_path}/text"), role, kind)
                                }
                                None => self.partial(NoteDiagnostic::UnknownTextShape),
                            }
                        }
                        Some("tool_result") if anthropic => match block.get("content") {
                            Some(content) => self.content(
                                content,
                                &format!("{block_path}/content"),
                                "tool",
                                NoteSourceKind::ToolResult,
                                false,
                            ),
                            None => self.partial(NoteDiagnostic::UnknownTextShape),
                        },
                        // Known non-text modalities and tool arguments stay in raw storage.
                        Some(
                            "image" | "image_url" | "input_image" | "input_audio" | "audio"
                            | "file" | "input_file" | "tool_use" | "thinking" | "redacted_thinking"
                            | "refusal",
                        ) => {}
                        _ => self.partial(NoteDiagnostic::UnknownTextShape),
                    }
                }
            }
            _ => self.partial(NoteDiagnostic::UnknownTextShape),
        }
    }
    fn responses(&mut self, value: &Value) {
        if value
            .get("previous_response_id")
            .is_some_and(|value| !value.is_null())
            || value
                .get("conversation")
                .is_some_and(|value| !value.is_null())
        {
            self.result.external_history = true;
            self.result
                .diagnostics
                .insert(NoteDiagnostic::ExternalHistory);
        }
        if let Some(instructions) = value.get("instructions") {
            match instructions {
                Value::String(text) => self.text(
                    text,
                    "/instructions",
                    "system",
                    NoteSourceKind::Instructions,
                ),
                Value::Null => {}
                _ => self.partial(NoteDiagnostic::UnknownTextShape),
            }
        }
        match value.get("input") {
            Some(Value::String(text)) => self.text(text, "/input", "user", NoteSourceKind::Message),
            Some(Value::Array(items)) => {
                for (index, item) in items.iter().enumerate() {
                    if self.stopped() {
                        break;
                    }
                    self.response_item(item, &format!("/input/{index}"));
                }
            }
            Some(Value::Object(_)) => self.response_item(&value["input"], "/input"),
            _ => self.partial(NoteDiagnostic::UnknownTextShape),
        }
    }
    fn response_item(&mut self, item: &Value, path: &str) {
        match item.get("type").and_then(Value::as_str) {
            Some("function_call_output") => match item.get("output") {
                Some(Value::String(text)) => self.text(
                    text,
                    &format!("{path}/output"),
                    "tool",
                    NoteSourceKind::ToolResult,
                ),
                Some(Value::Array(_)) => self.content(
                    &item["output"],
                    &format!("{path}/output"),
                    "tool",
                    NoteSourceKind::ToolResult,
                    false,
                ),
                _ => self.partial(NoteDiagnostic::UnknownTextShape),
            },
            Some("input_text" | "output_text") => match item.get("text").and_then(Value::as_str) {
                Some(text) => self.text(
                    text,
                    &format!("{path}/text"),
                    if item.get("type").and_then(Value::as_str) == Some("output_text") {
                        "assistant"
                    } else {
                        "user"
                    },
                    NoteSourceKind::Message,
                ),
                None => self.partial(NoteDiagnostic::UnknownTextShape),
            },
            Some("message") | None if item.get("role").is_some() => {
                let Some(
                    role @ ("system" | "developer" | "user" | "assistant" | "tool" | "function"),
                ) = item.get("role").and_then(Value::as_str)
                else {
                    self.partial(NoteDiagnostic::UnknownRole);
                    return;
                };
                match item.get("content") {
                    Some(content) => self.content(
                        content,
                        &format!("{path}/content"),
                        role,
                        NoteSourceKind::Message,
                        false,
                    ),
                    None => self.partial(NoteDiagnostic::UnknownTextShape),
                }
            }
            Some("item_reference") => {
                self.result.external_history = true;
                self.result
                    .diagnostics
                    .insert(NoteDiagnostic::ExternalHistory);
            }
            Some(
                "function_call" | "reasoning" | "computer_call" | "web_search_call"
                | "file_search_call",
            ) => {}
            _ => self.partial(NoteDiagnostic::UnknownTextShape),
        }
    }
    fn prompt(&mut self, value: Option<&Value>) {
        match value {
            Some(Value::String(text)) => self.text(text, "/prompt", "user", NoteSourceKind::Prompt),
            Some(Value::Array(items)) => {
                for (index, item) in items.iter().enumerate() {
                    if self.stopped() {
                        break;
                    }
                    match item.as_str() {
                        Some(text) => self.text(
                            text,
                            &format!("/prompt/{index}"),
                            "user",
                            NoteSourceKind::Prompt,
                        ),
                        None => self.partial(NoteDiagnostic::UnknownTextShape),
                    }
                }
            }
            _ => self.partial(NoteDiagnostic::UnknownTextShape),
        }
    }
}

#[cfg(test)]
mod tests;
