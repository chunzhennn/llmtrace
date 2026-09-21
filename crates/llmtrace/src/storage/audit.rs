//! UI session administration and audit event reads.
use super::*;

pub(super) const AUDIT_SUMMARY_TOTALS_SQL: &str = r#"
        SELECT COUNT(*)::bigint AS event_count,
               COUNT(DISTINCT user_id) FILTER (WHERE user_id IS NOT NULL AND user_id <> '')::bigint AS user_count,
               COUNT(DISTINCT remote_addr) FILTER (WHERE remote_addr IS NOT NULL AND remote_addr <> '')::bigint AS remote_addr_count,
               MIN(created_at) AS first_seen_at,
               MAX(created_at) AS last_seen_at
        FROM ui_audit_events
        WHERE created_at >= $1
        "#;
pub(super) const AUDIT_SUMMARY_EVENT_TYPES_SQL: &str = r#"
        SELECT event_type AS name,
               COUNT(*)::bigint AS event_count,
               COUNT(DISTINCT user_id) FILTER (WHERE user_id IS NOT NULL AND user_id <> '')::bigint AS user_count,
               COUNT(DISTINCT remote_addr) FILTER (WHERE remote_addr IS NOT NULL AND remote_addr <> '')::bigint AS remote_addr_count,
               MIN(created_at) AS first_seen_at,
               MAX(created_at) AS last_seen_at
        FROM ui_audit_events
        WHERE created_at >= $1
        GROUP BY 1
        ORDER BY event_count DESC, name ASC
        LIMIT $2
        "#;
pub(super) const AUDIT_SUMMARY_USERS_SQL: &str = r#"
        SELECT COALESCE(NULLIF(user_id, ''), 'unknown') AS name,
               COUNT(*)::bigint AS event_count,
               COUNT(DISTINCT user_id) FILTER (WHERE user_id IS NOT NULL AND user_id <> '')::bigint AS user_count,
               COUNT(DISTINCT remote_addr) FILTER (WHERE remote_addr IS NOT NULL AND remote_addr <> '')::bigint AS remote_addr_count,
               MIN(created_at) AS first_seen_at,
               MAX(created_at) AS last_seen_at
        FROM ui_audit_events
        WHERE created_at >= $1
        GROUP BY 1
        ORDER BY event_count DESC, name ASC
        LIMIT $2
        "#;
pub(super) const AUDIT_SUMMARY_REMOTE_ADDRS_SQL: &str = r#"
        SELECT COALESCE(NULLIF(remote_addr, ''), 'unknown') AS name,
               COUNT(*)::bigint AS event_count,
               COUNT(DISTINCT user_id) FILTER (WHERE user_id IS NOT NULL AND user_id <> '')::bigint AS user_count,
               COUNT(DISTINCT remote_addr) FILTER (WHERE remote_addr IS NOT NULL AND remote_addr <> '')::bigint AS remote_addr_count,
               MIN(created_at) AS first_seen_at,
               MAX(created_at) AS last_seen_at
        FROM ui_audit_events
        WHERE created_at >= $1
        GROUP BY 1
        ORDER BY event_count DESC, name ASC
        LIMIT $2
        "#;
pub(super) const LIST_UI_SESSIONS_SQL: &str = r#"
        SELECT encode(sha256(convert_to(id, 'UTF8')), 'hex') AS session_hash,
               user_id,
               display_name,
               login_method,
               created_at,
               expires_at,
               expires_at <= $1 AS expired
        FROM ui_sessions
        WHERE $2::boolean OR expires_at > $1
        ORDER BY expires_at DESC, created_at DESC, session_hash ASC
        LIMIT $3 OFFSET $4
        "#;
pub(super) const REVOKE_UI_SESSION_SQL: &str = r#"
        DELETE FROM ui_sessions
        WHERE encode(sha256(convert_to(id, 'UTF8')), 'hex') = $1
        RETURNING encode(sha256(convert_to(id, 'UTF8')), 'hex') AS session_hash,
                  user_id,
                  display_name,
                  login_method,
                  created_at,
                  expires_at
        "#;

pub(super) fn ui_session_row(row: sqlx::postgres::PgRow, now: DateTime<Utc>) -> Value {
    let expires_at = row.get::<DateTime<Utc>, _>("expires_at");
    json!({
        "session_hash": row.get::<String, _>("session_hash"),
        "user_id": row.get::<String, _>("user_id"),
        "display_name": row.get::<String, _>("display_name"),
        "login_method": row.get::<String, _>("login_method"),
        "created_at": row.get::<DateTime<Utc>, _>("created_at"),
        "expires_at": expires_at,
        "expires_in_secs": seconds_until(now, expires_at),
        "expired": expires_at <= now,
    })
}

pub async fn list_ui_sessions(
    pool: &PgPool,
    include_expired: bool,
    limit: Option<i64>,
    offset: Option<i64>,
) -> anyhow::Result<Value> {
    let now = Utc::now();
    let page = Page::from_query(limit, offset);
    let mut tx = begin_api_read_tx(pool).await?;
    let rows = sqlx::query(LIST_UI_SESSIONS_SQL)
        .bind(now)
        .bind(include_expired)
        .bind(page.fetch_limit())
        .bind(page.offset)
        .fetch_all(&mut *tx)
        .await?;
    tx.commit().await?;

    let has_more = rows.len() > page.limit as usize;
    let items = rows
        .into_iter()
        .take(page.limit as usize)
        .map(|row| ui_session_row(row, now))
        .collect::<Vec<_>>();

    Ok(json!({
        "items": items,
        "page": {
            "limit": page.limit,
            "offset": page.offset,
            "include_expired": include_expired,
            "has_more": has_more,
            "next_offset": page.info(has_more).next_offset,
        },
    }))
}

pub async fn revoke_ui_session(pool: &PgPool, session_hash: &str) -> anyhow::Result<Option<Value>> {
    let now = Utc::now();
    let row = sqlx::query(REVOKE_UI_SESSION_SQL)
        .bind(session_hash)
        .fetch_optional(pool)
        .await?;

    Ok(row.map(|row| ui_session_row(row, now)))
}

pub async fn record_ui_audit_event(
    pool: &PgPool,
    event_type: &str,
    user_id: Option<&str>,
    detail: Value,
) -> anyhow::Result<()> {
    sqlx::query(
        r#"
        INSERT INTO ui_audit_events (event_type, user_id, remote_addr, detail)
        VALUES ($1, $2, NULL, $3)
        "#,
    )
    .bind(event_type)
    .bind(user_id)
    .bind(detail)
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn list_audit_events(
    pool: &PgPool,
    event_type: Option<String>,
    user_id: Option<String>,
    limit: Option<i64>,
    offset: Option<i64>,
) -> anyhow::Result<Value> {
    let page = Page::from_query(limit, offset);
    let mut tx = begin_api_read_tx(pool).await?;
    let rows = sqlx::query(
        r#"
        SELECT id, created_at, event_type, user_id, remote_addr, detail
        FROM ui_audit_events
        WHERE ($1::text IS NULL OR event_type = $1)
          AND ($2::text IS NULL OR user_id = $2)
        ORDER BY created_at DESC, id DESC
        LIMIT $3 OFFSET $4
        "#,
    )
    .bind(event_type)
    .bind(user_id)
    .bind(page.fetch_limit())
    .bind(page.offset)
    .fetch_all(&mut *tx)
    .await?;
    tx.commit().await?;

    let has_more = rows.len() > page.limit as usize;
    let items = read_rows::<AuditEvent>(rows.into_iter().take(page.limit as usize).collect())?;

    Ok(json!({
        "items": items,
        "page": page.info(has_more),
    }))
}

pub async fn audit_summary(
    pool: &PgPool,
    since_hours: Option<i64>,
    limit: Option<i64>,
) -> anyhow::Result<Value> {
    let window = TOP_N_WINDOW.resolve(since_hours, limit);
    let cutoff = window.cutoff(Utc::now());
    let mut tx = begin_api_read_tx(pool).await?;

    let totals = sqlx::query(AUDIT_SUMMARY_TOTALS_SQL)
        .bind(cutoff)
        .fetch_one(&mut *tx)
        .await?;
    let event_types = sqlx::query(AUDIT_SUMMARY_EVENT_TYPES_SQL)
        .bind(cutoff)
        .bind(window.limit)
        .fetch_all(&mut *tx)
        .await?;
    let users = sqlx::query(AUDIT_SUMMARY_USERS_SQL)
        .bind(cutoff)
        .bind(window.limit)
        .fetch_all(&mut *tx)
        .await?;
    let remote_addrs = sqlx::query(AUDIT_SUMMARY_REMOTE_ADDRS_SQL)
        .bind(cutoff)
        .bind(window.limit)
        .fetch_all(&mut *tx)
        .await?;

    tx.commit().await?;

    Ok(json!({
        "window": window.metadata(cutoff),
        "totals": {
            "event_count": totals.get::<i64, _>("event_count"),
            "user_count": totals.get::<i64, _>("user_count"),
            "remote_addr_count": totals.get::<i64, _>("remote_addr_count"),
            "first_seen_at": totals.try_get::<Option<DateTime<Utc>>, _>("first_seen_at")?,
            "last_seen_at": totals.try_get::<Option<DateTime<Utc>>, _>("last_seen_at")?,
        },
        "event_types": read_rows::<AuditSummaryRow>(event_types)?,
        "top_users": read_rows::<AuditSummaryRow>(users)?,
        "top_remote_addrs": read_rows::<AuditSummaryRow>(remote_addrs)?,
    }))
}
