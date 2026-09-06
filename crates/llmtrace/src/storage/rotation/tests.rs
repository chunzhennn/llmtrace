use super::*;
use crate::plugins::PluginManager;
use crate::trace::{TraceEvent, build_trace};

async fn capture(pool: &PgPool, archive: &ArchiveConfig, offset: i64) -> anyhow::Result<Uuid> {
    let id = Uuid::new_v4();
    let minute = DateTime::from_timestamp(Utc::now().timestamp() / 60 * 60 - 240, 0).unwrap();
    let mut event = TraceEvent::base(id, minute + ChronoDuration::seconds(offset));
    event.original_uri = "/v1/chat/completions".into();
    event.upstream_url = "https://llm.example.com/v1/chat/completions".into();
    event.api_key_hash = Some("rotation-test".into());
    event.status = Some(if offset % 2 == 0 { 200 } else { 429 });
    event.duration_ms = Some(1000);
    event.ttft_ms = Some(100);
    event.request_body = br#"{"model":"test","metadata":{"session_id":"same-conversation"},"messages":[{"role":"user","content":"hello"}]}"#.to_vec();
    event.response_body =
        br#"{"choices":[{"message":{"role":"assistant","content":"hello back"}}]}"#.to_vec();
    event.request_body_bytes = event.request_body.len() as i64;
    event.response_body_bytes = event.response_body.len() as i64;
    let (trace, messages, user_id, user_name) = build_trace(event, &PluginManager::load(&[])?)?;
    insert_trace(pool, archive, trace, messages, user_id, user_name).await?;
    Ok(id)
}

fn archive_config() -> ArchiveConfig {
    ArchiveConfig {
        filesystem_root: std::env::temp_dir()
            .join(format!("llmtrace-rotation-test-{}", Uuid::new_v4())),
        ..Default::default()
    }
}

#[sqlx::test(migrations = "./migrations")]
#[ignore = "requires DATABASE_URL pointing to a local PostgreSQL test server"]
async fn maintenance_coordinates_pruners_and_skips_sessions_being_written(
    pool: PgPool,
) -> anyhow::Result<()> {
    let archive = ArchiveConfig {
        storage_backend: ArchiveStorageBackend::Postgres,
        ..archive_config()
    };
    let id = capture(&pool, &archive, 0).await?;
    let config = StorageConfig {
        rotate_size_bytes: 1,
        ..Default::default()
    };
    let mut prune = pool.begin().await?;
    assert!(try_prune_lock(&mut prune).await?);
    assert_eq!(maintain_archive(&pool, &config, &archive).await?, 0);
    assert_eq!(
        delete_request_traces_before(&pool, Utc::now(), 100).await?,
        0
    );
    assert!(get_request(&pool, id, &archive, false).await?.is_some());
    prune.rollback().await?;

    let session: Uuid = sqlx::query_scalar("SELECT session_id FROM request_traces WHERE id = $1")
        .bind(id)
        .fetch_one(&pool)
        .await?;
    sqlx::query("DELETE FROM request_traces WHERE id = $1")
        .bind(id)
        .execute(&pool)
        .await?;
    let mut writer = pool.begin().await?;
    sqlx::query("SELECT id FROM trace_sessions WHERE id = $1 FOR UPDATE")
        .bind(session)
        .execute(&mut *writer)
        .await?;
    assert_eq!(
        delete_empty_sessions_before(&pool, Utc::now(), 100).await?,
        0
    );
    writer.commit().await?;
    // A writer finishing before cleanup leaves a populated session intact.
    let new_id = capture(&pool, &archive, 1).await?;
    assert_eq!(
        delete_empty_sessions_before(&pool, Utc::now(), 100).await?,
        0
    );
    assert!(get_request(&pool, new_id, &archive, true).await?.is_some());
    assert_eq!(maintain_archive(&pool, &config, &archive).await?, 1);
    Ok(())
}

async fn segment(pool: &PgPool, id: Uuid) -> anyhow::Result<(Uuid, String, i64)> {
    Ok(sqlx::query_as(
        "SELECT s.id, s.storage_key, s.compressed_bytes FROM payload_archive_segments s JOIN payload_archive_records r ON r.segment_id = s.id WHERE r.trace_id = $1 LIMIT 1",
    ).bind(id).fetch_one(pool).await?)
}

#[sqlx::test(migrations = "./migrations")]
#[ignore = "requires DATABASE_URL pointing to a local PostgreSQL test server"]
async fn rotation_keeps_newest_requests_and_consistent_statistics(
    pool: PgPool,
) -> anyhow::Result<()> {
    let mut archive = archive_config();
    let mut ids = Vec::new();
    let mut sizes = Vec::new();
    let mut paths = Vec::new();
    for offset in 0..4 {
        archive.storage_backend = if offset % 2 == 0 {
            ArchiveStorageBackend::Postgres
        } else {
            ArchiveStorageBackend::Filesystem
        };
        let id = capture(&pool, &archive, offset).await?;
        let (_, key, size) = segment(&pool, id).await?;
        paths.push(archive_file_path(&archive.filesystem_root, &key)?);
        sizes.push(size);
        ids.push(id);
    }
    assert!(paths[1].exists());
    let mut config = StorageConfig::default();
    assert_eq!(maintain_archive(&pool, &config, &archive).await?, 0);
    // A rolled-back prune must neither schedule nor remove archive files.
    let mut tx = pool.begin().await?;
    sqlx::query("DELETE FROM request_traces WHERE id = $1")
        .bind(ids[1])
        .execute(&mut *tx)
        .await?;
    tx.rollback().await?;
    assert!(paths[1].exists());
    config.rotate_size_bytes = (sizes[2] + sizes[3]) as u64;
    config.retention_prune_batch_size = 1; // Exercise multiple committed batches.
    assert_eq!(maintain_archive(&pool, &config, &archive).await?, 2);
    assert!(!paths[1].exists());
    assert!(paths[3].exists());
    for (index, id) in ids.iter().enumerate() {
        let detail = get_request(&pool, *id, &archive, true).await?;
        if index < 2 {
            assert!(detail.is_none());
        } else {
            assert!(
                detail.unwrap()["request_body"]
                    .as_str()
                    .unwrap()
                    .contains("hello")
            );
        }
    }
    let rollup: (i64, i64, i64, i64, i64, i64) = sqlx::query_as(
        "SELECT SUM(total)::bigint, SUM(errors)::bigint, SUM(duration_count)::bigint, SUM(duration_sum_ms)::bigint, SUM(ttft_count)::bigint, SUM(ttft_sum_ms)::bigint FROM trace_rollups_minute",
    ).fetch_one(&pool).await?;
    assert_eq!(rollup, (2, 1, 2, 2000, 2, 200));
    let counts: (i64, i64, i64, i64) = sqlx::query_as(
        "SELECT (SELECT COUNT(*) FROM session_messages), (SELECT COUNT(*) FROM payload_archive_records), (SELECT COUNT(*) FROM payload_archive_segment_blobs), (SELECT COUNT(*) FROM trace_sessions)",
    ).fetch_one(&pool).await?;
    assert_eq!(counts, (4, 4, 1, 1));
    let status = retention_status(&pool, &config).await?;
    assert_eq!(
        status["rotation"]["retained_bytes"],
        config.rotate_size_bytes
    );
    assert_eq!(status["rotation"]["pending_delete_files"], 0);
    assert_eq!(status["rotation"]["over_limit"], false);
    assert_eq!(maintain_archive(&pool, &config, &archive).await?, 0); // Exact limit is retained.
    config.rotate_size_bytes = 1;
    assert_eq!(maintain_archive(&pool, &config, &archive).await?, 2);
    let remaining: (i64, i64, i64) = sqlx::query_as(
        "SELECT (SELECT COUNT(*) FROM payload_archive_segments), (SELECT COUNT(*) FROM trace_sessions), (SELECT COUNT(*) FROM trace_rollups_minute)",
    ).fetch_one(&pool).await?;
    assert_eq!(remaining, (0, 0, 0));
    fs::remove_dir_all(&archive.filesystem_root)?;
    Ok(())
}

#[sqlx::test(migrations = "./migrations")]
#[ignore = "requires DATABASE_URL pointing to a local PostgreSQL test server"]
async fn rotation_preserves_shared_legacy_segments_until_last_reference(
    pool: PgPool,
) -> anyhow::Result<()> {
    let archive = archive_config();
    let first = capture(&pool, &archive, 0).await?;
    let second = capture(&pool, &archive, 1).await?;
    let (shared, key, size) = segment(&pool, first).await?;
    let (unused, _, _) = segment(&pool, second).await?;
    // Identical payloads can legitimately reference the same legacy segment.
    sqlx::query("UPDATE payload_archive_records SET segment_id = $1 WHERE trace_id = $2")
        .bind(shared)
        .bind(second)
        .execute(&pool)
        .await?;
    sqlx::query("DELETE FROM payload_archive_segments WHERE id = $1")
        .bind(unused)
        .execute(&pool)
        .await?;
    collect_archive_files(&pool, &archive, Instant::now() + MAINTENANCE_TIME_BUDGET).await?;
    let config = StorageConfig {
        rotate_size_bytes: (size - 1) as u64,
        retention_prune_batch_size: 1,
        ..Default::default()
    };
    assert_eq!(rotate_batch(&pool, &config).await?, 1);
    assert!(archive_file_path(&archive.filesystem_root, &key)?.exists());
    assert!(get_request(&pool, second, &archive, true).await?.is_some());
    assert_eq!(maintain_archive(&pool, &config, &archive).await?, 1);
    assert!(!archive_file_path(&archive.filesystem_root, &key)?.exists());
    fs::remove_dir_all(&archive.filesystem_root)?;
    Ok(())
}

#[sqlx::test(migrations = "./migrations")]
#[ignore = "requires DATABASE_URL pointing to a local PostgreSQL test server"]
async fn rotation_retries_file_failures_before_evicting_more_requests(
    pool: PgPool,
) -> anyhow::Result<()> {
    let archive = archive_config();
    let first = capture(&pool, &archive, 0).await?;
    let second = capture(&pool, &archive, 1).await?;
    let (_, key, _) = segment(&pool, first).await?;
    let path = archive_file_path(&archive.filesystem_root, &key)?;
    let backup = archive.filesystem_root.join("backup.zst");
    fs::rename(&path, &backup)?;
    fs::create_dir(&path)?; // remove_file must fail, even when tests run as root.
    sqlx::query("DELETE FROM request_traces WHERE id = $1")
        .bind(first)
        .execute(&pool)
        .await?;
    let mut config = StorageConfig {
        rotate_size_bytes: 1,
        ..Default::default()
    };
    assert!(maintain_archive(&pool, &config, &archive).await.is_err());
    assert!(get_request(&pool, second, &archive, false).await?.is_some());
    let status = retention_status(&pool, &config).await?;
    assert_eq!(status["rotation"]["pending_delete_files"], 1);
    assert_eq!(status["rotation"]["over_limit"], true);
    fs::remove_dir(&path)?;
    fs::rename(&backup, &path)?;
    config.rotate_size_bytes = 0;
    // A new maintenance invocation (including a restart with rotation disabled)
    // finishes committed file jobs without evicting remaining requests.
    assert_eq!(maintain_archive(&pool, &config, &archive).await?, 0);
    assert!(!path.exists());
    assert!(get_request(&pool, second, &archive, true).await?.is_some());
    let (_, key, _) = segment(&pool, second).await?;
    sqlx::query("DELETE FROM request_traces WHERE id = $1")
        .bind(second)
        .execute(&pool)
        .await?;
    fs::remove_file(archive_file_path(&archive.filesystem_root, &key)?)?;
    maintain_archive(&pool, &config, &archive).await?; // Missing files are idempotent.
    assert_eq!(
        retention_status(&pool, &config).await?["rotation"]["pending_delete_files"],
        0
    );
    fs::remove_dir_all(&archive.filesystem_root)?;
    Ok(())
}

#[sqlx::test(migrations = "./migrations")]
#[ignore = "requires DATABASE_URL pointing to a local PostgreSQL test server"]
async fn age_retention_reclaims_payloads_and_preserves_partial_minute_rollups(
    pool: PgPool,
) -> anyhow::Result<()> {
    let archive = archive_config();
    let first = capture(&pool, &archive, 0).await?;
    let second = capture(&pool, &archive, 1).await?;
    let (_, key, _) = segment(&pool, first).await?;
    assert_eq!(delete_request_traces_before(&pool, Utc::now(), 1).await?, 1);
    assert_eq!(delete_rollups_before(&pool, Utc::now(), 100).await?, 0);
    let total: i64 = sqlx::query_scalar("SELECT SUM(total)::bigint FROM trace_rollups_minute")
        .fetch_one(&pool)
        .await?;
    assert_eq!(total, 1);
    maintain_archive(&pool, &StorageConfig::default(), &archive).await?;
    assert!(!archive_file_path(&archive.filesystem_root, &key)?.exists());
    assert!(get_request(&pool, second, &archive, true).await?.is_some());
    // Account for captures made before archive segments were introduced.
    sqlx::query("DELETE FROM payload_archive_segments")
        .execute(&pool)
        .await?;
    sqlx::query("UPDATE request_traces SET request_body_compressed = $1 WHERE id = $2")
        .bind(b"legacy".as_slice())
        .bind(second)
        .execute(&pool)
        .await?;
    let config = StorageConfig {
        rotate_size_bytes: 5,
        ..Default::default()
    };
    assert_eq!(maintain_archive(&pool, &config, &archive).await?, 1);
    assert!(get_request(&pool, second, &archive, false).await?.is_none());
    fs::remove_dir_all(&archive.filesystem_root)?;
    Ok(())
}
