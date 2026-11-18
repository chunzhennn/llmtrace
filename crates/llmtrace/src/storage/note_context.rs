//! On-demand, authenticated prompt tag context from the shared request body reader.
use std::sync::atomic::AtomicBool;

use serde::Serialize;
use serde_json::Value;
use sqlx::{PgPool, Row};
use uuid::Uuid;

use crate::config::ArchiveConfig;
use crate::prompt_tags::{self, NoteIndexVersion, RequestPromptMetadata};
use crate::storage::bodies;
use crate::storage::models::{BodyStatus, REQUEST_METADATA_COLUMNS, RequestMetadata};

#[derive(Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub(crate) enum NoteContext {
    Available {
        before: String,
        matched: String,
        after: String,
        truncated: bool,
    },
    Missing,
    Unreadable,
    OverBudget,
    UnsupportedVersion,
}

/// Does no body I/O until explicitly called. Missing/corrupt bodies remain
/// distinct from an empty text field, and UTF-8 byte spans never reach JS.
pub(crate) async fn get_note_context(
    pool: &PgPool,
    archive: &ArchiveConfig,
    options: &super::note_index::NoteIndexOptions,
    id: Uuid,
) -> anyhow::Result<Option<NoteContext>> {
    let mut tx = super::begin_api_snapshot_tx(pool).await?;
    let row = sqlx::query("SELECT request_id, index_version, field_ordinal, match_ordinal, source_path, decoded_start_byte, decoded_end_byte, tag_value, tag_name, cfg.tag_names
        FROM trace_prompt_tags CROSS JOIN note_index_settings cfg WHERE id = $1 AND cfg.singleton")
        .bind(id).fetch_optional(&mut *tx).await?;
    let Some(row) = row else {
        return Ok(None);
    };
    if NoteIndexVersion::try_from(row.try_get::<i32, _>("index_version")?).is_err() {
        return Ok(Some(NoteContext::UnsupportedVersion));
    }
    let source_path: String = row.try_get("source_path")?;
    let expected_value: String = row.try_get("tag_value")?;
    let expected_tag: String = row.try_get("tag_name")?;
    let tags: Vec<String> = row.try_get("tag_names")?;
    let field_ordinal: i32 = row.try_get("field_ordinal")?;
    let match_ordinal: i32 = row.try_get("match_ordinal")?;
    let start: usize = row.try_get::<i32, _>("decoded_start_byte")?.try_into()?;
    let end: usize = row.try_get::<i32, _>("decoded_end_byte")?.try_into()?;
    let request_id: Uuid = row.try_get("request_id")?;
    let query = format!("SELECT {REQUEST_METADATA_COLUMNS} FROM request_traces WHERE id = $1");
    let metadata = sqlx::query_as::<_, RequestMetadata>(&query)
        .bind(request_id)
        .fetch_one(&mut *tx)
        .await?;
    let limits = options.extraction_limits()?;
    if usize::try_from(metadata.request_body_bytes)? > limits.max_body_bytes() {
        return Ok(Some(NoteContext::OverBudget));
    }
    let body =
        match bodies::load_request_body(&mut tx, archive, &metadata, options.max_segment_bytes)
            .await
        {
            Ok(body) => body,
            Err(error) if error.is::<bodies::BodyReadBudgetExceeded>() => {
                return Ok(Some(NoteContext::OverBudget));
            }
            Err(error) => return Err(error),
        };
    tx.commit().await?;
    let Some(bytes) = body.bytes else {
        return Ok(Some(match body.status {
            BodyStatus::Missing => NoteContext::Missing,
            _ => NoteContext::Unreadable,
        }));
    };
    let context = tokio::task::spawn_blocking(move || {
        let extraction = prompt_tags::extract(
            &bytes,
            RequestPromptMetadata {
                request_kind: &metadata.summary.request_kind,
                original_uri: &metadata.summary.original_uri,
                request_headers: &metadata.request_headers,
                capture_complete: !body.truncated,
            },
            &tags,
            limits,
            &AtomicBool::new(false),
        );
        let verified = extraction.notes.iter().any(|note| {
            note.field_ordinal == field_ordinal
                && note.match_ordinal == match_ordinal
                && note.source_path == source_path
                && note.tag_name == expected_tag
                && note.value == expected_value
                && note.span.start() == start
                && note.span.end() == end
        });
        if !verified {
            return NoteContext::Unreadable;
        }
        let value: Value = match serde_json::from_slice(&bytes) {
            Ok(value) => value,
            Err(_) => return NoteContext::Unreadable,
        };
        let Some(text) = value.pointer(&source_path).and_then(Value::as_str) else {
            return NoteContext::Unreadable;
        };
        if prompt_tags::DecodedTextSpan::try_new(text, start, end).is_err() {
            return NoteContext::Unreadable;
        }
        let mut before_start = start.saturating_sub(512);
        while !text.is_char_boundary(before_start) {
            before_start += 1;
        }
        let mut after_end = end.saturating_add(512).min(text.len());
        while !text.is_char_boundary(after_end) {
            after_end -= 1;
        }
        let mut match_end = end.min(start.saturating_add(8192));
        while !text.is_char_boundary(match_end) {
            match_end -= 1;
        }
        NoteContext::Available {
            before: text[before_start..start].to_owned(),
            matched: text[start..match_end].to_owned(),
            after: text[end..after_end].to_owned(),
            truncated: before_start > 0 || after_end < text.len() || match_end < end,
        }
    })
    .await?;
    Ok(Some(context))
}
