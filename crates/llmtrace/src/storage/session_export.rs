//! Lossless export of retained session captures. Message previews are supplementary;
//! original provider payloads are the source for conversation/tool reconstruction.

use bytes::Bytes;
use tokio::sync::mpsc;

use super::*;

pub struct SessionExport {
    tx: Transaction<'static, Postgres>,
    archive: ArchiveConfig,
    id: Uuid,
    header: Value,
    expected_requests: i64,
}

impl SessionExport {
    pub async fn prepare(
        pool: &PgPool,
        archive: &ArchiveConfig,
        id: Uuid,
    ) -> anyhow::Result<Option<Self>> {
        let mut tx = begin_api_snapshot_tx(pool).await?;
        let session = sqlx::query_as::<_, SessionMetadata>(
            "SELECT id, session_key, first_seen, last_seen, user_id, user_name, summary FROM trace_sessions WHERE id = $1"
        ).bind(id).fetch_optional(&mut *tx).await?;
        let Some(session) = session else {
            tx.commit().await?;
            return Ok(None);
        };
        let expected_requests: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM request_traces WHERE session_id = $1")
                .bind(id)
                .fetch_one(&mut *tx)
                .await?;
        Ok(Some(Self {
            tx,
            archive: archive.clone(),
            id,
            header: json!({
                "type": "session",
                "schema_version": 1,
                "exported_at": Utc::now(),
                "session": session,
                "request_count": expected_requests,
            }),
            expected_requests,
        }))
    }

    pub async fn write(
        mut self,
        sender: &mpsc::Sender<Result<Bytes, io::Error>>,
    ) -> anyhow::Result<()> {
        sender.send(Ok(json_line(&self.header)?)).await?;
        let mut cursor: Option<(DateTime<Utc>, Uuid)> = None;
        let mut requests = 0;
        let mut message_previews = 0;
        let mut truncated_bodies = 0;
        let mut unavailable_bodies = 0;
        loop {
            // One request at a time also bounds memory for historical inline bodies.
            // The existing (session_id, started_at, id) index supports this cursor.
            let mut query = QueryBuilder::<Postgres>::new(format!(
                "SELECT {REQUEST_METADATA_COLUMNS} FROM request_traces WHERE session_id = "
            ));
            query.push_bind(self.id);
            if let Some((started_at, id)) = cursor {
                query
                    .push(" AND (started_at, id) > (")
                    .push_bind(started_at)
                    .push(", ")
                    .push_bind(id)
                    .push(")");
            }
            let row = query
                .push(" ORDER BY started_at ASC, id ASC LIMIT 1")
                .build_query_as::<RequestMetadata>()
                .fetch_optional(&mut *self.tx)
                .await?;
            let Some(row) = row else { break };
            let id = row.summary.id;
            cursor = Some((row.summary.started_at, id));
            let bodies = bodies::load_trace_bodies(&mut self.tx, &self.archive, &row).await?;
            let request_body = bodies.request;
            let response_body = bodies.response;
            for body in [&request_body, &response_body] {
                truncated_bodies += i64::from(body.truncated);
                unavailable_bodies += i64::from(body.status != BodyStatus::Available);
            }
            let previews = sqlx::query_as::<_, SessionMessage>(
                "SELECT id, request_id, role, content, created_at, content_truncated FROM session_messages
                 WHERE request_id = $1 AND session_id = $2 ORDER BY created_at ASC, id ASC",
            )
            .bind(id)
            .bind(self.id)
            .fetch_all(&mut *self.tx)
            .await?;
            message_previews += previews.len() as i64;
            let request = row;
            // Escaping large JSON strings is CPU work, just like archive decoding.
            let line = tokio::task::spawn_blocking(move || {
                json_line(&json!({
                    "type": "request",
                    "request": request,
                    "request_body": request_body.into_export(),
                    "response_body": response_body.into_export(),
                    "message_previews": previews,
                }))
            })
            .await??;
            sender.send(Ok(line)).await?;
            requests += 1;
        }
        anyhow::ensure!(
            requests == self.expected_requests,
            "session export count mismatch"
        );
        self.tx.commit().await?;
        // Consumers must require this final record; HTTP 200 alone is not success.
        sender
            .send(Ok(json_line(&json!({
                "type": "end",
                "session_id": self.id,
                "export_complete": true,
                "request_count": requests,
                "message_preview_count": message_previews,
                "truncated_body_count": truncated_bodies,
                "unavailable_body_count": unavailable_bodies,
                "captured_bodies_complete": truncated_bodies == 0 && unavailable_bodies == 0,
            }))?))
            .await?;
        Ok(())
    }
}

fn json_line(value: &Value) -> anyhow::Result<Bytes> {
    let mut bytes = serde_json::to_vec(value)?;
    bytes.push(b'\n');
    Ok(bytes.into())
}

#[cfg(test)]
mod tests;
