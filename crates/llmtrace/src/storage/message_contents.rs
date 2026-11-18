//! Exact preview-text identity, transactional interning and reference-safe GC.
//! Occurrences retain role, order and nullable legacy truncation independently.
use std::collections::BTreeMap;

use sha2::{Digest, Sha256};
use sqlx::{Postgres, Row, Transaction};

use crate::types::ParsedMessage;

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct ContentHash(pub(super) [u8; 32]);

impl ContentHash {
    pub(super) fn of(content: &str) -> Self {
        Self(Sha256::digest(content.as_bytes()).into())
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) struct MessageContentId(i64);

/// Exactly one ID per input message, in input order (including duplicates).
pub(super) struct InternedMessages {
    ids: Vec<MessageContentId>,
}

impl InternedMessages {
    pub(super) fn into_ids(self, expected: usize) -> Result<Vec<i64>, MessageStorageError> {
        if self.ids.len() != expected {
            return Err(MessageStorageError::BatchMapping);
        }
        Ok(self.ids.into_iter().map(|id| id.0).collect())
    }
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum MessageStorageError {
    #[error("message dictionary database operation failed")]
    Database(#[from] sqlx::Error),
    #[error("message dictionary hash/content integrity mismatch")]
    ContentMismatch,
    #[error("message dictionary batch mapping is incomplete")]
    BatchMapping,
    #[error("message dictionary contention exceeded retry budget")]
    Contention,
    #[error("invalid message write mode")]
    InvalidMode,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum MessageWriteMode {
    Legacy,
    DualWrite,
    ReferencesOnly,
}

impl MessageWriteMode {
    pub(super) fn as_str(self) -> &'static str {
        match self {
            Self::Legacy => "legacy",
            Self::DualWrite => "dual_write",
            Self::ReferencesOnly => "references_only",
        }
    }

    pub(super) fn parse(value: &str) -> Result<Self, MessageStorageError> {
        match value {
            "legacy" => Ok(Self::Legacy),
            "dual_write" => Ok(Self::DualWrite),
            "references_only" => Ok(Self::ReferencesOnly),
            _ => Err(MessageStorageError::InvalidMode),
        }
    }
}

/// Borrows bounded, already-persistable previews; never clones message bodies.
pub(super) async fn intern_batch(
    tx: &mut Transaction<'_, Postgres>,
    messages: &[ParsedMessage],
) -> Result<InternedMessages, MessageStorageError> {
    let inputs: Vec<_> = messages
        .iter()
        .map(|message| (ContentHash::of(&message.content), message.content.as_str()))
        .collect();
    intern_texts(tx, &inputs).await
}

pub(super) async fn intern_texts(
    tx: &mut Transaction<'_, Postgres>,
    inputs: &[(ContentHash, &str)],
) -> Result<InternedMessages, MessageStorageError> {
    let mut unique = BTreeMap::new();
    for &(hash, content) in inputs {
        if unique
            .insert(hash, content)
            .is_some_and(|old| old != content)
        {
            return Err(MessageStorageError::ContentMismatch);
        }
    }
    if inputs.is_empty() {
        return Ok(InternedMessages { ids: Vec::new() });
    }
    let hashes: Vec<Vec<u8>> = unique.keys().map(|hash| hash.0.to_vec()).collect();
    let contents: Vec<&str> = unique.values().copied().collect();
    // Repeated histories normally find every text here, avoiding an INSERT round
    // trip and conflict/sequence work for a dictionary that already contains it.
    // Missing rows still use bounded insert/lookup retries: a concurrent GC can
    // remove an unlocked conflict row before the next READ COMMITTED statement.
    for attempt in 0..=3 {
        let rows = sqlx::query(
            "SELECT id, content_sha256, content_bytes, content FROM message_contents
             WHERE content_sha256 = ANY($1) ORDER BY id FOR KEY SHARE",
        )
        .bind(&hashes)
        .fetch_all(&mut **tx)
        .await?;
        if rows.len() != unique.len() {
            if attempt == 3 {
                return Err(MessageStorageError::Contention);
            }
            sqlx::query(
                "INSERT INTO message_contents(content_sha256, content_bytes, content)
                 SELECT hash, octet_length(content), content
                 FROM UNNEST($1::bytea[], $2::text[]) AS input(hash, content)
                 ORDER BY hash ON CONFLICT (content_sha256) DO NOTHING",
            )
            .bind(&hashes)
            .bind(&contents)
            .execute(&mut **tx)
            .await?;
            continue;
        }
        let mut ids = BTreeMap::new();
        for row in rows {
            let hash: Vec<u8> = row.try_get("content_sha256")?;
            let hash = ContentHash(
                hash.try_into()
                    .map_err(|_| MessageStorageError::ContentMismatch)?,
            );
            let content: &str = row.try_get("content")?;
            let bytes: i32 = row.try_get("content_bytes")?;
            if unique.get(&hash).copied() != Some(content)
                || usize::try_from(bytes).ok() != Some(content.len())
            {
                return Err(MessageStorageError::ContentMismatch);
            }
            ids.insert(hash, MessageContentId(row.try_get("id")?));
        }
        let ids = inputs
            .iter()
            .map(|(hash, _)| {
                ids.get(hash)
                    .copied()
                    .ok_or(MessageStorageError::BatchMapping)
            })
            .collect::<Result<Vec<_>, _>>()?;
        return Ok(InternedMessages { ids });
    }
    Err(MessageStorageError::Contention)
}

/// Lock candidates before checking references in a new READ COMMITTED statement.
/// Writers take KEY SHARE locks in the same ID order. Never trust a refcount or
/// a snapshot taken before waiting for a writer to commit its reference.
pub(super) async fn collect_unused(
    tx: &mut Transaction<'_, Postgres>,
    limit: i64,
) -> Result<u64, MessageStorageError> {
    let prepared: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM pg_index WHERE indexrelid = to_regclass('idx_session_messages_content_id') AND indisvalid)")
        .fetch_one(&mut **tx).await?;
    if !prepared {
        return Ok(0);
    }
    let ids: Vec<i64> = sqlx::query_scalar(
        "SELECT id FROM message_contents c
         WHERE NOT EXISTS (SELECT 1 FROM session_messages m WHERE m.content_id = c.id)
         ORDER BY id LIMIT $1 FOR UPDATE SKIP LOCKED",
    )
    .bind(limit)
    .fetch_all(&mut **tx)
    .await?;
    Ok(sqlx::query(
        "DELETE FROM message_contents c WHERE id = ANY($1)
         AND NOT EXISTS (SELECT 1 FROM session_messages m WHERE m.content_id = c.id)",
    )
    .bind(&ids)
    .execute(&mut **tx)
    .await?
    .rows_affected())
}

#[cfg(test)]
mod tests;
