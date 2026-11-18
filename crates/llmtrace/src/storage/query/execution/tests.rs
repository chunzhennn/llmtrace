use super::*;
use crate::storage;
use crate::storage::message_contents::ContentHash;

fn request(value: Value) -> StructuredQuery {
    serde_json::from_value(value).unwrap()
}

async fn session(pool: &PgPool, name: &str, when: DateTime<Utc>) -> anyhow::Result<Uuid> {
    let id = Uuid::new_v4();
    sqlx::query("INSERT INTO trace_sessions(id, session_key, first_seen, last_seen, user_name) VALUES ($1, $2, $3, $3, $2)")
        .bind(id).bind(name).bind(when).execute(pool).await?;
    Ok(id)
}

async fn note(
    pool: &PgPool,
    session: Uuid,
    role: &str,
    value: &str,
    when: DateTime<Utc>,
    model: &str,
) -> anyhow::Result<Uuid> {
    let config = storage::note_index::configure_test_tags(pool, &["Team", "topic"]).await?;
    let request = Uuid::new_v4();
    sqlx::query("INSERT INTO request_traces(id, started_at, session_id, method, original_uri, upstream_url, model) VALUES ($1, $2, $3, 'POST', '/completions', 'https://synthetic.invalid', $4)")
        .bind(request).bind(when).bind(session).bind(model).execute(pool).await?;
    sqlx::query("INSERT INTO request_prompt_index_state(request_id, index_version, state, capture_complete, tag_revision) VALUES ($1, 1, 'ready', true, $2)").bind(request).bind(config.revision).execute(pool).await?;
    let value_id: i64 = sqlx::query_scalar("INSERT INTO note_values(index_version, value_sha256, value, value_bytes) VALUES (1, $1, $2, octet_length($2)) ON CONFLICT (index_version, value_sha256) DO UPDATE SET value = note_values.value RETURNING id")
        .bind(ContentHash::of(value).0.to_vec()).bind(value).fetch_one(pool).await?;
    let id = Uuid::new_v4();
    sqlx::query("INSERT INTO request_notes(id, request_id, index_version, field_ordinal, match_ordinal, source_path, source_kind, role, note_value_id, decoded_start_byte, decoded_end_byte, tag_name) VALUES ($1, $2, 1, 0, 0, '/prompt', 'prompt', $3, $4, 0, 10, 'Team')")
        .bind(id).bind(request).bind(role).bind(value_id).execute(pool).await?;
    Ok(id)
}

#[test]
fn related_fields_are_filter_only_and_reject_negative_unknown_or_invalid_filters() {
    for value in [
        json!({"dataset":"sessions", "fields":["prompt_note.value"]}),
        json!({"dataset":"sessions", "order_by":[{"field":"prompt_note.role"}]}),
        json!({"dataset":"requests", "filters":[{"field":"prompt_note.value","op":"eq","value":"x"}]}),
        json!({"dataset":"sessions", "filters":[{"field":"prompt_note.value","op":"ne","value":"x"}]}),
        json!({"dataset":"sessions", "filters":[{"field":"prompt_note.value","op":"is_null"}]}),
        json!({"dataset":"sessions", "filters":[{"field":"prompt_note.observed_at","op":"gte","value":"2026-11-01T00:00:00"}]}),
    ] {
        assert!(ValidatedQuery::try_new(request(value)).is_err());
    }
    assert!(ValidatedQuery::try_new(request(json!({"dataset":"sessions", "filters":[{"field":"prompt_note.value","op":"eq","value":""}]}))).is_ok());
}

#[sqlx::test(migrations = "./migrations")]
#[ignore = "requires local PostgreSQL"]
async fn note_helper_statement_timeouts_return_typed_query_errors(
    pool: PgPool,
) -> anyhow::Result<()> {
    let mut blocker = pool.begin().await?;
    sqlx::query("LOCK TABLE note_values IN ACCESS EXCLUSIVE MODE")
        .execute(&mut *blocker)
        .await?;
    let executor = QueryExecutor::new(&pool);
    let query_error = executor
        .run(request(json!({"dataset":"sessions", "filters":[
            {"field":"prompt_note.value", "op":"eq", "value":"synthetic"}
        ]})))
        .await;
    let context_error = executor
        .note_context(&Default::default(), &Default::default(), Uuid::new_v4())
        .await;
    blocker.rollback().await?;
    assert!(matches!(query_error, Err(StructuredQueryError::Timeout)));
    assert!(matches!(context_error, Err(StructuredQueryError::Timeout)));
    // SQL errors leave no unusable transaction or checked-out connection.
    assert!(
        executor
            .run(request(json!({"dataset":"sessions"})))
            .await
            .is_ok()
    );
    executor.pool.close().await;
    assert!(matches!(
        executor
            .note_context(&Default::default(), &Default::default(), Uuid::new_v4())
            .await,
        Err(StructuredQueryError::Unavailable)
    ));
    Ok(())
}

#[sqlx::test(migrations = "./migrations")]
#[ignore = "requires local PostgreSQL"]
async fn related_predicates_apply_to_one_note_and_deduplicate_sessions(
    pool: PgPool,
) -> anyhow::Result<()> {
    let now = Utc::now();
    let a = session(&pool, "a", now).await?;
    let b = session(&pool, "b", now).await?;
    note(
        &pool,
        a,
        "user",
        "target",
        now - chrono::Duration::days(2),
        "old-model",
    )
    .await?;
    note(&pool, a, "tool", "other", now, "new-model").await?;
    let other = note(&pool, a, "tool", "target", now, "new-model").await?;
    sqlx::query("UPDATE request_notes SET tag_name = 'topic' WHERE id = $1")
        .bind(other)
        .execute(&pool)
        .await?;
    note(&pool, b, "tool", "target", now, "new-model").await?;
    note(&pool, b, "tool", "target", now, "new-model").await?;
    let response = run(&pool, request(json!({"dataset":"sessions", "filters":[
        {"field":"prompt_tag.name","op":"eq","value":"Team"},
        {"field":"prompt_tag.value", "op":"eq", "value":"target"},
        {"field":"prompt_tag.role", "op":"eq", "value":"tool"},
        {"field":"prompt_tag.model", "op":"eq", "value":"new-model"},
        {"field":"prompt_tag.observed_at", "op":"gte", "value":(now - chrono::Duration::days(1)).to_rfc3339()}
    ]}))).await?;
    assert_eq!(response["rows"].as_array().unwrap().len(), 1);
    assert_eq!(response["rows"][0]["id"], json!(b));
    let response = run(
        &pool,
        request(json!({"dataset":"sessions", "filters":[
            {"field":"prompt_tag.value", "op":"eq", "value":"target"},
            {"field":"prompt_tag.value", "op":"eq", "value":"other"}
        ]})),
    )
    .await?;
    assert!(response["rows"].as_array().unwrap().is_empty());
    assert_eq!(response["search"]["coverage"], "unknown");
    let details = run(&pool, request(json!({"dataset":"prompt_tags", "filters":[{"field":"tag_name","op":"eq","value":"Team"},{"field":"tag_value", "op":"eq", "value":"target"}]}))).await?;
    assert_eq!(details["rows"].as_array().unwrap().len(), 3);
    Ok(())
}

#[sqlx::test(migrations = "./migrations")]
#[ignore = "requires local PostgreSQL"]
async fn cursors_cover_tied_rows_without_projecting_keys_and_reject_tampering(
    pool: PgPool,
) -> anyhow::Result<()> {
    let now = Utc::now();
    for name in ["a", "b", "c"] {
        session(&pool, name, now).await?;
    }
    let mut query = request(json!({"dataset":"sessions", "fields":["user_name"], "limit":1}));
    let mut names = Vec::new();
    let mut first_cursor = None;
    loop {
        let result = run(&pool, query.clone()).await?;
        names.push(result["rows"][0]["user_name"].as_str().unwrap().to_owned());
        query.cursor = result["page"]["next_cursor"].as_str().map(str::to_owned);
        if first_cursor.is_none() {
            first_cursor = query.cursor.clone();
        }
        if query.cursor.is_none() {
            assert_eq!(result["page"]["has_more"], false);
            break;
        }
    }
    names.sort();
    assert_eq!(names, ["a", "b", "c"]);
    query.cursor = first_cursor.clone();
    query.fields = Some(vec!["id".into()]);
    assert!(matches!(
        run(&pool, query.clone()).await,
        Err(StructuredQueryError::Invalid(_))
    ));
    query.fields = Some(vec!["user_name".into()]);
    let mut cursor = first_cursor.unwrap();
    cursor.push('x');
    query.cursor = Some(cursor);
    assert!(matches!(
        run(&pool, query).await,
        Err(StructuredQueryError::Invalid(_))
    ));
    Ok(())
}

#[sqlx::test(migrations = "./migrations")]
#[ignore = "requires local PostgreSQL"]
async fn long_plugin_projection_keys_and_literal_contains_preserve_values(
    pool: PgPool,
) -> anyhow::Result<()> {
    let now = Utc::now();
    let session = session(&pool, "literal", now).await?;
    let common = "a".repeat(64);
    let values: serde_json::Map<_, _> = (0..64).map(|n| (format!("field_{n}"), json!(n))).collect();
    let fields: Vec<_> = (0..64)
        .map(|n| format!("plugin_metadata.{common}.field_{n}"))
        .collect();
    sqlx::query("INSERT INTO request_traces(id, started_at, method, original_uri, upstream_url, plugin_metadata, tags) VALUES ($1, now(), 'POST', '/completions', 'https://synthetic.invalid', $2, ARRAY['', NULL, 'a'])")
        .bind(Uuid::new_v4()).bind(json!({&common:values})).execute(&pool).await?;
    let result = storage::run_structured_query(&pool, request(json!({"dataset":"requests", "fields":fields, "filters":[{"field":"tags","op":"contains","value":""}]}))).await?;
    assert_eq!(result["rows"][0].as_object().unwrap().len(), 64);
    assert_eq!(result["rows"][0][&fields[63]], 63);
    note(&pool, session, "user", "100%_\\'🙂", now, "model").await?;
    note(&pool, session, "user", "100foo", now, "model").await?;
    let result = run(&pool, request(json!({"dataset":"sessions", "filters":[{"field":"prompt_note.value","op":"contains","value":"%_\\'🙂"}]}))).await?;
    assert_eq!(result["rows"].as_array().unwrap().len(), 1);
    Ok(())
}

#[sqlx::test(migrations = "./migrations")]
#[ignore = "requires local PostgreSQL"]
async fn abort_releases_query_connections_after_the_sql_timeout_bound(
    pool: PgPool,
) -> anyhow::Result<()> {
    let mut blocker = pool.begin().await?;
    sqlx::query("LOCK TABLE trace_sessions IN ACCESS EXCLUSIVE MODE")
        .execute(&mut *blocker)
        .await?;
    let executor = std::sync::Arc::new(QueryExecutor::new(&pool));
    let running = executor.clone();
    let task =
        tokio::spawn(async move { running.run(request(json!({"dataset":"sessions"}))).await });
    // Authoritative lock observation is the barrier, not a guessed sleep.
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let waiting: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM pg_stat_activity WHERE datname = current_database() AND wait_event_type = 'Lock' AND query LIKE 'SELECT jsonb_build_object%')").fetch_one(&pool).await?;
            if waiting { break; }
            tokio::task::yield_now().await;
        }
        Ok::<_, sqlx::Error>(())
    }).await??;
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    // Dropping an HTTP future need not cancel PostgreSQL immediately. Its own
    // statement timeout must still return the connection while the lock is held.
    tokio::time::timeout(Duration::from_secs(6), async {
        loop {
            let waiting: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM pg_stat_activity WHERE datname = current_database() AND wait_event_type = 'Lock' AND query LIKE 'SELECT jsonb_build_object%')").fetch_one(&pool).await?;
            if !waiting && executor.pool.num_idle() == executor.pool.size() as usize { break; }
            tokio::task::yield_now().await;
        }
        Ok::<_, sqlx::Error>(())
    }).await??;
    blocker.rollback().await?;
    assert!(
        executor
            .run(request(json!({"dataset":"sessions"})))
            .await
            .is_ok()
    );
    executor.pool.close().await;
    Ok(())
}

#[sqlx::test(migrations = "./migrations")]
#[ignore = "requires local PostgreSQL"]
async fn oversized_query_pages_fail_without_returning_partial_rows_and_release_the_pool(
    pool: PgPool,
) -> anyhow::Result<()> {
    let metadata = json!({"synthetic":"x".repeat(3 * 1024 * 1024)});
    for _ in 0..4 {
        sqlx::query("INSERT INTO request_traces(id, started_at, method, original_uri, upstream_url, plugin_metadata) VALUES ($1, now(), 'POST', '/', 'https://synthetic.invalid', $2)")
            .bind(Uuid::new_v4()).bind(&metadata).execute(&pool).await?;
    }
    let executor = QueryExecutor::new(&pool);
    assert!(matches!(
        executor
            .run(request(
                json!({"dataset":"requests", "fields":["plugin_metadata"], "limit":4})
            ))
            .await,
        Err(StructuredQueryError::ResponseTooLarge)
    ));
    let result = executor
        .run(request(
            json!({"dataset":"requests", "fields":["id"], "limit":4}),
        ))
        .await?;
    assert_eq!(result["rows"].as_array().unwrap().len(), 4);
    assert_eq!(result["page"]["has_more"], false);
    executor.pool.close().await;
    Ok(())
}

#[sqlx::test(migrations = "./migrations")]
#[ignore = "requires local PostgreSQL"]
async fn note_query_matches_the_shared_frontend_contract_fixture(
    pool: PgPool,
) -> anyhow::Result<()> {
    let id = Uuid::from_u128(1);
    let when = DateTime::parse_from_rfc3339("2026-11-01T00:00:00Z")?.with_timezone(&Utc);
    sqlx::query("INSERT INTO trace_sessions(id, session_key, first_seen, last_seen, user_name) VALUES ($1, 'contract', $2, $2, 'synthetic user')").bind(id).bind(when).execute(&pool).await?;
    note(&pool, id, "user", "synthetic", when, "synthetic-model").await?;
    let result = run(&pool, request(json!({"dataset":"sessions", "fields":["id","last_seen","user_name"], "filters":[{"field":"prompt_note.value","op":"eq","value":"synthetic"}], "limit":10}))).await?;
    let expected: Value = serde_json::from_str(include_str!(
        "../../../../ui/src/lib/api/fixtures/prompt-note-query.json"
    ))?;
    assert_eq!(result, expected);
    Ok(())
}
