//! Paired body reads for details and exports. One snapshot, one decode per segment.
use base64::Engine;
use base64::engine::general_purpose::STANDARD;

use super::*;

pub(super) struct CapturedBody {
    pub status: BodyStatus,
    pub bytes: Option<Vec<u8>>,
    pub truncated: bool,
}

impl CapturedBody {
    fn available(bytes: Vec<u8>, truncated: bool) -> Self {
        Self {
            status: BodyStatus::Available,
            bytes: Some(bytes),
            truncated,
        }
    }

    fn unavailable(status: BodyStatus, truncated: bool) -> Self {
        Self {
            status,
            bytes: None,
            truncated,
        }
    }

    pub fn text(&self) -> String {
        self.bytes
            .as_deref()
            .map(|bytes| String::from_utf8_lossy(bytes).into_owned())
            .unwrap_or_default()
    }

    pub fn into_export(self) -> ExportBody {
        let captured_bytes = self.bytes.as_ref().map(Vec::len);
        let (encoding, data) = match self.bytes {
            Some(bytes) => match String::from_utf8(bytes) {
                Ok(text) => (Some("utf8"), Some(text)),
                Err(error) => (Some("base64"), Some(STANDARD.encode(error.as_bytes()))),
            },
            None => (None, None),
        };
        ExportBody {
            status: self.status,
            encoding,
            data,
            captured_bytes,
            truncated: self.truncated,
        }
    }
}

pub(super) struct CapturedBodies {
    pub request: CapturedBody,
    pub response: CapturedBody,
}

#[derive(Debug, Serialize)]
pub(super) struct ExportBody {
    pub status: BodyStatus,
    pub encoding: Option<&'static str>,
    pub data: Option<String>,
    pub captured_bytes: Option<usize>,
    pub truncated: bool,
}

#[derive(sqlx::FromRow)]
struct ArchiveRecord {
    direction: String,
    segment_id: Uuid,
    uncompressed_offset: i64,
    uncompressed_len: i64,
    body_sha256: String,
    complete: bool,
}

#[derive(sqlx::FromRow)]
struct ArchiveSegment {
    id: Uuid,
    uncompressed_bytes: i64,
    storage_backend: String,
    storage_key: String,
    checksum_sha256: String,
    compressed_payload: Option<Vec<u8>>,
}

pub(super) async fn load_trace_bodies(
    tx: &mut Transaction<'_, Postgres>,
    archive: &ArchiveConfig,
    request: &RequestMetadata,
) -> anyhow::Result<CapturedBodies> {
    let id = request.summary.id;
    let records = sqlx::query_as::<_, ArchiveRecord>(
        "SELECT direction, segment_id, uncompressed_offset, uncompressed_len, body_sha256, complete
         FROM payload_archive_records WHERE trace_id = $1 AND direction IN ('request_body', 'response_body')",
    ).bind(id).fetch_all(&mut **tx).await?;
    let mut ids: Vec<Uuid> = records.iter().map(|r| r.segment_id).collect();
    ids.sort_unstable();
    ids.dedup();
    let segments = sqlx::query_as::<_, ArchiveSegment>(
        "SELECT s.id, s.uncompressed_bytes, s.storage_backend, s.storage_key,
                s.checksum_sha256, b.compressed_payload
         FROM payload_archive_segments s
         LEFT JOIN payload_archive_segment_blobs b ON b.segment_id = s.id
         WHERE s.id = ANY($1)",
    )
    .bind(ids)
    .fetch_all(&mut **tx)
    .await?;
    // Fetch legacy data only when an archive record is absent. Metadata-only
    // requests never execute any of these body queries.
    let inline: Option<(Vec<u8>, Vec<u8>)> = if records.len() < 2 {
        sqlx::query_as("SELECT request_body_compressed, response_body_compressed FROM request_traces WHERE id = $1")
            .bind(id).fetch_optional(&mut **tx).await?
    } else {
        None
    };
    let archive = archive.clone();
    let observed = [request.request_body_bytes, request.response_body_bytes];
    let truncated = [
        request.summary.request_body_truncated,
        request.summary.response_body_truncated,
    ];
    tokio::task::spawn_blocking(move || {
        let inline = inline.unwrap_or_default();
        decode_bodies(
            id,
            records,
            segments,
            [inline.0, inline.1],
            observed,
            truncated,
            |segment| decode_segment(&archive, segment),
        )
    })
    .await
    .context("body reader task failed")
}

fn decode_segment(archive: &ArchiveConfig, segment: ArchiveSegment) -> anyhow::Result<Vec<u8>> {
    let compressed = match segment.storage_backend.as_str() {
        "filesystem" => read_archive_file(&archive.filesystem_root, &segment.storage_key)?,
        "postgres" => segment
            .compressed_payload
            .context("archive blob is missing")?,
        other => anyhow::bail!("unknown archive storage backend {other:?}"),
    };
    anyhow::ensure!(
        sha256_hex(&compressed) == segment.checksum_sha256,
        "archive checksum mismatch"
    );
    decompress_with_limit(
        &compressed,
        archive_decode_limit(segment.uncompressed_bytes)?,
    )
}

fn decode_bodies(
    id: Uuid,
    records: Vec<ArchiveRecord>,
    segments: Vec<ArchiveSegment>,
    inline: [Vec<u8>; 2],
    observed: [i64; 2],
    truncated: [bool; 2],
    mut decode: impl FnMut(ArchiveSegment) -> anyhow::Result<Vec<u8>>,
) -> CapturedBodies {
    let index = |direction: &str| usize::from(direction == "response_body");
    let mut bodies = std::array::from_fn::<_, 2, _>(|i| {
        CapturedBody::unavailable(BodyStatus::Unreadable, truncated[i])
    });
    for record in &records {
        bodies[index(&record.direction)].truncated |= !record.complete;
    }
    // Process a shared segment only once, and drop it before decoding the next.
    for segment in segments {
        let segment_id = segment.id;
        match decode(segment) {
            Ok(bytes) => {
                for record in records
                    .iter()
                    .filter(|record| record.segment_id == segment_id)
                {
                    let i = index(&record.direction);
                    let decoded = (|| -> anyhow::Result<Vec<u8>> {
                        let body = decode_archive_frame_body(&bytes, record.uncompressed_offset)?;
                        anyhow::ensure!(
                            body.len() as i64 == record.uncompressed_len,
                            "archive body length mismatch"
                        );
                        anyhow::ensure!(
                            sha256_hex(&body) == record.body_sha256,
                            "archive body checksum mismatch"
                        );
                        Ok(body)
                    })();
                    match decoded {
                        Ok(body) => bodies[i] = CapturedBody::available(body, bodies[i].truncated),
                        Err(error) => {
                            tracing::warn!(%id, %segment_id, %error, "stored body unavailable")
                        }
                    }
                }
            }
            Err(error) => tracing::warn!(%id, %segment_id, %error, "archive segment unavailable"),
        }
    }
    for (i, compressed) in inline.into_iter().enumerate() {
        if records.iter().any(|record| index(&record.direction) == i) {
            continue;
        }
        bodies[i] = if compressed.is_empty() {
            if observed[i] == 0 {
                CapturedBody::available(Vec::new(), truncated[i])
            } else {
                CapturedBody::unavailable(BodyStatus::Missing, truncated[i])
            }
        } else {
            match archive_decode_limit(observed[i])
                .and_then(|limit| decompress_with_limit(&compressed, limit))
            {
                Ok(bytes) => CapturedBody::available(bytes, truncated[i]),
                Err(error) => {
                    tracing::warn!(%id, %error, "legacy body unavailable");
                    CapturedBody::unavailable(BodyStatus::Unreadable, truncated[i])
                }
            }
        };
    }
    let [request, response] = bodies;
    CapturedBodies { request, response }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shared_segment_is_decoded_once_for_both_bodies() {
        let id = Uuid::new_v4();
        let segment_id = Uuid::new_v4();
        let request =
            encode_archive_frame(id, PayloadDirection::RequestBody, None, b"request").unwrap();
        let response =
            encode_archive_frame(id, PayloadDirection::ResponseBody, None, b"response").unwrap();
        let offset = request.len() as i64;
        let data = [request, response].concat();
        let record = |direction: &str, offset, body: &[u8]| ArchiveRecord {
            direction: direction.into(),
            segment_id,
            uncompressed_offset: offset,
            uncompressed_len: body.len() as i64,
            body_sha256: sha256_hex(body),
            complete: true,
        };
        let segment = ArchiveSegment {
            id: segment_id,
            uncompressed_bytes: data.len() as i64,
            storage_backend: "postgres".into(),
            storage_key: String::new(),
            checksum_sha256: String::new(),
            compressed_payload: None,
        };
        let mut decodes = 0;
        let bodies = decode_bodies(
            id,
            vec![
                record("request_body", 0, b"request"),
                record("response_body", offset, b"response"),
            ],
            vec![segment],
            [vec![], vec![]],
            [7, 8],
            [false; 2],
            |_| {
                decodes += 1;
                Ok(data.clone())
            },
        );
        assert_eq!(decodes, 1);
        assert_eq!(bodies.request.text(), "request");
        assert_eq!(bodies.response.text(), "response");
    }

    #[test]
    fn export_preserves_binary_text_and_missing_status() {
        for bytes in [
            b"\xff\x00\xfe".to_vec(),
            "hello\n世界\n".as_bytes().to_vec(),
        ] {
            let body = CapturedBody::available(bytes.clone(), true).into_export();
            let decoded = match body.encoding.unwrap() {
                "base64" => STANDARD.decode(body.data.as_ref().unwrap()).unwrap(),
                _ => body.data.as_ref().unwrap().as_bytes().to_vec(),
            };
            assert_eq!(decoded, bytes);
            assert_eq!(body.captured_bytes, Some(bytes.len()));
            assert!(body.truncated);
        }
        let missing = CapturedBody::unavailable(BodyStatus::Missing, false).into_export();
        assert_eq!(missing.status, BodyStatus::Missing);
        assert_eq!(missing.data, None);
        let empty = CapturedBody::available(vec![], false).into_export();
        assert_eq!(empty.data.as_deref(), Some(""));
        assert_eq!(empty.captured_bytes, Some(0));
    }
}
