//! Durable derived prompt tag indexing: SQL/fencing and task ownership are separate.
mod note_index_maintenance;
mod note_index_store;
mod note_index_worker;

pub(crate) use note_index_maintenance::{NoteMaintenanceAction, maintain};
pub(crate) use note_index_store::request_index_status;
pub(crate) use note_index_worker::{NoteIndexOptions, NoteIndexWorker};

#[derive(serde::Deserialize, serde::Serialize, sqlx::FromRow)]
pub(crate) struct PromptTagSettings {
    pub(crate) revision: i64,
    pub(crate) tags: Vec<String>,
}

pub(crate) async fn tag_settings(pool: &sqlx::PgPool) -> anyhow::Result<PromptTagSettings> {
    Ok(sqlx::query_as("SELECT tag_revision AS revision, tag_names AS tags FROM note_index_settings WHERE singleton")
        .fetch_one(pool).await?)
}

pub(crate) async fn save_tag_settings(
    pool: &sqlx::PgPool,
    settings: &PromptTagSettings,
) -> anyhow::Result<Option<PromptTagSettings>> {
    Ok(sqlx::query_as("UPDATE note_index_settings SET tag_names = $2,
        tag_revision = tag_revision + (tag_names <> $2)::integer WHERE singleton AND tag_revision = $1
        RETURNING tag_revision AS revision, tag_names AS tags")
        .bind(settings.revision).bind(&settings.tags).fetch_optional(pool).await?)
}

#[cfg(test)]
pub(super) async fn configure_test_tags(
    pool: &sqlx::PgPool,
    tags: &[&str],
) -> anyhow::Result<PromptTagSettings> {
    let mut settings = tag_settings(pool).await?;
    settings.tags = tags.iter().map(|s| s.to_string()).collect();
    save_tag_settings(pool, &settings)
        .await?
        .ok_or_else(|| anyhow::anyhow!("fixture settings changed"))
}
