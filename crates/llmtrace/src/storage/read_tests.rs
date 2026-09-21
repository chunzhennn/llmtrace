use super::*;

#[sqlx::test(migrations = "./migrations")]
#[ignore = "requires local PostgreSQL"]
async fn request_and_analytics_reads_preserve_filtering_and_aggregation(
    pool: PgPool,
) -> anyhow::Result<()> {
    use crate::{
        plugins::PluginManager,
        trace::{TraceEvent, build_trace},
    };
    let plugins = PluginManager::load(&[])?;
    let archive = ArchiveConfig {
        storage_backend: ArchiveStorageBackend::Postgres,
        ..Default::default()
    };
    let started = Utc::now() - ChronoDuration::minutes(1);
    let mut ids = Vec::new();
    for (index, status, duration, person) in [
        (1, 200, 100, "alice"),
        (2, 429, 500, "alice"),
        (3, 200, 900, "bob"),
    ] {
        let id = Uuid::from_u128(index);
        let mut event = TraceEvent::base(id, started);
        event.original_uri = "/v1/chat/completions".into();
        event.upstream_url = format!("https://{person}.example/v1/chat/completions");
        event.upstream_host = Some(format!("{person}.example"));
        event.api_key_hash = Some(person.into());
        event.user_id = Some(person.into());
        event.user_name = Some(person.into());
        event.status = Some(status);
        event.duration_ms = Some(duration);
        event.ttft_ms = Some(20);
        event.request_body = serde_json::to_vec(
            &json!({"model":person,"metadata":{"session_id":"shared"},"messages":[{"role":"user","content":"hello"}]}),
        )?;
        event.response_body = br#"{"choices":[{"message":{"role":"assistant","content":"hello"}}],"usage":{"prompt_tokens":10,"completion_tokens":2}}"#.to_vec();
        event.request_body_bytes = event.request_body.len() as i64;
        event.response_body_bytes = event.response_body.len() as i64;
        let (trace, messages, uid, name) = build_trace(event, &plugins)?;
        insert_trace(&pool, &archive, trace, messages, uid, name).await?;
        ids.push(id);
    }
    let failures = list_requests(
        &pool,
        RequestListFilters {
            has_error: Some(true),
            ..Default::default()
        },
    )
    .await?;
    assert_eq!(failures["items"].as_array().unwrap().len(), 1);
    assert_eq!(failures["items"][0]["id"], ids[1].to_string());
    let session_id = Uuid::parse_str(failures["items"][0]["session_id"].as_str().unwrap())?;
    let requests = list_session_requests(&pool, session_id, Some(1), Some(0))
        .await?
        .unwrap();
    assert_eq!(requests["items"][0]["id"], ids[1].to_string());
    assert_eq!(requests["page"]["next_offset"], 1);
    let last = list_session_requests(&pool, session_id, Some(1), Some(1))
        .await?
        .unwrap();
    assert_eq!(last["items"][0]["id"], ids[0].to_string());
    assert_eq!(last["page"]["has_more"], false);
    let session = get_session(&pool, session_id, None, None).await?.unwrap();
    assert_eq!(session["request_stats"]["request_count"], 2);
    assert_eq!(session["request_stats"]["error_count"], 1);
    assert_eq!(session["request_stats"]["avg_duration_ms"], 300);
    assert_eq!(session["request_stats"]["input_tokens"], 20);
    let errors = recent_error_requests(&pool, None, None).await?;
    assert_eq!(errors["items"].as_array().unwrap().len(), 1);
    let slow = slow_requests(&pool, None, Some(500), Some(1), None).await?;
    assert_eq!(slow["items"][0]["id"], ids[2].to_string());
    assert_eq!(slow["page"]["has_more"], true);
    for (name, value) in [
        ("model", model_usage(&pool, None, None).await?),
        ("user_id", user_usage(&pool, None, None).await?),
        ("api_key_hash", api_key_usage(&pool, None, None).await?),
    ] {
        assert_eq!(value["items"].as_array().unwrap().len(), 2);
        assert_eq!(value["items"][0][name], "alice");
        assert_eq!(value["items"][0]["request_count"], 2);
        assert_eq!(value["items"][0]["error_count"], 1);
        assert_eq!(value["items"][0]["input_tokens"], 20);
    }
    let upstreams = upstream_health(&pool, None, None).await?;
    assert_eq!(upstreams["items"][0]["upstream_host"], "alice.example");
    assert_eq!(upstreams["items"][0]["error_rate"], 0.5);
    let latency = latency_summary(&pool, None, None).await?;
    assert_eq!(latency["totals"]["request_count"], 3);
    assert_eq!(latency["totals"]["p50_duration_ms"], 500);
    assert_eq!(stats(&pool).await?["errors"], 1);
    request_facets(&pool, None, None).await?;
    usage_summary(&pool, None, None).await?;
    usage_timeseries(&pool, None, None).await?;
    data_overview(&pool, None).await?;
    data_integrity(&pool, None).await?;
    storage_summary(&pool).await?;
    Ok(())
}

#[sqlx::test(migrations = "./migrations")]
#[ignore = "requires local PostgreSQL"]
async fn workflow_enumerates_more_than_500_sessions(pool: PgPool) -> anyhow::Result<()> {
    sqlx::query(
        "INSERT INTO trace_sessions (id, session_key, first_seen, last_seen)
                 SELECT md5(i::text)::uuid, 'workflow-' || i, '2026-09-15T12:00:00Z'::timestamptz,
                        '2026-09-15T13:00:00Z'::timestamptz FROM generate_series(1, 501) i",
    )
    .execute(&pool)
    .await?;
    let mut query = json!({
        "dataset": "sessions", "fields": ["id", "last_seen"], "limit": 500,
        "filters": [
            {"field":"last_seen", "op":"gte", "value":"2026-09-15T00:00:00Z"},
            {"field":"last_seen", "op":"lt", "value":"2026-09-16T00:00:00Z"}
        ],
        "order_by": [{"field":"id", "direction":"asc"}]
    });
    let first = run_structured_query(&pool, serde_json::from_value(query.clone())?).await?;
    let first = first["rows"].as_array().unwrap();
    assert_eq!(first.len(), 500);
    let last_id = first.last().unwrap()["id"].clone();
    query["filters"]
        .as_array_mut()
        .unwrap()
        .push(json!({"field":"id", "op":"gt", "value": last_id}));
    let next = run_structured_query(&pool, serde_json::from_value(query.clone())?).await?;
    let next = next["rows"].as_array().unwrap();
    assert_eq!(next.len(), 1);
    assert!(next[0]["id"].as_str() > last_id.as_str());
    query["filters"][2]["value"] = next[0]["id"].clone();
    let end = run_structured_query(&pool, serde_json::from_value(query)?).await?;
    assert!(end["rows"].as_array().unwrap().is_empty());
    Ok(())
}

#[sqlx::test(migrations = "./migrations")]
#[ignore = "requires local PostgreSQL"]
async fn nullable_projection_rejects_wrong_column_types(pool: PgPool) -> anyhow::Result<()> {
    let row = sqlx::query("SELECT gen_random_uuid() AS id, 'session' AS session_key,
                          now() AS first_seen, now() AS last_seen, 42 AS user_id, NULL::text AS user_name")
        .fetch_one(&pool).await?;
    assert!(matches!(
        SessionInfo::from_row(&row),
        Err(sqlx::Error::ColumnDecode { .. })
    ));
    Ok(())
}

// Verify results and read-only behavior, rather than spelling of the SQL.
#[sqlx::test(migrations = "./migrations")]
#[ignore = "requires local PostgreSQL"]
async fn breakdowns_preserve_null_groups_time_windows_and_error_classification(
    pool: PgPool,
) -> anyhow::Result<()> {
    for (model, status, error, duration, hours) in [
        (Some("alpha"), Some(200), None, Some(100_i64), 1),
        (Some("alpha"), Some(400), None, Some(500), 1),
        (Some("untimed"), Some(503), Some("proxy failure"), None, 1),
        (Some("untimed"), Some(503), None, None, 1),
        (None, None, Some("connection failure"), None, 1),
        (Some("old"), Some(500), None, Some(9999), 48),
    ] {
        sqlx::query("INSERT INTO request_traces (id, started_at, method, original_uri, upstream_url, model, upstream_host, request_kind, status, error, duration_ms, request_headers)
            VALUES (gen_random_uuid(), $1, 'POST', '/secret-path', 'https://private.example/secret', $2, $2, $3, $4, $5, $6, '{\"authorization\":\"sentinel\"}')")
            .bind(Utc::now() - ChronoDuration::hours(hours)).bind(model).bind(if model.is_some() { "generic_http" } else { "" })
            .bind(status).bind(error).bind(duration).execute(&pool).await?;
    }
    let usage = usage_summary(&pool, None, None).await?;
    assert_eq!(usage["totals"]["request_count"], 5);
    assert_eq!(usage["totals"]["error_count"], 4);
    assert_eq!(usage["top_models"][0]["name"], "alpha");
    assert_eq!(usage["top_models"][2]["name"], "unknown");
    assert_eq!(usage["request_kinds"][1]["name"], "");
    assert_eq!(
        usage_summary(&pool, Some(72), None).await?["totals"]["request_count"],
        6
    );
    let limited = usage_summary(&pool, Some(0), Some(0)).await?;
    assert_eq!(limited["window"]["since_hours"], 1);
    assert_eq!(limited["window"]["limit"], 1);
    assert!(limited["top_models"].as_array().unwrap().len() <= 1);

    let errors = error_summary(&pool, None, None).await?;
    assert_eq!(errors["totals"]["error_count"], 4);
    assert_eq!(errors["top_models"][0]["name"], "untimed");
    assert_eq!(errors["top_models"][0]["error_count"], 2);
    let sources: std::collections::BTreeMap<_, _> = errors["sources"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| {
            (
                row["name"].as_str().unwrap(),
                row["error_count"].as_i64().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        sources,
        std::collections::BTreeMap::from([
            ("http_4xx", 1),
            ("http_5xx", 1),
            ("proxy_error", 1),
            ("proxy_error_and_http_5xx", 1),
        ])
    );
    assert_eq!(errors["status_classes"][0]["name"], "5xx");
    assert_eq!(errors["status_classes"][0]["error_count"], 2);
    let latency = latency_summary(&pool, None, None).await?;
    assert_eq!(latency["totals"]["request_count"], 5);
    assert_eq!(latency["totals"]["duration_count"], 2);
    assert_eq!(latency["totals"]["p50_duration_ms"], 300);
    assert_eq!(latency["top_models"].as_array().unwrap().len(), 1);
    assert_eq!(latency["top_models"][0]["name"], "alpha");
    let slow = slow_requests(&pool, None, None, None, None).await?;
    assert!(slow["items"].as_array().unwrap().is_empty());
    assert_eq!(slow["page"]["limit"], 50);
    assert_eq!(
        slow_requests(&pool, None, Some(-1), None, None).await?["items"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    let facets = request_facets(&pool, None, None).await?;
    assert!(
        facets["facets"]["models"]
            .as_array()
            .unwrap()
            .iter()
            .all(|r| r["value"] != "unknown")
    );
    assert!(facets["facets"]["statuses"][0]["value"].is_number());
    assert!(facets["facets"]["error_states"][0]["value"].is_boolean());
    for value in [
        usage,
        errors,
        latency,
        data_overview(&pool, None).await?,
        data_integrity(&pool, None).await?,
        storage_summary(&pool).await?,
    ] {
        assert_aggregate_only(&value);
    }
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM request_traces")
            .fetch_one(&pool)
            .await?,
        6
    );
    Ok(())
}

fn assert_aggregate_only(value: &Value) {
    match value {
        Value::Object(fields) => {
            for (key, value) in fields {
                assert!(
                    ![
                        "original_uri",
                        "upstream_url",
                        "session_key",
                        "request_headers",
                        "response_headers",
                        "request_body",
                        "response_body",
                        "plugin_metadata",
                        "detail"
                    ]
                    .contains(&key.as_str()),
                    "unexpected field {key}"
                );
                assert_aggregate_only(value);
            }
        }
        Value::Array(values) => values.iter().for_each(assert_aggregate_only),
        _ => (),
    }
}

#[sqlx::test(migrations = "./migrations")]
#[ignore = "requires local PostgreSQL"]
async fn administration_reads_preserve_expiry_hashes_and_audit_aggregates(
    pool: PgPool,
) -> anyhow::Result<()> {
    for (token, hours) in [("active-token", 1), ("expired-token", -1)] {
        sqlx::query("INSERT INTO ui_sessions VALUES ($1, 'alice', 'Alice', 'local', now(), $2)")
            .bind(token)
            .bind(Utc::now() + ChronoDuration::hours(hours))
            .execute(&pool)
            .await?;
    }
    sqlx::query("INSERT INTO ui_audit_events (event_type, user_id, remote_addr, detail) VALUES
        ('login_success', 'alice', '127.0.0.1', '{\"secret\":true}'), ('login_success', 'alice', NULL, '{}'), ('logout', NULL, NULL, '{}')")
        .execute(&pool).await?;
    let active = list_ui_sessions(&pool, false, None, None).await?;
    assert_eq!(active["items"].as_array().unwrap().len(), 1);
    assert_eq!(
        active["items"][0]["session_hash"],
        sha256_hex(b"active-token")
    );
    assert_eq!(active["items"][0]["expired"], false);
    let all = list_ui_sessions(&pool, true, Some(1), None).await?;
    assert_eq!(all["page"]["include_expired"], true);
    assert_eq!(all["page"]["next_offset"], 1);
    let last = list_ui_sessions(&pool, true, Some(1), Some(1)).await?;
    assert_eq!(last["items"][0]["expired"], true);
    assert!(!all.to_string().contains("active-token"));
    let audit = audit_summary(&pool, None, Some(1)).await?;
    assert_eq!(audit["totals"]["event_count"], 3);
    assert_eq!(audit["event_types"][0]["event_count"], 2);
    assert_eq!(audit["event_types"].as_array().unwrap().len(), 1);
    assert_aggregate_only(&audit);
    let retention = retention_status(
        &pool,
        &StorageConfig {
            retention_days: Some(1),
            ..Default::default()
        },
    )
    .await?;
    assert_eq!(retention["expired"]["ui_sessions"], 1);
    assert_aggregate_only(&retention);
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM ui_sessions")
            .fetch_one(&pool)
            .await?,
        2
    );
    let removed = revoke_ui_session(&pool, &sha256_hex(b"active-token"))
        .await?
        .unwrap();
    assert_eq!(removed["session_hash"], active["items"][0]["session_hash"]);
    assert_eq!(removed["expired"], false);
    assert!(
        revoke_ui_session(&pool, &sha256_hex(b"active-token"))
            .await?
            .is_none()
    );
    Ok(())
}
