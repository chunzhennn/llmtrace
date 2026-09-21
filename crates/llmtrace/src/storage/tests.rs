use super::*;

#[test]
fn decompress_with_limit_allows_body_within_limit() {
    let compressed = compress(b"hello").unwrap();
    let decompressed = decompress_with_limit(&compressed, 5).unwrap();

    assert_eq!(decompressed, b"hello");
}

#[test]
fn decompress_with_limit_allows_empty_body_with_zero_limit() {
    let decompressed = decompress_with_limit(&[], 0).unwrap();

    assert!(decompressed.is_empty());
}

#[test]
fn decompress_with_limit_rejects_body_over_limit() {
    let compressed = compress(b"hello").unwrap();
    let error = decompress_with_limit(&compressed, 4)
        .unwrap_err()
        .to_string();

    assert!(error.contains("decompression limit"));
}

#[test]
fn archive_frame_round_trips_body() {
    let trace_id = Uuid::new_v4();
    let body = br#"{"messages":[{"role":"user","content":"hello"}]}"#;
    let frame = encode_archive_frame(
        trace_id,
        PayloadDirection::RequestBody,
        Some("application/json"),
        body,
    )
    .unwrap();

    let decoded = decode_archive_frame_body(&frame, 0).unwrap();

    assert_eq!(decoded, body);
}

#[test]
fn archive_file_path_rejects_unsafe_keys() {
    let root = Path::new("spool/archive");

    assert!(archive_file_path(root, "session/00000000000000000000.zst").is_ok());
    assert!(archive_file_path(root, "../escape.zst").is_err());
    assert!(archive_file_path(root, "/tmp/escape.zst").is_err());
}

#[test]
fn seconds_since_reports_nonnegative_lag() {
    let now = DateTime::parse_from_rfc3339("2026-07-01T12:00:00Z")
        .unwrap()
        .with_timezone(&Utc);
    let earlier = DateTime::parse_from_rfc3339("2026-07-01T11:59:30Z")
        .unwrap()
        .with_timezone(&Utc);
    let later = DateTime::parse_from_rfc3339("2026-07-01T12:00:30Z")
        .unwrap()
        .with_timezone(&Utc);

    assert_eq!(seconds_since(now, Some(earlier)), Some(30));
    assert_eq!(seconds_since(now, Some(later)), Some(0));
    assert_eq!(seconds_since(now, None), None);
}

#[test]
fn seconds_until_reports_nonnegative_remaining_ttl() {
    let now = DateTime::parse_from_rfc3339("2026-07-01T12:00:00Z")
        .unwrap()
        .with_timezone(&Utc);
    let future = DateTime::parse_from_rfc3339("2026-07-01T12:05:00Z")
        .unwrap()
        .with_timezone(&Utc);
    let past = DateTime::parse_from_rfc3339("2026-07-01T11:55:00Z")
        .unwrap()
        .with_timezone(&Utc);

    assert_eq!(seconds_until(now, future), 300);
    assert_eq!(seconds_until(now, past), 0);
}

#[test]
fn rate_handles_empty_and_negative_counts() {
    assert_eq!(rate(5, 10), 0.5);
    assert_eq!(rate(5, 0), 0.0);
    assert_eq!(rate(-5, 10), 0.0);
}

#[test]
fn retention_cutoff_subtracts_retention_days() {
    let now = DateTime::parse_from_rfc3339("2026-06-29T12:00:00Z")
        .unwrap()
        .with_timezone(&Utc);

    assert_eq!(
        retention_cutoff(now, 30),
        DateTime::parse_from_rfc3339("2026-05-30T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc)
    );
}

#[test]
fn retention_prune_result_sums_deleted_rows() {
    let result = RetentionPruneResult {
        request_traces: 1,
        trace_rollups_minute: 2,
        trace_sessions: 3,
        ui_audit_events: 4,
        ui_sessions: 5,
        oauth_states: 6,
    };

    assert_eq!(result.total_deleted(), 21);
}

#[test]
fn retention_expired_total_sums_nonnegative_counts() {
    assert_eq!(retention_expired_total([1, 2, 3, 4, 5, 6]), 21);
    assert_eq!(retention_expired_total([1, -10, 3, 0, 5, 6]), 15);
}

#[test]
fn storage_summary_totals_use_nonnegative_saturating_addition() {
    assert_eq!(nonnegative_i64(10), 10);
    assert_eq!(nonnegative_i64(-10), 0);
    assert_eq!(saturating_add_i64(5, 7), 12);
    assert_eq!(saturating_add_i64(5, -7), 5);
    assert_eq!(saturating_add_i64(i64::MAX, 1), i64::MAX);
}

#[test]
fn request_search_escapes_like_wildcards() {
    assert_eq!(escape_like(r#"100%\_match"#), r#"100\%\\\_match"#);
}

#[tokio::test]
async fn readiness_check_timeout_allows_fast_future() {
    readiness_check_with_timeout(async { Ok(()) }, Duration::from_millis(1))
        .await
        .unwrap();
}

#[tokio::test]
async fn readiness_check_timeout_rejects_slow_future() {
    let error = readiness_check_with_timeout(
        async { std::future::pending::<anyhow::Result<()>>().await },
        Duration::from_millis(1),
    )
    .await
    .unwrap_err()
    .to_string();

    assert!(error.contains("readiness check timed out"));
}
