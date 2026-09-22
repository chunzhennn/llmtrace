use super::*;

#[test]
fn uuid_comparisons_match_the_discovered_schema() {
    let dataset = dataset_spec("sessions").unwrap();
    for op in [
        QueryOp::Eq,
        QueryOp::Ne,
        QueryOp::Gt,
        QueryOp::Gte,
        QueryOp::Lt,
        QueryOp::Lte,
    ] {
        let mut builder = QueryBuilder::<Postgres>::new("");
        append_filter(
            &mut builder,
            dataset,
            &QueryFilter {
                field: "id".into(),
                op,
                value: Some(json!(Uuid::new_v4())),
            },
        )
        .unwrap();
    }
    let schema = structured_query_schema();
    let sessions = schema["datasets"]
        .as_array()
        .unwrap()
        .iter()
        .find(|v| v["name"] == "sessions")
        .unwrap();
    let id = sessions["fields"]
        .as_array()
        .unwrap()
        .iter()
        .find(|v| v["name"] == "id")
        .unwrap();
    for op in ["gt", "gte", "lt", "lte"] {
        assert!(id["operators"].as_array().unwrap().contains(&json!(op)));
    }
    let mut builder = QueryBuilder::<Postgres>::new("");
    assert!(
        append_filter(
            &mut builder,
            dataset,
            &QueryFilter {
                field: "id".into(),
                op: QueryOp::Gt,
                value: Some(json!("invalid-uuid")),
            }
        )
        .is_err()
    );
}

#[test]
fn structured_query_rejects_unknown_dataset() {
    let error = dataset_spec("ui_sessions").unwrap_err().to_string();

    assert!(error.contains("unknown query dataset"));
}

#[test]
fn structured_query_schema_describes_datasets_fields_and_limits() {
    let schema = structured_query_schema();

    assert_eq!(schema["limits"]["max_fields"], MAX_STRUCTURED_QUERY_FIELDS);
    assert_eq!(
        schema["limits"]["max_filters"],
        MAX_STRUCTURED_QUERY_FILTERS
    );
    assert_eq!(
        schema["limits"]["max_order_by"],
        MAX_STRUCTURED_QUERY_ORDER_BY
    );
    assert_eq!(
        schema["limits"]["max_string_value_bytes"],
        MAX_STRUCTURED_QUERY_STRING_VALUE_BYTES
    );
    assert_eq!(
        schema["limits"]["max_json_value_bytes"],
        MAX_STRUCTURED_QUERY_JSON_VALUE_BYTES
    );
    assert_eq!(schema["sort_directions"], json!(["asc", "desc"]));
    assert_eq!(
        schema["plugin_metadata"],
        json!({
            "dataset": "requests",
            "field_prefix": PLUGIN_METADATA_FIELD_PREFIX,
            "filter_kind": "json_path",
            "operators": ["eq", "ne", "contains", "is_null", "is_not_null"],
            "max_segments": MAX_PLUGIN_METADATA_PATH_SEGMENTS,
            "max_segment_bytes": MAX_PLUGIN_METADATA_PATH_SEGMENT_LEN,
            "segment_pattern": "[A-Za-z0-9_-]+",
        })
    );

    let datasets = schema["datasets"].as_array().unwrap();
    let requests = datasets
        .iter()
        .find(|dataset| dataset["name"] == "requests")
        .unwrap();
    assert!(
        requests["default_fields"]
            .as_array()
            .unwrap()
            .contains(&json!("started_at"))
    );
    assert_eq!(
        requests["default_order"],
        json!([{"field": "started_at", "direction": "desc"}])
    );

    let fields = requests["fields"].as_array().unwrap();
    let status = fields
        .iter()
        .find(|field| field["name"] == "status")
        .unwrap();
    assert_eq!(status["filter_kind"], "int");
    assert_eq!(
        status["operators"],
        json!([
            "eq",
            "ne",
            "gt",
            "gte",
            "lt",
            "lte",
            "is_null",
            "is_not_null"
        ])
    );

    let tags = fields.iter().find(|field| field["name"] == "tags").unwrap();
    assert_eq!(tags["filter_kind"], "text_array");
    assert_eq!(
        tags["operators"],
        json!(["contains", "is_null", "is_not_null"])
    );
}

#[test]
fn structured_query_rejects_unknown_field() {
    let dataset = dataset_spec("requests").unwrap();
    let fields = vec!["id".to_string(), "request_body_compressed".to_string()];

    let error = selected_fields(dataset, Some(&fields))
        .unwrap_err()
        .to_string();

    assert!(error.contains("is not allowed"));
}

#[test]
fn structured_query_rejects_too_many_selected_fields() {
    let dataset = dataset_spec("requests").unwrap();
    let fields = vec!["id".to_string(); MAX_STRUCTURED_QUERY_FIELDS + 1];

    let error = selected_fields(dataset, Some(&fields))
        .unwrap_err()
        .to_string();

    assert!(error.contains("fields must contain at most"));
}

#[test]
fn structured_query_deduplicates_selected_fields() {
    let dataset = dataset_spec("requests").unwrap();
    let fields = vec!["id".to_string(), "id".to_string(), "status".to_string()];

    let selected = selected_fields(dataset, Some(&fields)).unwrap();

    assert_eq!(
        selected
            .iter()
            .map(|field| field.name())
            .collect::<Vec<_>>(),
        vec!["id", "status"]
    );
}

#[test]
fn structured_query_allows_plugin_metadata_path_field() {
    let dataset = dataset_spec("requests").unwrap();
    let fields = vec![
        "id".to_string(),
        "plugin_metadata.api-key-user-mapper.customer_tier".to_string(),
    ];

    let selected = selected_fields(dataset, Some(&fields)).unwrap();

    assert_eq!(
        selected
            .iter()
            .map(|field| field.name())
            .collect::<Vec<_>>(),
        vec!["id", "plugin_metadata.api-key-user-mapper.customer_tier"]
    );
}

#[test]
fn structured_query_rejects_invalid_plugin_metadata_path_field() {
    let dataset = dataset_spec("requests").unwrap();
    let fields = vec!["plugin_metadata.api.key with spaces".to_string()];

    let error = selected_fields(dataset, Some(&fields))
        .unwrap_err()
        .to_string();

    assert!(error.contains("plugin_metadata path segment"));
}

#[test]
fn structured_query_rejects_too_many_filters() {
    let filters = vec![
        QueryFilter {
            field: "status".to_string(),
            op: QueryOp::Eq,
            value: Some(json!(200)),
        };
        MAX_STRUCTURED_QUERY_FILTERS + 1
    ];

    let error = validate_structured_query_filters(&filters)
        .unwrap_err()
        .to_string();

    assert!(error.contains("filters must contain at most"));
}

#[test]
fn structured_query_rejects_too_many_order_fields() {
    let dataset = dataset_spec("requests").unwrap();
    let order_by = vec![
        QueryOrder {
            field: "started_at".to_string(),
            direction: SortDirection::Desc,
        };
        MAX_STRUCTURED_QUERY_ORDER_BY + 1
    ];

    let error = selected_order(dataset, &order_by).unwrap_err().to_string();

    assert!(error.contains("order_by must contain at most"));
}

#[test]
fn structured_query_rejects_wrong_filter_type() {
    let dataset = dataset_spec("requests").unwrap();
    let filter = QueryFilter {
        field: "status".to_string(),
        op: QueryOp::Eq,
        value: Some(json!("200")),
    };
    let mut builder = QueryBuilder::<Postgres>::new("");

    let error = append_filter(&mut builder, dataset, &filter)
        .unwrap_err()
        .to_string();

    assert!(error.contains("requires an integer value"));
}

#[test]
fn structured_query_rejects_oversized_string_filter_value() {
    let dataset = dataset_spec("requests").unwrap();
    let filter = QueryFilter {
        field: "model".to_string(),
        op: QueryOp::Eq,
        value: Some(json!(
            "a".repeat(MAX_STRUCTURED_QUERY_STRING_VALUE_BYTES + 1)
        )),
    };
    let mut builder = QueryBuilder::<Postgres>::new("");

    let error = append_filter(&mut builder, dataset, &filter)
        .unwrap_err()
        .to_string();

    assert!(error.contains("string value must be at most"));
}

#[test]
fn structured_query_rejects_oversized_json_filter_value() {
    let dataset = dataset_spec("requests").unwrap();
    let filter = QueryFilter {
        field: "plugin_metadata".to_string(),
        op: QueryOp::Contains,
        value: Some(json!({"payload": "a".repeat(MAX_STRUCTURED_QUERY_JSON_VALUE_BYTES)})),
    };
    let mut builder = QueryBuilder::<Postgres>::new("");

    let error = append_filter(&mut builder, dataset, &filter)
        .unwrap_err()
        .to_string();

    assert!(error.contains("JSON value must be at most"));
}

#[test]
fn structured_query_allows_tag_membership_filter() {
    let dataset = dataset_spec("requests").unwrap();
    let filter = QueryFilter {
        field: "tags".to_string(),
        op: QueryOp::Contains,
        value: Some(json!("websocket")),
    };
    let mut builder = QueryBuilder::<Postgres>::new("");

    append_filter(&mut builder, dataset, &filter).unwrap();
}

#[test]
fn structured_query_allows_plugin_metadata_path_filter() {
    let dataset = dataset_spec("requests").unwrap();
    let filter = QueryFilter {
        field: "plugin_metadata.api-key-user-mapper.customer_tier".to_string(),
        op: QueryOp::Eq,
        value: Some(json!("enterprise")),
    };
    let mut builder = QueryBuilder::<Postgres>::new("");

    append_filter(&mut builder, dataset, &filter).unwrap();
}

#[test]
fn message_truncation_filter_accepts_boolean_values_only() {
    let dataset = dataset_spec("messages").unwrap();
    for (value, accepted) in [
        (json!(true), true),
        (json!(false), true),
        (json!("true"), false),
    ] {
        let mut builder = QueryBuilder::<Postgres>::new("");
        let result = append_filter(
            &mut builder,
            dataset,
            &QueryFilter {
                field: "content_truncated".into(),
                op: QueryOp::Eq,
                value: Some(value),
            },
        );
        assert_eq!(result.is_ok(), accepted);
    }
}
