//! Allowlisted structured queries and schema discovery.
use super::*;
mod execution;
mod session_note_filter;

pub(crate) use execution::QueryExecutor;
#[cfg(test)]
pub(super) use execution::{ExplainPlanMode, explain};

pub(super) const MAX_STRUCTURED_QUERY_FIELDS: usize = 64;
pub(super) const MAX_STRUCTURED_QUERY_FILTERS: usize = 32;
pub(super) const MAX_STRUCTURED_QUERY_ORDER_BY: usize = 8;
pub(super) const MAX_STRUCTURED_QUERY_STRING_VALUE_BYTES: usize = 4 * 1024;
pub(super) const MAX_STRUCTURED_QUERY_JSON_VALUE_BYTES: usize = 16 * 1024;
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct StructuredQuery {
    pub dataset: String,
    pub fields: Option<Vec<String>>,
    #[serde(default)]
    pub filters: Vec<QueryFilter>,
    #[serde(default)]
    pub order_by: Vec<QueryOrder>,
    pub limit: Option<i64>,
    pub cursor: Option<String>,
    pub offset: Option<i64>,
}

#[derive(Debug, thiserror::Error)]
pub enum StructuredQueryError {
    #[error("{0}")]
    Invalid(String),
    #[error(transparent)]
    Execution(#[from] anyhow::Error),
    #[error("query capacity exhausted")]
    Busy,
    #[error("query dependency unavailable")]
    Unavailable,
    #[error("query deadline exceeded")]
    Timeout,
    #[error("query response byte budget exceeded")]
    ResponseTooLarge,
}

impl StructuredQueryError {
    fn invalid(error: impl std::fmt::Display) -> Self {
        Self::Invalid(error.to_string())
    }

    fn execution(error: impl Into<anyhow::Error>) -> Self {
        let error = error.into();
        // Helpers can add context before reaching the query boundary. Classify
        // the underlying SQLx error here as well as direct execution failures.
        match error.downcast_ref::<sqlx::Error>() {
            Some(
                sqlx::Error::PoolTimedOut
                | sqlx::Error::PoolClosed
                | sqlx::Error::Io(_)
                | sqlx::Error::Tls(_),
            ) => Self::Unavailable,
            Some(sqlx::Error::Database(source)) if source.code().as_deref() == Some("57014") => {
                Self::Timeout
            }
            _ => Self::Execution(error),
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct QueryFilter {
    pub field: String,
    pub op: QueryOp,
    pub value: Option<Value>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct QueryOrder {
    pub field: String,
    #[serde(default)]
    pub direction: SortDirection,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum QueryOp {
    Eq,
    Ne,
    Contains,
    Gt,
    Gte,
    Lt,
    Lte,
    IsNull,
    IsNotNull,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SortDirection {
    Asc,
    #[default]
    Desc,
}

impl SortDirection {
    fn as_str(self) -> &'static str {
        match self {
            Self::Asc => "asc",
            Self::Desc => "desc",
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub(super) struct DatasetSpec {
    name: &'static str,
    relation: &'static str,
    fields: &'static [FieldSpec],
    default_fields: &'static [&'static str],
    default_order: &'static [DefaultOrder],
}

#[derive(Debug, Clone, Copy)]
pub(super) struct FieldSpec {
    name: &'static str,
    filter: Option<FilterKind>,
}

const fn field(name: &'static str, filter: Option<FilterKind>) -> FieldSpec {
    FieldSpec { name, filter }
}

#[derive(Debug, Clone)]
pub(super) enum QueryField {
    Static(&'static FieldSpec),
    PluginMetadataPath { name: String, path: Vec<String> },
}

#[derive(Debug, Clone, Copy)]
pub(super) struct DefaultOrder {
    field: &'static str,
    direction: SortDirection,
}

#[derive(Debug, Clone, Copy)]
pub(super) enum FilterKind {
    Text,
    Int,
    Bool,
    Timestamp,
    Uuid,
    Json,
    JsonPath,
    TextArray,
}

impl FilterKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::Int => "int",
            Self::Bool => "bool",
            Self::Timestamp => "timestamp",
            Self::Uuid => "uuid",
            Self::Json => "json",
            Self::JsonPath => "json_path",
            Self::TextArray => "text_array",
        }
    }

    fn operators(self) -> &'static [&'static str] {
        match self {
            Self::Text => &[
                "eq",
                "ne",
                "contains",
                "gt",
                "gte",
                "lt",
                "lte",
                "is_null",
                "is_not_null",
            ],
            Self::Int | Self::Timestamp | Self::Uuid => &[
                "eq",
                "ne",
                "gt",
                "gte",
                "lt",
                "lte",
                "is_null",
                "is_not_null",
            ],
            Self::Bool => &["eq", "ne", "is_null", "is_not_null"],
            Self::Json | Self::JsonPath => &["eq", "ne", "contains", "is_null", "is_not_null"],
            Self::TextArray => &["contains", "is_null", "is_not_null"],
        }
    }
}

impl QueryField {
    fn name(&self) -> &str {
        match self {
            Self::Static(field) => field.name,
            Self::PluginMetadataPath { name, .. } => name,
        }
    }

    fn filter(&self) -> Option<FilterKind> {
        match self {
            Self::Static(field) => field.filter,
            Self::PluginMetadataPath { .. } => Some(FilterKind::JsonPath),
        }
    }

    fn append_sql(&self, builder: &mut QueryBuilder<'_, Postgres>) {
        match self {
            Self::Static(field) => {
                builder.push(field.name);
            }
            Self::PluginMetadataPath { path, .. } => {
                builder.push("(plugin_metadata #> ");
                builder.push_bind(path.clone());
                builder.push(")");
            }
        }
    }
}

pub fn structured_query_schema() -> Value {
    json!({
        "execution_budget": execution::budget(),
        "limits": {
            "max_fields": MAX_STRUCTURED_QUERY_FIELDS,
            "max_filters": MAX_STRUCTURED_QUERY_FILTERS,
            "max_order_by": MAX_STRUCTURED_QUERY_ORDER_BY,
            "max_limit": 500,
            "max_string_value_bytes": MAX_STRUCTURED_QUERY_STRING_VALUE_BYTES,
            "max_json_value_bytes": MAX_STRUCTURED_QUERY_JSON_VALUE_BYTES,
        },
        "sort_directions": ["asc", "desc"],
        "plugin_metadata": {
            "dataset": "requests",
            "field_prefix": PLUGIN_METADATA_FIELD_PREFIX,
            "filter_kind": FilterKind::JsonPath.as_str(),
            "operators": FilterKind::JsonPath.operators(),
            "max_segments": MAX_PLUGIN_METADATA_PATH_SEGMENTS,
            "max_segment_bytes": MAX_PLUGIN_METADATA_PATH_SEGMENT_LEN,
            "segment_pattern": "[A-Za-z0-9_-]+",
        },
        "datasets": DATASETS
            .iter()
            .filter(|dataset| dataset.name != "prompt_notes")
            .map(dataset_schema)
            .collect::<Vec<_>>(),
    })
}

pub(super) fn dataset_schema(dataset: &DatasetSpec) -> Value {
    json!({
        "name": dataset.name,
        "default_fields": dataset.default_fields,
        "default_order": dataset
            .default_order
            .iter()
            .map(default_order_schema)
            .collect::<Vec<_>>(),
        "fields": dataset
            .fields
            .iter()
            .filter(|f| dataset.name != "prompt_tags" || f.name != "note_value")
            .map(field_schema)
            .collect::<Vec<_>>(),
        "related_filter_fields": if dataset.name == "sessions" { session_note_filter::schema() } else { json!([]) },
        "cursor_orders": match dataset.name {
            "sessions" => json!([[{"field":"last_seen", "direction":"desc"}]]),
            "prompt_notes" | "prompt_tags" => json!([[{"field":"observed_at", "direction":"desc"}]]),
            _ => json!([]),
        },
    })
}

pub(super) fn field_schema(field: &FieldSpec) -> Value {
    json!({
        "name": field.name,
        "filter_kind": field.filter.map(|kind| kind.as_str()),
        "operators": field
            .filter
            .map(|kind| kind.operators())
            .unwrap_or_default(),
    })
}

pub(super) fn default_order_schema(order: &DefaultOrder) -> Value {
    json!({
        "field": order.field,
        "direction": order.direction.as_str(),
    })
}

static REQUEST_FIELDS: &[FieldSpec] = &[
    field("ttfb_ms", Some(FilterKind::Int)),
    field("input_tokens", Some(FilterKind::Int)),
    field("output_tokens", Some(FilterKind::Int)),
    field("cached_input_tokens", Some(FilterKind::Int)),
    field("cache_creation_input_tokens", Some(FilterKind::Int)),
    field("estimated_cost_microusd", Some(FilterKind::Int)),
    field("tool_call_count", Some(FilterKind::Int)),
    field("usage_complete", Some(FilterKind::Bool)),
    field("id", Some(FilterKind::Uuid)),
    field("started_at", Some(FilterKind::Timestamp)),
    field("completed_at", Some(FilterKind::Timestamp)),
    field("method", Some(FilterKind::Text)),
    field("original_uri", Some(FilterKind::Text)),
    field("upstream_url", Some(FilterKind::Text)),
    field("upstream_host", Some(FilterKind::Text)),
    field("status", Some(FilterKind::Int)),
    field("error", Some(FilterKind::Text)),
    field("request_kind", Some(FilterKind::Text)),
    field("model", Some(FilterKind::Text)),
    field("api_key_hash", Some(FilterKind::Text)),
    field("session_key", Some(FilterKind::Text)),
    field("session_id", Some(FilterKind::Uuid)),
    field("ttft_ms", Some(FilterKind::Int)),
    field("duration_ms", Some(FilterKind::Int)),
    field("bytes_in", Some(FilterKind::Int)),
    field("bytes_out", Some(FilterKind::Int)),
    field("request_headers", Some(FilterKind::Json)),
    field("response_headers", Some(FilterKind::Json)),
    field("request_body_bytes", Some(FilterKind::Int)),
    field("response_body_bytes", Some(FilterKind::Int)),
    field("request_body_truncated", Some(FilterKind::Bool)),
    field("response_body_truncated", Some(FilterKind::Bool)),
    field("content_type", Some(FilterKind::Text)),
    field("plugin_metadata", Some(FilterKind::Json)),
    field("tags", Some(FilterKind::TextArray)),
];

static SESSION_FIELDS: &[FieldSpec] = &[
    field("id", Some(FilterKind::Uuid)),
    field("session_key", Some(FilterKind::Text)),
    field("first_seen", Some(FilterKind::Timestamp)),
    field("last_seen", Some(FilterKind::Timestamp)),
    field("user_id", Some(FilterKind::Text)),
    field("user_name", Some(FilterKind::Text)),
    field("summary", Some(FilterKind::Json)),
];

static MESSAGE_FIELDS: &[FieldSpec] = &[
    field("id", Some(FilterKind::Int)),
    field("request_id", Some(FilterKind::Uuid)),
    field("session_id", Some(FilterKind::Uuid)),
    field("role", Some(FilterKind::Text)),
    field("content", Some(FilterKind::Text)),
    field("content_truncated", Some(FilterKind::Bool)),
    field("created_at", Some(FilterKind::Timestamp)),
];

static ROLLUP_FIELDS: &[FieldSpec] = &[
    field("bucket", Some(FilterKind::Timestamp)),
    field("last_seen", Some(FilterKind::Timestamp)),
    field("total", Some(FilterKind::Int)),
    field("errors", Some(FilterKind::Int)),
    field("captured_bytes", Some(FilterKind::Int)),
    field("duration_count", Some(FilterKind::Int)),
    field("duration_sum_ms", Some(FilterKind::Int)),
    field("ttft_count", Some(FilterKind::Int)),
    field("ttft_sum_ms", Some(FilterKind::Int)),
];

static PROMPT_NOTE_FIELDS: &[FieldSpec] = &[
    field("id", Some(FilterKind::Uuid)),
    field("request_id", Some(FilterKind::Uuid)),
    field("session_id", Some(FilterKind::Uuid)),
    field("observed_at", Some(FilterKind::Timestamp)),
    field("role", Some(FilterKind::Text)),
    field("source_path", Some(FilterKind::Text)),
    field("source_kind", Some(FilterKind::Text)),
    field("note_value", Some(FilterKind::Text)),
    field("tag_value", Some(FilterKind::Text)),
    field("tag_name", Some(FilterKind::Text)),
    field("index_version", Some(FilterKind::Int)),
    field("model", Some(FilterKind::Text)),
    field("request_kind", Some(FilterKind::Text)),
    field("api_key_hash", Some(FilterKind::Text)),
    field("user_id", Some(FilterKind::Text)),
    field("user_name", Some(FilterKind::Text)),
    field("capture_complete", Some(FilterKind::Bool)),
    field("external_history", Some(FilterKind::Bool)),
    field("index_state", Some(FilterKind::Text)),
];

static DATASETS: &[DatasetSpec] = &[
    DatasetSpec {
        name: "prompt_tags",
        relation: "trace_prompt_tags",
        fields: PROMPT_NOTE_FIELDS,
        default_fields: &[
            "id",
            "request_id",
            "session_id",
            "observed_at",
            "role",
            "tag_name",
            "tag_value",
            "source_path",
            "index_version",
            "capture_complete",
            "index_state",
        ],
        default_order: &[DefaultOrder {
            field: "observed_at",
            direction: SortDirection::Desc,
        }],
    },
    DatasetSpec {
        name: "prompt_notes",
        relation: "trace_prompt_notes",
        fields: PROMPT_NOTE_FIELDS,
        default_fields: &[
            "id",
            "request_id",
            "session_id",
            "observed_at",
            "role",
            "note_value",
            "source_path",
            "index_version",
            "capture_complete",
            "index_state",
        ],
        default_order: &[DefaultOrder {
            field: "observed_at",
            direction: SortDirection::Desc,
        }],
    },
    DatasetSpec {
        name: "requests",
        relation: "trace_requests",
        fields: REQUEST_FIELDS,
        default_fields: &[
            "id",
            "started_at",
            "method",
            "original_uri",
            "upstream_host",
            "status",
            "request_kind",
            "model",
            "duration_ms",
            "bytes_in",
            "bytes_out",
            "tags",
        ],
        default_order: &[DefaultOrder {
            field: "started_at",
            direction: SortDirection::Desc,
        }],
    },
    DatasetSpec {
        name: "sessions",
        relation: "trace_sessions",
        fields: SESSION_FIELDS,
        default_fields: &[
            "id",
            "session_key",
            "first_seen",
            "last_seen",
            "user_id",
            "user_name",
        ],
        default_order: &[DefaultOrder {
            field: "last_seen",
            direction: SortDirection::Desc,
        }],
    },
    DatasetSpec {
        name: "messages",
        relation: "trace_messages",
        fields: MESSAGE_FIELDS,
        default_fields: &[
            "id",
            "request_id",
            "session_id",
            "role",
            "content",
            "created_at",
        ],
        default_order: &[DefaultOrder {
            field: "created_at",
            direction: SortDirection::Desc,
        }],
    },
    DatasetSpec {
        name: "rollups_minute",
        relation: "trace_rollups_minute",
        fields: ROLLUP_FIELDS,
        default_fields: &[
            "bucket",
            "last_seen",
            "total",
            "errors",
            "captured_bytes",
            "duration_count",
            "duration_sum_ms",
            "ttft_count",
            "ttft_sum_ms",
        ],
        default_order: &[DefaultOrder {
            field: "bucket",
            direction: SortDirection::Desc,
        }],
    },
];

pub async fn run_structured_query(
    pool: &PgPool,
    request: StructuredQuery,
) -> Result<Value, StructuredQueryError> {
    execution::run(pool, request).await
}

pub(super) fn dataset_spec(name: &str) -> anyhow::Result<&'static DatasetSpec> {
    DATASETS
        .iter()
        .find(|dataset| dataset.name == name)
        .ok_or_else(|| anyhow::anyhow!("unknown query dataset {name}"))
}

pub(super) fn selected_fields(
    dataset: &'static DatasetSpec,
    requested: Option<&[String]>,
) -> anyhow::Result<Vec<QueryField>> {
    let names: Vec<&str> = match requested {
        Some([]) => anyhow::bail!("fields must not be empty"),
        Some(fields) => {
            if fields.len() > MAX_STRUCTURED_QUERY_FIELDS {
                anyhow::bail!(
                    "fields must contain at most {} entries",
                    MAX_STRUCTURED_QUERY_FIELDS
                );
            }
            fields.iter().map(String::as_str).collect()
        }
        None => dataset.default_fields.to_vec(),
    };

    let mut selected = Vec::with_capacity(names.len());
    for name in names {
        let field = query_field(dataset, name)?;
        if selected
            .iter()
            .any(|selected_field: &QueryField| selected_field.name() == field.name())
        {
            continue;
        }
        selected.push(field);
    }

    if selected.is_empty() {
        anyhow::bail!("at least one field must be selected");
    }
    Ok(selected)
}

pub(super) fn selected_order(
    dataset: &'static DatasetSpec,
    requested: &[QueryOrder],
) -> anyhow::Result<Vec<(QueryField, SortDirection)>> {
    if requested.len() > MAX_STRUCTURED_QUERY_ORDER_BY {
        anyhow::bail!(
            "order_by must contain at most {} entries",
            MAX_STRUCTURED_QUERY_ORDER_BY
        );
    }

    if requested.is_empty() {
        return dataset
            .default_order
            .iter()
            .map(|order| Ok((query_field(dataset, order.field)?, order.direction)))
            .collect();
    }

    requested
        .iter()
        .map(|order| Ok((query_field(dataset, &order.field)?, order.direction)))
        .collect()
}

pub(super) fn field_spec(
    dataset: &'static DatasetSpec,
    name: &str,
) -> anyhow::Result<&'static FieldSpec> {
    dataset
        .fields
        .iter()
        .find(|field| field.name == name)
        .ok_or_else(|| anyhow::anyhow!("field {name} is not allowed for dataset {}", dataset.name))
}

pub(super) fn query_field(dataset: &'static DatasetSpec, name: &str) -> anyhow::Result<QueryField> {
    if let Ok(field) = field_spec(dataset, name) {
        return Ok(QueryField::Static(field));
    }

    if dataset.name == "requests"
        && let Some(path) = plugin_metadata_path(name)?
    {
        return Ok(QueryField::PluginMetadataPath {
            name: name.to_string(),
            path,
        });
    }

    anyhow::bail!("field {name} is not allowed for dataset {}", dataset.name)
}

pub(super) fn validate_structured_query_filters(filters: &[QueryFilter]) -> anyhow::Result<()> {
    if filters.len() > MAX_STRUCTURED_QUERY_FILTERS {
        anyhow::bail!(
            "filters must contain at most {} entries",
            MAX_STRUCTURED_QUERY_FILTERS
        );
    }
    Ok(())
}

pub(super) const PLUGIN_METADATA_FIELD_PREFIX: &str = "plugin_metadata.";
pub(super) const MAX_PLUGIN_METADATA_PATH_SEGMENTS: usize = 16;
pub(super) const MAX_PLUGIN_METADATA_PATH_SEGMENT_LEN: usize = 64;

pub(super) fn plugin_metadata_path(name: &str) -> anyhow::Result<Option<Vec<String>>> {
    let Some(path) = name.strip_prefix(PLUGIN_METADATA_FIELD_PREFIX) else {
        return Ok(None);
    };
    if path.is_empty() {
        anyhow::bail!("plugin_metadata path must not be empty");
    }

    let segments: Vec<String> = path.split('.').map(str::to_string).collect();
    if segments.len() > MAX_PLUGIN_METADATA_PATH_SEGMENTS {
        anyhow::bail!(
            "plugin_metadata path must have at most {} segments",
            MAX_PLUGIN_METADATA_PATH_SEGMENTS
        );
    }

    for segment in &segments {
        if !valid_plugin_metadata_path_segment(segment) {
            anyhow::bail!(
                "plugin_metadata path segment {segment:?} must contain only ASCII letters, digits, '_' or '-'"
            );
        }
    }

    Ok(Some(segments))
}

pub(super) fn valid_plugin_metadata_path_segment(segment: &str) -> bool {
    !segment.is_empty()
        && segment.len() <= MAX_PLUGIN_METADATA_PATH_SEGMENT_LEN
        && segment
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
}

pub(super) fn append_filter(
    builder: &mut QueryBuilder<'_, Postgres>,
    dataset: &'static DatasetSpec,
    filter: &QueryFilter,
) -> anyhow::Result<()> {
    let field = query_field(dataset, &filter.field)?;
    let kind = field
        .filter()
        .ok_or_else(|| anyhow::anyhow!("field {} cannot be filtered", filter.field))?;

    match filter.op {
        QueryOp::IsNull => {
            append_null_filter(builder, &field, true);
            return Ok(());
        }
        QueryOp::IsNotNull => {
            append_null_filter(builder, &field, false);
            return Ok(());
        }
        QueryOp::Eq | QueryOp::Ne if filter.value.as_ref().is_some_and(Value::is_null) => {
            append_null_filter(builder, &field, matches!(filter.op, QueryOp::Eq));
            return Ok(());
        }
        _ => {}
    }

    let value = filter
        .value
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("filter {} requires a value", filter.field))?;

    match kind {
        FilterKind::Text => append_text_filter(builder, &field, filter.op, value),
        FilterKind::Int => append_int_filter(builder, &field, filter.op, value),
        FilterKind::Bool => append_bool_filter(builder, &field, filter.op, value),
        FilterKind::Timestamp => append_timestamp_filter(builder, &field, filter.op, value),
        FilterKind::Uuid => append_uuid_filter(builder, &field, filter.op, value),
        FilterKind::Json | FilterKind::JsonPath => {
            append_json_filter(builder, &field, filter.op, value)
        }
        FilterKind::TextArray => append_text_array_filter(builder, &field, filter.op, value),
    }
}

pub(super) fn append_null_filter(
    builder: &mut QueryBuilder<'_, Postgres>,
    field: &QueryField,
    is_null: bool,
) {
    match field {
        QueryField::PluginMetadataPath { .. } => {
            builder.push("(");
            field.append_sql(builder);
            builder.push(if is_null {
                " IS NULL OR "
            } else {
                " IS NOT NULL AND "
            });
            field.append_sql(builder);
            builder.push(if is_null {
                " = 'null'::jsonb"
            } else {
                " <> 'null'::jsonb"
            });
            builder.push(")");
        }
        QueryField::Static(_) => {
            field.append_sql(builder);
            builder.push(if is_null { " IS NULL" } else { " IS NOT NULL" });
        }
    }
}

pub(super) fn append_text_filter(
    builder: &mut QueryBuilder<'_, Postgres>,
    field: &QueryField,
    op: QueryOp,
    value: &Value,
) -> anyhow::Result<()> {
    let value = value_as_string(field.name(), value)?;
    match op {
        QueryOp::Eq | QueryOp::Ne | QueryOp::Gt | QueryOp::Gte | QueryOp::Lt | QueryOp::Lte => {
            field.append_sql(builder);
            builder.push(comparison_operator(op)?);
            builder.push_bind(value);
        }
        QueryOp::Contains => {
            field.append_sql(builder);
            builder.push(" ILIKE ");
            builder.push_bind(format!("%{}%", escape_like(&value)));
            builder.push(" ESCAPE '\\'");
        }
        QueryOp::IsNull | QueryOp::IsNotNull => unreachable!(),
    }
    Ok(())
}

pub(super) fn append_int_filter(
    builder: &mut QueryBuilder<'_, Postgres>,
    field: &QueryField,
    op: QueryOp,
    value: &Value,
) -> anyhow::Result<()> {
    let value = value_as_i64(field.name(), value)?;
    field.append_sql(builder);
    builder.push(comparison_operator(op)?);
    builder.push_bind(value);
    Ok(())
}

pub(super) fn append_bool_filter(
    builder: &mut QueryBuilder<'_, Postgres>,
    field: &QueryField,
    op: QueryOp,
    value: &Value,
) -> anyhow::Result<()> {
    let value = value_as_bool(field.name(), value)?;
    match op {
        QueryOp::Eq | QueryOp::Ne => {
            field.append_sql(builder);
            builder.push(comparison_operator(op)?);
            builder.push_bind(value);
            Ok(())
        }
        _ => anyhow::bail!(
            "operator {:?} is not supported for boolean field {}",
            op,
            field.name()
        ),
    }
}

pub(super) fn append_timestamp_filter(
    builder: &mut QueryBuilder<'_, Postgres>,
    field: &QueryField,
    op: QueryOp,
    value: &Value,
) -> anyhow::Result<()> {
    let value = value_as_timestamp(field.name(), value)?;
    field.append_sql(builder);
    builder.push(comparison_operator(op)?);
    builder.push_bind(value);
    Ok(())
}

pub(super) fn append_uuid_filter(
    builder: &mut QueryBuilder<'_, Postgres>,
    field: &QueryField,
    op: QueryOp,
    value: &Value,
) -> anyhow::Result<()> {
    let value = value_as_uuid(field.name(), value)?;
    match op {
        QueryOp::Eq | QueryOp::Ne | QueryOp::Gt | QueryOp::Gte | QueryOp::Lt | QueryOp::Lte => {
            field.append_sql(builder);
            builder.push(comparison_operator(op)?);
            builder.push_bind(value);
            Ok(())
        }
        _ => anyhow::bail!(
            "operator {:?} is not supported for uuid field {}",
            op,
            field.name()
        ),
    }
}

pub(super) fn append_json_filter(
    builder: &mut QueryBuilder<'_, Postgres>,
    field: &QueryField,
    op: QueryOp,
    value: &Value,
) -> anyhow::Result<()> {
    validate_json_filter_value_size(field.name(), value)?;
    match op {
        QueryOp::Eq | QueryOp::Ne => {
            field.append_sql(builder);
            builder.push(comparison_operator(op)?);
            builder.push_bind(value.clone());
            Ok(())
        }
        QueryOp::Contains => {
            field.append_sql(builder);
            builder.push(" @> ");
            builder.push_bind(value.clone());
            Ok(())
        }
        _ => anyhow::bail!(
            "operator {:?} is not supported for json field {}",
            op,
            field.name()
        ),
    }
}

pub(super) fn append_text_array_filter(
    builder: &mut QueryBuilder<'_, Postgres>,
    field: &QueryField,
    op: QueryOp,
    value: &Value,
) -> anyhow::Result<()> {
    let value = value_as_string(field.name(), value)?;
    match op {
        QueryOp::Contains => {
            field.append_sql(builder);
            builder
                .push(" @> ARRAY[")
                .push_bind(value)
                .push("]::text[]");
            Ok(())
        }
        _ => anyhow::bail!(
            "operator {:?} is not supported for text array field {}",
            op,
            field.name()
        ),
    }
}

pub(super) fn comparison_operator(op: QueryOp) -> anyhow::Result<&'static str> {
    match op {
        QueryOp::Eq => Ok(" = "),
        QueryOp::Ne => Ok(" <> "),
        QueryOp::Gt => Ok(" > "),
        QueryOp::Gte => Ok(" >= "),
        QueryOp::Lt => Ok(" < "),
        QueryOp::Lte => Ok(" <= "),
        QueryOp::Contains | QueryOp::IsNull | QueryOp::IsNotNull => {
            anyhow::bail!("operator {:?} is not a scalar comparison", op)
        }
    }
}

pub(super) fn value_as_string(field: &str, value: &Value) -> anyhow::Result<String> {
    let value = value
        .as_str()
        .map(str::to_string)
        .ok_or_else(|| anyhow::anyhow!("field {field} requires a string value"))?;
    if value.len() > MAX_STRUCTURED_QUERY_STRING_VALUE_BYTES {
        anyhow::bail!(
            "field {field} string value must be at most {} bytes",
            MAX_STRUCTURED_QUERY_STRING_VALUE_BYTES
        );
    }
    anyhow::ensure!(
        !value.contains('\0'),
        "field {field} string value must not contain NUL"
    );
    Ok(value)
}

pub(super) fn validate_json_filter_value_size(field: &str, value: &Value) -> anyhow::Result<()> {
    let value_len = serde_json::to_vec(value)?.len();
    if value_len > MAX_STRUCTURED_QUERY_JSON_VALUE_BYTES {
        anyhow::bail!(
            "field {field} JSON value must be at most {} bytes",
            MAX_STRUCTURED_QUERY_JSON_VALUE_BYTES
        );
    }
    anyhow::ensure!(
        !json_has_nul(value),
        "field {field} JSON value must not contain NUL"
    );
    Ok(())
}

fn json_has_nul(value: &Value) -> bool {
    match value {
        Value::String(value) => value.contains('\0'),
        Value::Array(items) => items.iter().any(json_has_nul),
        Value::Object(fields) => fields
            .iter()
            .any(|(key, value)| key.contains('\0') || json_has_nul(value)),
        _ => false,
    }
}

pub(super) fn value_as_i64(field: &str, value: &Value) -> anyhow::Result<i64> {
    value
        .as_i64()
        .ok_or_else(|| anyhow::anyhow!("field {field} requires an integer value"))
}

pub(super) fn value_as_bool(field: &str, value: &Value) -> anyhow::Result<bool> {
    value
        .as_bool()
        .ok_or_else(|| anyhow::anyhow!("field {field} requires a boolean value"))
}

pub(super) fn value_as_timestamp(field: &str, value: &Value) -> anyhow::Result<DateTime<Utc>> {
    let value = value_as_string(field, value)?;
    Ok(DateTime::parse_from_rfc3339(&value)
        .map_err(|_| anyhow::anyhow!("field {field} requires an RFC3339 timestamp"))?
        .with_timezone(&Utc))
}

pub(super) fn value_as_uuid(field: &str, value: &Value) -> anyhow::Result<Uuid> {
    let value = value_as_string(field, value)?;
    Uuid::parse_str(&value).map_err(|_| anyhow::anyhow!("field {field} requires a UUID value"))
}

#[cfg(test)]
mod tests;
