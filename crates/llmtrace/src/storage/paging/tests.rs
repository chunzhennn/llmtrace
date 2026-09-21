use super::*;

#[test]
fn page_defaults_clamps_and_continuation() {
    for (limit, offset, expected_limit, expected_offset) in [
        (None, None, 100, 0),
        (Some(i64::MIN), Some(-1), 1, 0),
        (Some(0), Some(0), 1, 0),
        (Some(50), Some(100), 50, 100),
        (Some(500), Some(1_000_000), 500, 1_000_000),
        (Some(i64::MAX), Some(i64::MAX), 500, 1_000_000),
    ] {
        let page = Page::from_query(limit, offset);
        assert_eq!(
            page,
            Page {
                limit: expected_limit,
                offset: expected_offset
            }
        );
        assert_eq!(page.fetch_limit(), expected_limit + 1);
        for has_more in [false, true] {
            let info = page.info(has_more);
            assert_eq!(info.limit, expected_limit);
            assert_eq!(info.offset, expected_offset);
            assert_eq!(info.has_more, has_more);
            assert_eq!(
                info.next_offset,
                has_more.then_some(expected_offset + expected_limit)
            );
        }
    }
}

#[test]
fn window_policies_keep_their_endpoint_limits() {
    for (policy, default, max) in [
        (TOP_N_WINDOW, 10, 50),
        (REQUEST_FACET_WINDOW, 25, 100),
        (RECENT_ERROR_WINDOW, 50, 200),
    ] {
        for (input, hours, limit) in [
            (None, 24, default),
            (Some(i64::MIN), 1, 1),
            (Some(0), 1, 1),
            (Some(6), 6, 6),
            (Some(i64::MAX), 2160, max),
        ] {
            let window = policy.resolve(input, input);
            assert_eq!(
                window,
                Window {
                    since_hours: hours,
                    limit
                }
            );
            assert_eq!(window.fetch_limit(), limit + 1);
            let now = Utc::now();
            assert_eq!(window.cutoff(now), now - Duration::hours(hours));
        }
    }
}

#[test]
fn lookback_defaults_bounds_and_cutoff() {
    for (input, expected) in [
        (None, 24),
        (Some(-10), 1),
        (Some(18), 18),
        (Some(i64::MAX), 2160),
    ] {
        let window = LookbackWindow::from_query(input);
        assert_eq!(window.since_hours, expected);
        let now = Utc::now();
        assert_eq!(window.cutoff(now), now - Duration::hours(expected));
    }
}

#[test]
fn timeseries_resolution_bounds_row_count_by_bucket() {
    for (name, bucket, max, interval) in [
        (
            "minute",
            UsageTimeseriesBucket::Minute,
            24,
            "'1 minute'::interval",
        ),
        (
            "hour",
            UsageTimeseriesBucket::Hour,
            2160,
            "'1 hour'::interval",
        ),
        ("day", UsageTimeseriesBucket::Day, 8760, "'1 day'::interval"),
    ] {
        for (input, expected) in [
            (None, 24),
            (Some(-10), 1),
            (Some(12), 12),
            (Some(i64::MAX), max),
        ] {
            let window = UsageTimeseriesWindow::from_query(input, Some(name)).unwrap();
            assert_eq!(window.since_hours, expected);
            assert_eq!(window.bucket, bucket);
            assert_eq!(bucket.as_str(), name);
            assert_eq!(bucket.interval_sql(), interval);
            let now = Utc::now();
            assert_eq!(window.cutoff(now), now - Duration::hours(expected));
        }
    }
    for input in [None, Some(""), Some(" HoUr ")] {
        assert_eq!(
            UsageTimeseriesWindow::from_query(None, input)
                .unwrap()
                .bucket,
            UsageTimeseriesBucket::Hour
        );
    }
    assert!(UsageTimeseriesWindow::from_query(None, Some("week")).is_err());
}
