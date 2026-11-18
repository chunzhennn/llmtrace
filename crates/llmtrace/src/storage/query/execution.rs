//! Query validation, bounded execution and authenticated live-result cursors.
use std::sync::OnceLock;
use std::time::Duration;

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use chrono::{DateTime, Utc};
use futures_util::TryStreamExt;
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::postgres::PgPoolOptions;
use sqlx::{PgPool, Postgres, QueryBuilder, Row};
use tokio::sync::Semaphore;
use uuid::Uuid;

use crate::prompt_tags::NoteIndexVersion;

use super::session_note_filter::SessionNoteFilter;
use super::{
    DatasetSpec, QueryField, QueryFilter, SortDirection, StructuredQuery, StructuredQueryError,
};

const MAX_RESPONSE_BYTES: usize = 8 * 1024 * 1024;
const QUERY_DEADLINE: Duration = Duration::from_secs(6);
static QUERY_SLOTS: OnceLock<Semaphore> = OnceLock::new();

/// Owns an actual separate pool, not a clone of the ingestion pool. The pool is
/// lazy so existing synchronous AppState construction remains deterministic.
pub(crate) struct QueryExecutor {
    pool: PgPool,
}

impl QueryExecutor {
    pub(crate) fn new(ingestion_pool: &PgPool) -> Self {
        Self {
            pool: PgPoolOptions::new()
                .max_connections(4)
                .acquire_timeout(Duration::from_millis(500))
                .connect_lazy_with(ingestion_pool.connect_options().as_ref().clone()),
        }
    }
    pub(crate) async fn run(
        &self,
        request: StructuredQuery,
    ) -> Result<Value, StructuredQueryError> {
        crate::storage::run_structured_query(&self.pool, request).await
    }

    pub(crate) async fn note_context(
        &self,
        archive: &crate::config::ArchiveConfig,
        options: &crate::storage::note_index::NoteIndexOptions,
        id: Uuid,
    ) -> Result<Option<crate::storage::note_context::NoteContext>, StructuredQueryError> {
        let _permit = QUERY_SLOTS
            .get_or_init(|| Semaphore::new(8))
            .try_acquire()
            .map_err(|_| StructuredQueryError::Busy)?;
        tokio::time::timeout(
            QUERY_DEADLINE,
            crate::storage::get_note_context(&self.pool, archive, options, id),
        )
        .await
        .map_err(|_| StructuredQueryError::Timeout)?
        .map_err(StructuredQueryError::execution)
    }
}

#[derive(Serialize)]
pub(super) struct QueryBudget {
    concurrency: usize,
    read_pool_connections: u32,
    connection_wait_ms: u64,
    deadline_ms: u64,
    statement_timeout_ms: u64,
    max_response_bytes: usize,
}

pub(super) fn budget() -> QueryBudget {
    QueryBudget {
        concurrency: 8,
        read_pool_connections: 4,
        connection_wait_ms: 500,
        deadline_ms: QUERY_DEADLINE.as_millis() as u64,
        statement_timeout_ms: 4000,
        max_response_bytes: MAX_RESPONSE_BYTES,
    }
}

struct ValidatedQuery {
    dataset: &'static DatasetSpec,
    selected: Vec<QueryField>,
    order: Vec<(QueryField, SortDirection)>,
    filters: Vec<QueryFilter>,
    note_filter: SessionNoteFilter,
    limit: i64,
    offset: i64,
    cursor: Option<String>,
    cursor_sort: Option<CursorSort>,
}

impl ValidatedQuery {
    fn try_new(request: StructuredQuery) -> Result<Self, StructuredQueryError> {
        let dataset =
            super::dataset_spec(&request.dataset).map_err(StructuredQueryError::invalid)?;
        let selected = super::selected_fields(dataset, request.fields.as_deref())
            .map_err(StructuredQueryError::invalid)?;
        super::validate_structured_query_filters(&request.filters)
            .map_err(StructuredQueryError::invalid)?;
        let note_filter =
            SessionNoteFilter::try_new(&request.filters).map_err(StructuredQueryError::invalid)?;
        if !note_filter.is_empty() && dataset.name != "sessions" {
            return Err(StructuredQueryError::invalid(
                "related prompt tag filters require sessions",
            ));
        }
        // Validate all legacy scalar filters before obtaining a connection.
        let mut validation = QueryBuilder::<Postgres>::new("");
        for filter in &request.filters {
            if super::session_note_filter::related_field(&filter.field).is_none() {
                super::append_filter(&mut validation, dataset, filter)
                    .map_err(StructuredQueryError::invalid)?;
            }
        }
        let mut order = super::selected_order(dataset, &request.order_by)
            .map_err(StructuredQueryError::invalid)?;
        let cursor_sort = match (dataset.name, order.as_slice()) {
            ("sessions", [(field, SortDirection::Desc)]) if field.name() == "last_seen" => {
                Some(CursorSort::SessionsDesc)
            }
            ("prompt_tags" | "prompt_notes", [(field, SortDirection::Desc)])
                if field.name() == "observed_at" =>
            {
                Some(CursorSort::NotesDesc)
            }
            _ => None,
        };
        let unique = match dataset.name {
            "rollups_minute" => "bucket",
            _ => "id",
        };
        if matches!(dataset.name, "prompt_tags" | "prompt_notes") && cursor_sort.is_some() {
            order.push((
                super::query_field(dataset, "request_id").map_err(StructuredQueryError::invalid)?,
                SortDirection::Desc,
            ));
        }
        if !order.iter().any(|(field, _)| field.name() == unique) {
            order.push((
                super::query_field(dataset, unique).map_err(StructuredQueryError::invalid)?,
                SortDirection::Desc,
            ));
        }
        let offset = request.offset.unwrap_or(0);
        if !(0..=1_000_000).contains(&offset) {
            return Err(StructuredQueryError::invalid("offset must be 0..=1000000"));
        }
        if let Some(cursor) = &request.cursor
            && (cursor.len() > 4096 || cursor_sort.is_none() || offset != 0)
        {
            return Err(StructuredQueryError::invalid(
                "cursor requires an advertised sort and zero offset",
            ));
        }
        Ok(Self {
            dataset,
            selected,
            order,
            filters: request.filters,
            note_filter,
            limit: request.limit.unwrap_or(100).clamp(1, 500),
            offset,
            cursor: request.cursor,
            cursor_sort,
        })
    }

    fn fingerprint(
        &self,
        version: NoteIndexVersion,
        tag_revision: i64,
    ) -> Result<String, StructuredQueryError> {
        let canonical = json!({"dataset":self.dataset.name, "fields":self.selected.iter().map(QueryField::name).collect::<Vec<_>>(),
            "filters":self.filters, "order":self.order.iter().map(|(field, direction)| (field.name(), direction.as_str())).collect::<Vec<_>>(), "limit":self.limit, "version":i32::from(version), "tag_revision":tag_revision});
        Ok(hex::encode(Sha256::digest(
            serde_json::to_vec(&canonical).map_err(StructuredQueryError::execution)?,
        )))
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
enum CursorSort {
    SessionsDesc,
    NotesDesc,
}

#[derive(Serialize, Deserialize)]
struct CursorKey {
    observed_at: DateTime<Utc>,
    request_id: Option<Uuid>,
    id: Uuid,
}

#[derive(Serialize, Deserialize)]
struct CursorEnvelope {
    dataset: String,
    fingerprint: String,
    index_version: NoteIndexVersion,
    sort: CursorSort,
    key: CursorKey,
}

#[derive(Serialize)]
struct QueryPage {
    has_more: bool,
    next_cursor: Option<String>,
    offset: i64,
    row_ids: Vec<Uuid>,
}

#[derive(Serialize)]
struct SearchCoverage {
    scope: &'static str,
    index_version: NoteIndexVersion,
    coverage: &'static str,
    coverage_checked_at: Option<DateTime<Utc>>,
    coverage_counts_exact: bool,
    coverage_count_scope: &'static str,
    counts: Option<CoverageCounts>,
    warnings: Vec<&'static str>,
}

#[derive(Serialize, sqlx::FromRow)]
struct CoverageCounts {
    pending: i64,
    processing: i64,
    ready: i64,
    partial: i64,
    unsupported: i64,
    failed: i64,
    missing_state: i64,
    missing_source: i64,
}

pub(super) async fn run(
    pool: &PgPool,
    request: StructuredQuery,
) -> Result<Value, StructuredQueryError> {
    let query = ValidatedQuery::try_new(request)?;
    let _permit = QUERY_SLOTS
        .get_or_init(|| Semaphore::new(8))
        .try_acquire()
        .map_err(|_| StructuredQueryError::Busy)?;
    tokio::time::timeout(QUERY_DEADLINE, execute(pool, query, QueryPurpose::Rows))
        .await
        .map_err(|_| StructuredQueryError::Timeout)?
}

fn database_error(error: sqlx::Error) -> StructuredQueryError {
    StructuredQueryError::execution(error)
}

enum QueryPurpose {
    Rows,
    #[cfg(test)]
    Explain(ExplainPlanMode),
}

#[cfg(test)]
#[derive(Clone, Copy)]
pub(in crate::storage) enum ExplainPlanMode {
    Custom,
    Generic,
}

#[cfg(test)]
pub(in crate::storage) async fn explain(
    pool: &PgPool,
    request: StructuredQuery,
    mode: ExplainPlanMode,
) -> Result<Value, StructuredQueryError> {
    execute(
        pool,
        ValidatedQuery::try_new(request)?,
        QueryPurpose::Explain(mode),
    )
    .await
}

async fn execute(
    pool: &PgPool,
    mut query: ValidatedQuery,
    purpose: QueryPurpose,
) -> Result<Value, StructuredQueryError> {
    let mut tx = pool.begin().await.map_err(database_error)?;
    sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY")
        .execute(&mut *tx)
        .await
        .map_err(database_error)?;
    sqlx::query("SET LOCAL statement_timeout = '4s'")
        .execute(&mut *tx)
        .await
        .map_err(database_error)?;
    let plan_mode = match purpose {
        QueryPurpose::Rows if !query.note_filter.is_empty() => {
            Some("SET LOCAL plan_cache_mode = 'force_custom_plan'")
        }
        #[cfg(test)]
        QueryPurpose::Explain(ExplainPlanMode::Custom) => {
            Some("SET LOCAL plan_cache_mode = 'force_custom_plan'")
        }
        #[cfg(test)]
        QueryPurpose::Explain(ExplainPlanMode::Generic) => {
            Some("SET LOCAL plan_cache_mode = 'force_generic_plan'")
        }
        _ => None,
    };
    if let Some(plan_mode) = plan_mode {
        sqlx::query(plan_mode)
            .execute(&mut *tx)
            .await
            .map_err(database_error)?;
    }
    let (version, revision): (i32, i64) = sqlx::query_as(
        "SELECT active_version, tag_revision FROM note_index_settings WHERE singleton",
    )
    .fetch_one(&mut *tx)
    .await
    .map_err(database_error)?;
    let version =
        NoteIndexVersion::try_from(version).map_err(|_| StructuredQueryError::Unavailable)?;
    query
        .note_filter
        .resolve_values(&mut tx, version)
        .await
        .map_err(StructuredQueryError::execution)?;
    let tag_query = matches!(query.dataset.name, "prompt_tags" | "prompt_notes")
        || !query.note_filter.is_empty();
    let fingerprint = query.fingerprint(version, if tag_query { revision } else { 0 })?;
    let secret: Vec<u8> = sqlx::query_scalar("SELECT secret FROM query_cursor_key WHERE singleton")
        .fetch_one(&mut *tx)
        .await
        .map_err(database_error)?;
    let cursor = query
        .cursor
        .as_deref()
        .map(|token| decode_cursor(token, &secret, &query, &fingerprint, version))
        .transpose()?;
    let mut builder = QueryBuilder::<Postgres>::new("SELECT jsonb_build_object(");
    // JSON keys are bound values, never SQL aliases; PostgreSQL's identifier
    // truncation cannot collide even for maximum-length plugin paths.
    for (index, field) in query.selected.iter().enumerate() {
        if index > 0 && index % 32 == 0 {
            // PostgreSQL limits a function call to 100 arguments. Keep the
            // existing 64-field API limit using bounded JSON object chunks.
            builder.push(") || jsonb_build_object(");
        } else if index > 0 {
            builder.push(", ");
        }
        builder.push_bind(field.name().to_owned()).push("::text, ");
        field.append_sql(&mut builder);
    }
    builder.push(") AS data");
    match query.cursor_sort {
        Some(CursorSort::SessionsDesc) => {
            builder
                .push(", last_seen AS cursor_time, NULL::uuid AS cursor_request, id AS cursor_id");
        }
        Some(CursorSort::NotesDesc) => {
            builder.push(
                ", observed_at AS cursor_time, request_id AS cursor_request, id AS cursor_id",
            );
        }
        None => {}
    }
    builder
        .push(" FROM ")
        .push(query.dataset.relation)
        .push(" WHERE true");
    if matches!(query.dataset.name, "prompt_tags" | "prompt_notes") {
        builder
            .push(" AND index_version = ")
            .push_bind(i32::from(version))
            .push(" AND tag_revision = ")
            .push_bind(revision);
    }
    for filter in &query.filters {
        if super::session_note_filter::related_field(&filter.field).is_some() {
            continue;
        }
        builder.push(" AND ");
        if matches!(query.dataset.name, "prompt_tags" | "prompt_notes")
            && matches!(filter.field.as_str(), "note_value" | "tag_value")
            && matches!(filter.op, super::QueryOp::Eq)
            && let Some(value) = filter.value.as_ref().and_then(Value::as_str)
        {
            builder
                .push("note_value_sha256 = ")
                .push_bind(
                    crate::storage::message_contents::ContentHash::of(value)
                        .0
                        .to_vec(),
                )
                .push(" AND ");
        }
        super::append_filter(&mut builder, query.dataset, filter)
            .map_err(StructuredQueryError::invalid)?;
    }
    if !query.note_filter.is_empty() {
        builder.push(" AND ");
        query
            .note_filter
            .append_sql(&mut builder, version, revision);
    }
    if let Some(cursor) = cursor {
        match cursor.sort {
            CursorSort::SessionsDesc => {
                builder
                    .push(" AND (last_seen, id) < (")
                    .push_bind(cursor.key.observed_at)
                    .push(", ")
                    .push_bind(cursor.key.id)
                    .push(")");
            }
            CursorSort::NotesDesc => {
                builder
                    .push(" AND (observed_at, request_id, id) < (")
                    .push_bind(cursor.key.observed_at)
                    .push(", ")
                    .push_bind(cursor.key.request_id.ok_or_else(|| {
                        StructuredQueryError::invalid("invalid prompt tag cursor key")
                    })?)
                    .push(", ")
                    .push_bind(cursor.key.id)
                    .push(")");
            }
        }
    }
    builder.push(" ORDER BY ");
    for (index, (field, direction)) in query.order.iter().enumerate() {
        if index > 0 {
            builder.push(", ");
        }
        field.append_sql(&mut builder);
        builder.push(match direction {
            SortDirection::Asc => " ASC",
            SortDirection::Desc => " DESC",
        });
    }
    builder
        .push(" LIMIT ")
        .push_bind(query.limit + 1)
        .push(" OFFSET ")
        .push_bind(query.offset);
    #[cfg(test)]
    if let QueryPurpose::Explain(mode) = purpose {
        let plan = explain_prepared(&mut tx, &mut builder, mode).await?;
        tx.commit().await.map_err(database_error)?;
        return Ok(plan);
    }
    let statement = builder.build();
    let mut records = statement.fetch(&mut *tx);
    let mut has_more = false;
    let mut rows = Vec::with_capacity(query.limit as usize);
    let mut last_key = None;
    let mut row_ids = Vec::new();
    let mut bytes = 0;
    // Enforce the response budget as rows arrive, rather than first buffering
    // an entire page of potentially large plugin metadata in proxy memory.
    while let Some(row) = records.try_next().await.map_err(database_error)? {
        if rows.len() == query.limit as usize {
            has_more = true;
            break;
        }
        let data: Value = row.try_get("data").map_err(database_error)?;
        if query.dataset.name == "messages" && data.get("content") == Some(&Value::Null) {
            return Err(StructuredQueryError::execution(anyhow::anyhow!(
                "message preview dictionary content is missing"
            )));
        }
        bytes += serde_json::to_vec(&data)
            .map_err(StructuredQueryError::execution)?
            .len();
        if bytes > MAX_RESPONSE_BYTES {
            return Err(StructuredQueryError::ResponseTooLarge);
        }
        rows.push(data);
        if query.cursor_sort.is_some() {
            last_key = Some(CursorKey {
                observed_at: row.try_get("cursor_time").map_err(database_error)?,
                request_id: row.try_get("cursor_request").map_err(database_error)?,
                id: row.try_get("cursor_id").map_err(database_error)?,
            });
            row_ids.push(row.try_get("cursor_id").map_err(database_error)?);
        }
    }
    drop(records);
    let next_cursor = match (has_more, query.cursor_sort, last_key) {
        (true, Some(sort), Some(key)) => Some(encode_cursor(
            CursorEnvelope {
                dataset: query.dataset.name.to_owned(),
                fingerprint,
                index_version: version,
                sort,
                key,
            },
            &secret,
        )?),
        _ => None,
    };
    let search = if tag_query {
        let summary = sqlx::query("SELECT checked_at, pending, processing, ready, partial, unsupported, failed, missing_state, missing_source FROM note_index_coverage WHERE index_version = $1 AND tag_revision = $2")
            .bind(i32::from(version)).bind(revision).fetch_optional(&mut *tx).await.map_err(database_error)?;
        let checked_at = summary
            .as_ref()
            .map(|row| row.try_get::<DateTime<Utc>, _>("checked_at"))
            .transpose()
            .map_err(database_error)?;
        let counts = summary
            .as_ref()
            .map(|row| {
                use sqlx::FromRow;
                CoverageCounts::from_row(row)
            })
            .transpose()
            .map_err(database_error)?;
        Some(SearchCoverage {
            scope: if query.dataset.name == "prompt_notes"
                || query
                    .filters
                    .iter()
                    .any(|f| f.field.starts_with("prompt_note."))
            {
                "captured_request_prompt_notes"
            } else {
                "captured_request_prompt_tags"
            },
            index_version: version,
            coverage: "unknown",
            coverage_checked_at: checked_at,
            coverage_counts_exact: false,
            coverage_count_scope: "all_retained_requests_maintenance_snapshot",
            counts,
            warnings: vec![
                "Coverage is a maintenance snapshot of retained requests; zero matches do not prove absence.",
                "Results are live; late indexing, new requests and retention can change later pages.",
            ],
        })
    } else {
        None
    };
    tx.commit().await.map_err(database_error)?;
    let result = json!({"dataset":query.dataset.name, "fields":query.selected.iter().map(QueryField::name).collect::<Vec<_>>(), "rows":rows, "limit":query.limit,
        "page":QueryPage { has_more, next_cursor, offset:query.offset, row_ids }, "search":search });
    if serde_json::to_vec(&result)
        .map_err(StructuredQueryError::execution)?
        .len()
        > MAX_RESPONSE_BYTES
    {
        return Err(StructuredQueryError::ResponseTooLarge);
    }
    Ok(result)
}

#[cfg(test)]
async fn explain_prepared(
    tx: &mut sqlx::Transaction<'_, Postgres>,
    builder: &mut QueryBuilder<'_, Postgres>,
    mode: ExplainPlanMode,
) -> Result<Value, StructuredQueryError> {
    use sqlx::{Arguments, Execute, Executor};

    let mut query = builder.build();
    let sql = query.sql().to_owned();
    let arguments = query
        .take_arguments()
        .map_err(|error| StructuredQueryError::execution(anyhow::Error::from_boxed(error)))?
        .ok_or_else(|| {
            StructuredQueryError::execution(anyhow::anyhow!("missing query arguments"))
        })?;
    (&mut **tx).prepare(&sql).await.map_err(database_error)?;
    let statement = sqlx::query("SELECT name, ARRAY(SELECT format_type(t::oid, NULL) FROM unnest(parameter_types) t) AS types,
        generic_plans, custom_plans FROM pg_prepared_statements WHERE statement = $1 ORDER BY prepare_time DESC LIMIT 1")
        .bind(&sql).fetch_one(&mut **tx).await.map_err(database_error)?;
    let name: String = statement.try_get("name").map_err(database_error)?;
    let types: Vec<String> = statement.try_get("types").map_err(database_error)?;
    let before_custom: i64 = statement.try_get("custom_plans").map_err(database_error)?;
    let before_generic: i64 = statement.try_get("generic_plans").map_err(database_error)?;
    // Use the very same prepared SELECT as the production compiler. Preparing
    // an EXPLAIN wrapper alone does not prove that its inner SELECT is generic.
    if types.len() != arguments.len() {
        return Err(StructuredQueryError::execution(anyhow::anyhow!(
            "prepared parameter mapping changed"
        )));
    }
    // EXECUTE is a utility statement and cannot take extended-protocol binds.
    // Let PostgreSQL quote the original bound values, using only server-rendered
    // parameter types. No test input is interpolated as a SQL literal in Rust.
    let quoted = types
        .iter()
        .enumerate()
        .map(|(index, kind)| format!("quote_nullable(${}::{kind})", index + 1))
        .collect::<Vec<_>>()
        .join(", ");
    let quote_sql = format!("SELECT array_to_string(ARRAY[{quoted}], ', ')");
    let parameters: String = sqlx::query_scalar_with(&quote_sql, arguments)
        .fetch_one(&mut **tx)
        .await
        .map_err(database_error)?;
    let explain = format!(
        "EXPLAIN (ANALYZE, BUFFERS, FORMAT JSON) EXECUTE \"{}\"({parameters})",
        name.replace('"', "\"\"")
    );
    let mut plan: Value = sqlx::query_scalar(&explain)
        .fetch_one(&mut **tx)
        .await
        .map_err(database_error)?;
    let counts = sqlx::query(
        "SELECT generic_plans, custom_plans FROM pg_prepared_statements WHERE name = $1",
    )
    .bind(&name)
    .fetch_one(&mut **tx)
    .await
    .map_err(database_error)?;
    let custom = counts
        .try_get::<i64, _>("custom_plans")
        .map_err(database_error)?
        - before_custom;
    let generic = counts
        .try_get::<i64, _>("generic_plans")
        .map_err(database_error)?
        - before_generic;
    let expected = match mode {
        ExplainPlanMode::Custom => (1, 0),
        ExplainPlanMode::Generic => (0, 1),
    };
    if (custom, generic) != expected {
        return Err(StructuredQueryError::execution(anyhow::anyhow!(
            "EXPLAIN did not execute the requested prepared plan mode"
        )));
    }
    plan[0]["Prepared Plan Counters"] = json!({"custom_plans":custom, "generic_plans":generic});
    Ok(plan)
}

fn encode_cursor(cursor: CursorEnvelope, secret: &[u8]) -> Result<String, StructuredQueryError> {
    let payload = serde_json::to_vec(&cursor).map_err(StructuredQueryError::execution)?;
    let mut mac =
        Hmac::<Sha256>::new_from_slice(secret).map_err(StructuredQueryError::execution)?;
    mac.update(&payload);
    Ok(format!(
        "{}.{}",
        URL_SAFE_NO_PAD.encode(&payload),
        URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes())
    ))
}

fn decode_cursor(
    token: &str,
    secret: &[u8],
    query: &ValidatedQuery,
    fingerprint: &str,
    version: NoteIndexVersion,
) -> Result<CursorEnvelope, StructuredQueryError> {
    let invalid = || {
        StructuredQueryError::invalid(
            "cursor is invalid or belongs to another query/version; run the query again",
        )
    };
    let (payload, signature) = token.split_once('.').ok_or_else(invalid)?;
    let payload = URL_SAFE_NO_PAD.decode(payload).map_err(|_| invalid())?;
    let signature = URL_SAFE_NO_PAD.decode(signature).map_err(|_| invalid())?;
    let mut mac =
        Hmac::<Sha256>::new_from_slice(secret).map_err(StructuredQueryError::execution)?;
    mac.update(&payload);
    mac.verify_slice(&signature).map_err(|_| invalid())?;
    let cursor: CursorEnvelope = serde_json::from_slice(&payload).map_err(|_| invalid())?;
    if cursor.dataset != query.dataset.name
        || cursor.fingerprint != fingerprint
        || cursor.index_version != version
        || Some(cursor.sort) != query.cursor_sort
    {
        return Err(invalid());
    }
    Ok(cursor)
}

#[cfg(test)]
mod tests;
