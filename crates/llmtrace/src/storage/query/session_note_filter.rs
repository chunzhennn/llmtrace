//! Fixed session -> request -> prompt tag relation. Every related predicate applies to
//! one occurrence, never to independent prompt tag matches in the same session.
use chrono::{DateTime, Utc};
use serde_json::Value;
use sqlx::{Postgres, QueryBuilder, Row, Transaction};
use std::collections::BTreeMap;

use crate::prompt_tags::NoteIndexVersion;
use crate::storage::message_contents::ContentHash;

use super::{QueryFilter, QueryOp};

enum NotePredicate {
    ValueEquals {
        hash: ContentHash,
        value: String,
        id: Option<i64>,
    },
    ValueContains(String),
    TagName(String),
    Exists,
    RoleEquals(String),
    ModelEquals(String),
    ApiKeyEquals(String),
    Observed {
        op: QueryOp,
        value: DateTime<Utc>,
    },
}

pub(super) struct SessionNoteFilter {
    predicates: Vec<NotePredicate>,
}

pub(super) fn schema() -> Value {
    serde_json::json!([
        {"name":"prompt_tag.name","filter_kind":"text","operators":["eq"]},
        {"name":"prompt_tag.value","filter_kind":"text","operators":["eq","contains","is_not_null"]},
        {"name":"prompt_tag.role","filter_kind":"text","operators":["eq"]},
        {"name":"prompt_tag.model","filter_kind":"text","operators":["eq"]},
        {"name":"prompt_tag.api_key_hash","filter_kind":"text","operators":["eq"]},
        {"name":"prompt_tag.observed_at","filter_kind":"timestamp","operators":["eq","gt","gte","lt","lte"]}
    ])
}

pub(super) fn related_field(field: &str) -> Option<&str> {
    field
        .strip_prefix("prompt_tag.")
        .or_else(|| field.strip_prefix("prompt_note."))
}

impl SessionNoteFilter {
    pub(super) fn try_new(filters: &[QueryFilter]) -> anyhow::Result<Self> {
        let mut predicates = Vec::new();
        for filter in filters {
            let Some(field) = related_field(&filter.field) else {
                continue;
            };
            if field == "value" && matches!(filter.op, QueryOp::IsNotNull) {
                anyhow::ensure!(
                    filter.value.as_ref().is_none_or(Value::is_null),
                    "prompt tag existence filter takes no value"
                );
                predicates.push(NotePredicate::Exists);
                continue;
            }
            let value = filter
                .value
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("prompt tag filter requires a value"))?;
            let predicate = match (field, filter.op) {
                ("name", QueryOp::Eq) => {
                    NotePredicate::TagName(super::value_as_string(&filter.field, value)?)
                }
                ("value", QueryOp::Eq) => {
                    let value = super::value_as_string(&filter.field, value)?;
                    NotePredicate::ValueEquals {
                        hash: ContentHash::of(&value),
                        value,
                        id: None,
                    }
                }
                ("value", QueryOp::Contains) => {
                    NotePredicate::ValueContains(super::value_as_string(&filter.field, value)?)
                }
                ("role", QueryOp::Eq) => {
                    NotePredicate::RoleEquals(super::value_as_string(&filter.field, value)?)
                }
                ("model", QueryOp::Eq) => {
                    NotePredicate::ModelEquals(super::value_as_string(&filter.field, value)?)
                }
                ("api_key_hash", QueryOp::Eq) => {
                    NotePredicate::ApiKeyEquals(super::value_as_string(&filter.field, value)?)
                }
                (
                    "observed_at",
                    QueryOp::Eq | QueryOp::Gt | QueryOp::Gte | QueryOp::Lt | QueryOp::Lte,
                ) => NotePredicate::Observed {
                    op: filter.op,
                    value: super::value_as_timestamp(&filter.field, value)?,
                },
                _ => anyhow::bail!(
                    "field/operator is not supported for related prompt tag filtering"
                ),
            };
            predicates.push(predicate);
        }
        Ok(Self { predicates })
    }

    pub(super) fn is_empty(&self) -> bool {
        self.predicates.is_empty()
    }

    /// Resolve exact values once so PostgreSQL can use occurrence frequency
    /// statistics on note_value_id (including highly frequent values). Hash and
    /// exact UTF-8 comparison still verify identity; no user SQL is introduced.
    pub(super) async fn resolve_values(
        &mut self,
        tx: &mut Transaction<'_, Postgres>,
        version: NoteIndexVersion,
    ) -> anyhow::Result<()> {
        let mut expected = BTreeMap::new();
        for predicate in &self.predicates {
            if let NotePredicate::ValueEquals { hash, value, .. } = predicate {
                expected.insert(*hash, value.as_str());
            }
        }
        if expected.is_empty() {
            return Ok(());
        }
        let hashes: Vec<Vec<u8>> = expected.keys().map(|hash| hash.0.to_vec()).collect();
        let rows = sqlx::query("SELECT id, value_sha256, value FROM note_values WHERE index_version = $1 AND value_sha256 = ANY($2)")
            .bind(i32::from(version)).bind(&hashes).fetch_all(&mut **tx).await?;
        let mut resolved = BTreeMap::new();
        for row in rows {
            let hash: Vec<u8> = row.try_get("value_sha256")?;
            let hash = ContentHash(
                hash.try_into()
                    .map_err(|_| anyhow::anyhow!("prompt tag value hash integrity mismatch"))?,
            );
            anyhow::ensure!(
                expected.get(&hash).copied() == Some(row.try_get::<&str, _>("value")?),
                "prompt tag value identity integrity mismatch"
            );
            resolved.insert(hash, row.try_get::<i64, _>("id")?);
        }
        for predicate in &mut self.predicates {
            if let NotePredicate::ValueEquals { hash, id, .. } = predicate {
                *id = resolved.get(hash).copied();
            }
        }
        Ok(())
    }

    pub(super) fn append_sql(
        &self,
        builder: &mut QueryBuilder<'_, Postgres>,
        version: NoteIndexVersion,
        revision: i64,
    ) {
        builder.push("EXISTS (SELECT 1 FROM trace_prompt_tags pt WHERE pt.session_id = trace_sessions.id AND pt.index_version = ");
        builder
            .push_bind(i32::from(version))
            .push(" AND pt.tag_revision = ")
            .push_bind(revision);
        for predicate in &self.predicates {
            builder.push(" AND ");
            match predicate {
                NotePredicate::ValueEquals { hash, value, id } => {
                    let Some(id) = id else {
                        builder.push("false");
                        continue;
                    };
                    builder
                        .push("pt.note_value_id = ")
                        .push_bind(*id)
                        .push(" AND ");
                    builder
                        .push("pt.note_value_sha256 = ")
                        .push_bind(hash.0.to_vec())
                        .push(" AND pt.tag_value = ")
                        .push_bind(value.clone());
                }
                NotePredicate::ValueContains(value) => {
                    builder
                        .push("pt.tag_value ILIKE ")
                        .push_bind(format!("%{}%", crate::storage::escape_like(value)))
                        .push(" ESCAPE '\\'");
                }
                NotePredicate::Exists => {
                    builder.push("pt.tag_value IS NOT NULL");
                }
                NotePredicate::TagName(name) => {
                    builder.push("pt.tag_name = ").push_bind(name.clone());
                }
                NotePredicate::RoleEquals(value) => {
                    builder.push("pt.role = ").push_bind(value.clone());
                }
                NotePredicate::ModelEquals(value) => {
                    builder.push("pt.model = ").push_bind(value.clone());
                }
                NotePredicate::ApiKeyEquals(value) => {
                    builder.push("pt.api_key_hash = ").push_bind(value.clone());
                }
                NotePredicate::Observed { op, value } => {
                    builder
                        .push("pt.observed_at ")
                        .push(match op {
                            QueryOp::Eq => "=",
                            QueryOp::Gt => ">",
                            QueryOp::Gte => ">=",
                            QueryOp::Lt => "<",
                            QueryOp::Lte => "<=",
                            _ => unreachable!("validated prompt tag timestamp comparison"),
                        })
                        .push_bind(*value);
                }
            }
        }
        builder.push(")");
    }
}
