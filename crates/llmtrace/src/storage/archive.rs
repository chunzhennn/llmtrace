//! Archive framing, compression, persistence and filesystem access.
use super::*;

pub(super) const MAX_ARCHIVE_SEGMENT_DECODE_BYTES: usize = 2 * 1024 * 1024 * 1024;
pub(super) const ARCHIVE_DIRECTION_REQUEST_BODY: &str = "request_body";
pub(super) const ARCHIVE_DIRECTION_RESPONSE_BODY: &str = "response_body";
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PayloadDirection {
    RequestBody,
    ResponseBody,
}

impl PayloadDirection {
    fn as_str(self) -> &'static str {
        match self {
            Self::RequestBody => ARCHIVE_DIRECTION_REQUEST_BODY,
            Self::ResponseBody => ARCHIVE_DIRECTION_RESPONSE_BODY,
        }
    }
}

#[derive(Debug)]
pub(super) struct ArchiveRecordWrite {
    direction: PayloadDirection,
    content_type: Option<String>,
    uncompressed_offset: i64,
    uncompressed_len: i64,
    body_sha256: String,
}

#[cfg(test)]
pub(super) fn compress(data: &[u8]) -> anyhow::Result<Vec<u8>> {
    compress_with_level(data, 3)
}

pub(super) fn compress_with_level(data: &[u8], level: i32) -> anyhow::Result<Vec<u8>> {
    if data.is_empty() {
        return Ok(Vec::new());
    }
    Ok(zstd::stream::encode_all(data, level)?)
}

pub fn decompress_with_limit(data: &[u8], limit: usize) -> anyhow::Result<Vec<u8>> {
    if data.is_empty() {
        return Ok(Vec::new());
    }
    let read_limit = limit
        .checked_add(1)
        .ok_or_else(|| anyhow::anyhow!("body decompression limit is too large"))?;
    let mut decoder = zstd::stream::read::Decoder::new(data)?;
    let mut output = Vec::new();
    decoder
        .by_ref()
        .take(read_limit as u64)
        .read_to_end(&mut output)?;
    if output.len() > limit {
        anyhow::bail!("stored trace body exceeds decompression limit of {limit} bytes");
    }
    Ok(output)
}

pub(super) async fn prepare_trace_payloads(
    archive: &ArchiveConfig,
    trace: &mut TraceRecord,
) -> anyhow::Result<Vec<PreparedArchiveSegment>> {
    let id = trace.id;
    let request = std::mem::take(&mut trace.request_body);
    let response = std::mem::take(&mut trace.response_body);
    let content_type = trace.content_type.clone();
    let archive = archive.clone();
    tokio::task::spawn_blocking(move || {
        let prepared = vec![
            prepare_archive_record(id, PayloadDirection::RequestBody, None, &request)?,
            prepare_archive_record(id, PayloadDirection::ResponseBody, content_type, &response)?,
        ];
        drop(request);
        drop(response);
        let total = prepared
            .iter()
            .try_fold(0usize, |total, record| {
                total.checked_add(record.frame.len())
            })
            .ok_or_else(|| anyhow::anyhow!("archive segment append is too large"))?;
        if total > archive.segment_uncompressed_bytes {
            prepared
                .into_iter()
                .map(|record| prepare_archive_segment(vec![record], archive.compression_level))
                .collect()
        } else {
            Ok(vec![prepare_archive_segment(
                prepared,
                archive.compression_level,
            )?])
        }
    })
    .await?
}

pub(super) struct PreparedArchiveSegment {
    compressed: Vec<u8>,
    checksum: String,
    uncompressed_bytes: i64,
    writes: Vec<ArchiveRecordWrite>,
}

pub(super) fn prepare_archive_segment(
    records: Vec<PreparedArchiveRecord>,
    level: i32,
) -> anyhow::Result<PreparedArchiveSegment> {
    let mut buffer = Vec::new();
    let mut writes = Vec::with_capacity(records.len());
    for record in records {
        let offset = usize_to_i64_checked(buffer.len(), "archive segment offset")?;
        if buffer.is_empty() {
            buffer = record.frame;
        } else {
            buffer.extend_from_slice(&record.frame);
        }
        writes.push(ArchiveRecordWrite {
            direction: record.direction,
            content_type: record.content_type,
            uncompressed_offset: offset,
            uncompressed_len: record.uncompressed_len,
            body_sha256: record.body_sha256,
        });
    }
    let uncompressed_bytes = usize_to_i64_checked(buffer.len(), "archive segment size")?;
    let compressed = compress_with_level(&buffer, level)?;
    let checksum = sha256_hex(&compressed);
    Ok(PreparedArchiveSegment {
        compressed,
        checksum,
        uncompressed_bytes,
        writes,
    })
}

pub(super) async fn archive_prepared_payloads(
    tx: &mut Transaction<'_, Postgres>,
    archive: &ArchiveConfig,
    trace: &TraceRecord,
    prepared: PreparedArchiveSegment,
    journal_id: Option<Uuid>,
) -> anyhow::Result<()> {
    let lock_key = trace
        .session_id
        .map(|id| id.to_string())
        .unwrap_or_else(|| trace.id.to_string());
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))")
        .bind(lock_key)
        .execute(&mut **tx)
        .await?;

    // Immutable segments: never rewrite bytes referenced by a committed trace.
    let segment_id = if let Some(journal_id) = journal_id {
        let mut digest = Sha256::new();
        digest.update(journal_id.as_bytes());
        digest.update(trace.id.as_bytes());
        digest.update(prepared.checksum.as_bytes());
        Uuid::from_bytes(digest.finalize()[..16].try_into().unwrap())
    } else {
        Uuid::new_v4()
    };
    let segment_index = next_archive_segment_index(tx, trace.session_id).await?;
    // Repeated transaction failures reuse the same immutable bytes instead of
    // leaving a fresh orphan file on every journal retry. The key is independent
    // of a session UUID that might itself have rolled back.
    let storage_key = if let Some(journal_id) = journal_id {
        format!(
            "journal/{journal_id}/{}/{}.zst",
            trace.id, prepared.checksum
        )
    } else {
        archive_storage_key(trace.session_id, segment_id, segment_index)
    };
    let PreparedArchiveSegment {
        compressed,
        checksum,
        uncompressed_bytes,
        writes,
    } = prepared;
    let level = archive.compression_level;
    let compressed_bytes = usize_to_i64_checked(compressed.len(), "archive compressed size")?;
    let record_count = i64::try_from(writes.len()).context("archive record count overflow")?;

    // The parent must exist before inserting a PostgreSQL blob (including fallback).
    upsert_archive_segment(
        tx,
        segment_id,
        trace.session_id,
        segment_index,
        uncompressed_bytes,
        compressed_bytes,
        record_count,
        level,
        archive.storage_backend.as_str(),
        &storage_key,
        &checksum,
    )
    .await?;
    let (backend, key) =
        persist_archive_segment(tx, archive, segment_id, &storage_key, compressed).await?;
    sqlx::query(
        "UPDATE payload_archive_segments SET storage_backend = $2, storage_key = $3 WHERE id = $1",
    )
    .bind(segment_id)
    .bind(backend)
    .bind(key)
    .execute(&mut **tx)
    .await?;
    seal_archive_segment(tx, segment_id).await?;

    for (index, write) in writes.into_iter().enumerate() {
        let record_index = i64::try_from(index).context("archive record index overflow")?;
        insert_archive_record(tx, trace, segment_id, record_index, write).await?;
    }

    Ok(())
}

pub(super) struct PreparedArchiveRecord {
    direction: PayloadDirection,
    content_type: Option<String>,
    uncompressed_len: i64,
    body_sha256: String,
    frame: Vec<u8>,
}

pub(super) fn prepare_archive_record(
    trace_id: Uuid,
    direction: PayloadDirection,
    content_type: Option<String>,
    body: &[u8],
) -> anyhow::Result<PreparedArchiveRecord> {
    let frame = encode_archive_frame(trace_id, direction, content_type.as_deref(), body)?;
    Ok(PreparedArchiveRecord {
        direction,
        content_type,
        uncompressed_len: usize_to_i64_checked(body.len(), "archive body size")?,
        body_sha256: sha256_hex(body),
        frame,
    })
}

pub(super) fn encode_archive_frame(
    trace_id: Uuid,
    direction: PayloadDirection,
    content_type: Option<&str>,
    body: &[u8],
) -> anyhow::Result<Vec<u8>> {
    let header = json!({
        "trace_id": trace_id,
        "direction": direction.as_str(),
        "content_type": content_type,
    });
    let header = serde_json::to_vec(&header)?;
    let header_len = u32::try_from(header.len()).context("archive frame header is too large")?;
    let body_len = u64::try_from(body.len()).context("archive frame body is too large")?;
    let mut frame = Vec::with_capacity(4 + header.len() + 8 + body.len());
    frame.extend_from_slice(&header_len.to_be_bytes());
    frame.extend_from_slice(&header);
    frame.extend_from_slice(&body_len.to_be_bytes());
    frame.extend_from_slice(body);
    Ok(frame)
}

pub(super) fn decode_archive_frame_body(segment: &[u8], offset: i64) -> anyhow::Result<Vec<u8>> {
    if offset < 0 {
        anyhow::bail!("archive frame offset must not be negative");
    }
    let mut cursor = usize::try_from(offset).context("archive frame offset is too large")?;
    let header_len_end = cursor
        .checked_add(4)
        .ok_or_else(|| anyhow::anyhow!("archive frame header length overflows"))?;
    let header_len_bytes = segment
        .get(cursor..header_len_end)
        .ok_or_else(|| anyhow::anyhow!("archive frame header length is out of bounds"))?;
    let header_len = u32::from_be_bytes(header_len_bytes.try_into().unwrap()) as usize;
    cursor = header_len_end;
    let header_end = cursor
        .checked_add(header_len)
        .ok_or_else(|| anyhow::anyhow!("archive frame header length overflows"))?;
    segment
        .get(cursor..header_end)
        .ok_or_else(|| anyhow::anyhow!("archive frame header is out of bounds"))?;
    cursor = header_end;
    let body_len_end = cursor
        .checked_add(8)
        .ok_or_else(|| anyhow::anyhow!("archive frame body length overflows"))?;
    let body_len_bytes = segment
        .get(cursor..body_len_end)
        .ok_or_else(|| anyhow::anyhow!("archive frame body length is out of bounds"))?;
    let body_len = u64::from_be_bytes(body_len_bytes.try_into().unwrap());
    let body_len = usize::try_from(body_len).context("archive frame body is too large")?;
    cursor = body_len_end;
    let body_end = cursor
        .checked_add(body_len)
        .ok_or_else(|| anyhow::anyhow!("archive frame body length overflows"))?;
    let body = segment
        .get(cursor..body_end)
        .ok_or_else(|| anyhow::anyhow!("archive frame body is out of bounds"))?;
    Ok(body.to_vec())
}

pub(super) async fn next_archive_segment_index(
    tx: &mut Transaction<'_, Postgres>,
    session_id: Option<Uuid>,
) -> anyhow::Result<i64> {
    let row = sqlx::query(
        r#"
        SELECT COALESCE(MAX(segment_index), -1)::bigint AS max_segment_index
        FROM payload_archive_segments
        WHERE (($1::uuid IS NULL AND session_id IS NULL) OR session_id = $1)
        "#,
    )
    .bind(session_id)
    .fetch_one(&mut **tx)
    .await?;
    let max_segment_index: i64 = row.get("max_segment_index");
    max_segment_index
        .checked_add(1)
        .ok_or_else(|| anyhow::anyhow!("archive segment index overflow"))
}

pub(super) async fn seal_archive_segment(
    tx: &mut Transaction<'_, Postgres>,
    segment_id: Uuid,
) -> anyhow::Result<()> {
    sqlx::query(
        r#"
        UPDATE payload_archive_segments
        SET sealed = true,
            sealed_at = COALESCE(sealed_at, now()),
            updated_at = now()
        WHERE id = $1
        "#,
    )
    .bind(segment_id)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

pub(super) async fn persist_archive_segment(
    tx: &mut Transaction<'_, Postgres>,
    archive: &ArchiveConfig,
    segment_id: Uuid,
    preferred_storage_key: &str,
    compressed: Vec<u8>,
) -> anyhow::Result<(String, String)> {
    let compressed = if archive.storage_backend == ArchiveStorageBackend::Filesystem {
        let root = archive.filesystem_root.clone();
        let key = preferred_storage_key.to_string();
        let (result, compressed) = tokio::task::spawn_blocking(move || {
            let result = write_archive_file(&root, &key, segment_id, &compressed);
            (result, compressed)
        })
        .await?;
        match result {
            Ok(()) => {
                sqlx::query("DELETE FROM payload_archive_segment_blobs WHERE segment_id = $1")
                    .bind(segment_id)
                    .execute(&mut **tx)
                    .await?;
                return Ok((
                    ArchiveStorageBackend::Filesystem.as_str().to_string(),
                    preferred_storage_key.to_string(),
                ));
            }
            Err(error) => {
                tracing::warn!(
                    %segment_id,
                    error = %error,
                    "filesystem archive write failed; falling back to postgres"
                );
            }
        }
        compressed
    } else {
        compressed
    };

    sqlx::query(
        r#"
        INSERT INTO payload_archive_segment_blobs (segment_id, compressed_payload)
        VALUES ($1, $2)
        ON CONFLICT (segment_id) DO UPDATE
        SET compressed_payload = EXCLUDED.compressed_payload
        "#,
    )
    .bind(segment_id)
    .bind(compressed)
    .execute(&mut **tx)
    .await?;
    Ok((
        ArchiveStorageBackend::Postgres.as_str().to_string(),
        segment_id.to_string(),
    ))
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn upsert_archive_segment(
    tx: &mut Transaction<'_, Postgres>,
    segment_id: Uuid,
    session_id: Option<Uuid>,
    segment_index: i64,
    uncompressed_bytes: i64,
    compressed_bytes: i64,
    record_count: i64,
    compression_level: i32,
    storage_backend: &str,
    storage_key: &str,
    checksum_sha256: &str,
) -> anyhow::Result<()> {
    sqlx::query(
        r#"
        INSERT INTO payload_archive_segments (
            id, session_id, segment_index, uncompressed_bytes, compressed_bytes, record_count,
            compression_codec, compression_level, storage_backend, storage_key, checksum_sha256,
            sealed
        )
        VALUES ($1,$2,$3,$4,$5,$6,'zstd',$7,$8,$9,$10,false)
        ON CONFLICT (id) DO UPDATE
        SET updated_at = now(),
            uncompressed_bytes = EXCLUDED.uncompressed_bytes,
            compressed_bytes = EXCLUDED.compressed_bytes,
            record_count = EXCLUDED.record_count,
            compression_codec = EXCLUDED.compression_codec,
            compression_level = EXCLUDED.compression_level,
            storage_backend = EXCLUDED.storage_backend,
            storage_key = EXCLUDED.storage_key,
            checksum_sha256 = EXCLUDED.checksum_sha256,
            sealed = false,
            sealed_at = NULL
        "#,
    )
    .bind(segment_id)
    .bind(session_id)
    .bind(segment_index)
    .bind(uncompressed_bytes)
    .bind(compressed_bytes)
    .bind(record_count)
    .bind(compression_level)
    .bind(storage_backend)
    .bind(storage_key)
    .bind(checksum_sha256)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

pub(super) async fn insert_archive_record(
    tx: &mut Transaction<'_, Postgres>,
    trace: &TraceRecord,
    segment_id: Uuid,
    record_index: i64,
    write: ArchiveRecordWrite,
) -> anyhow::Result<()> {
    sqlx::query(
        r#"
        INSERT INTO payload_archive_records (
            id, trace_id, session_id, segment_id, record_index, direction, content_type,
            uncompressed_offset, uncompressed_len, body_sha256, complete
        )
        VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11)
        "#,
    )
    .bind(Uuid::new_v4())
    .bind(trace.id)
    .bind(trace.session_id)
    .bind(segment_id)
    .bind(record_index)
    .bind(write.direction.as_str())
    .bind(&write.content_type)
    .bind(write.uncompressed_offset)
    .bind(write.uncompressed_len)
    .bind(&write.body_sha256)
    .bind(match write.direction {
        PayloadDirection::RequestBody => !trace.request_body_truncated,
        PayloadDirection::ResponseBody => !trace.response_body_truncated,
    })
    .execute(&mut **tx)
    .await?;
    Ok(())
}

pub(super) fn archive_storage_key(
    session_id: Option<Uuid>,
    segment_id: Uuid,
    segment_index: i64,
) -> String {
    let owner = session_id
        .map(|id| id.to_string())
        .unwrap_or_else(|| format!("unscoped/{segment_id}"));
    format!("{owner}/{segment_index:020}-{segment_id}.zst")
}

pub(super) fn write_archive_file(
    root: &Path,
    storage_key: &str,
    segment_id: Uuid,
    compressed: &[u8],
) -> io::Result<()> {
    use std::io::Write;
    let path = archive_file_path(root, storage_key)?;
    let mut directories = fs::DirBuilder::new();
    directories.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        directories.mode(0o700);
    }
    if let Some(parent) = path.parent() {
        directories.create(parent)?;
    }
    let tmp_path = path.with_extension(format!("zst.tmp-{segment_id}"));
    let mut options = fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&tmp_path)?;
    file.write_all(compressed)?;
    file.sync_all()?;
    fs::rename(tmp_path, &path)?;
    // Database commit must never acknowledge an archive still only in page cache.
    // Sync ancestors as recursive directory creation can introduce several levels.
    let parent = fs::canonicalize(path.parent().unwrap_or_else(|| Path::new(".")))?;
    for directory in parent.ancestors() {
        fs::File::open(directory)?.sync_all()?;
    }
    Ok(())
}

pub(super) fn read_archive_file(root: &Path, storage_key: &str) -> io::Result<Vec<u8>> {
    fs::read(archive_file_path(root, storage_key)?)
}

pub(super) fn archive_file_path(root: &Path, storage_key: &str) -> io::Result<PathBuf> {
    let key = Path::new(storage_key);
    if key.is_absolute()
        || key.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "archive storage key must be a relative path without parent components",
        ));
    }
    Ok(root.join(key))
}

pub(super) fn archive_decode_limit(uncompressed_bytes: i64) -> anyhow::Result<usize> {
    if uncompressed_bytes < 0 {
        anyhow::bail!("archive segment size must not be negative");
    }
    let limit = usize::try_from(uncompressed_bytes).context("archive segment size is too large")?;
    if limit > MAX_ARCHIVE_SEGMENT_DECODE_BYTES {
        anyhow::bail!(
            "archive segment exceeds decode limit of {MAX_ARCHIVE_SEGMENT_DECODE_BYTES} bytes"
        );
    }
    Ok(limit)
}

pub(super) fn usize_to_i64_checked(value: usize, label: &str) -> anyhow::Result<i64> {
    i64::try_from(value).with_context(|| format!("{label} exceeds i64 range"))
}

pub(super) fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hex::encode(hasher.finalize())
}
