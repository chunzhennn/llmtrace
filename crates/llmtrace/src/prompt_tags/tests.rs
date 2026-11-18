use super::*;
use serde_json::json;

fn read(
    path: &str,
    body: &[u8],
    headers: &Value,
    limits: ExtractionLimits,
    stopped: bool,
) -> TagExtraction {
    extract(
        body,
        RequestPromptMetadata {
            request_kind: "generic_json",
            original_uri: path,
            request_headers: headers,
            capture_complete: true,
        },
        &["PROJECT".into(), "priority".into()],
        limits,
        &AtomicBool::new(stopped),
    )
}

fn run(path: &str, body: Value) -> TagExtraction {
    read(
        path,
        &serde_json::to_vec(&body).unwrap(),
        &json!({"content-type":"application/json"}),
        Default::default(),
        false,
    )
}

#[test]
fn configured_tags_follow_request_text_fields_in_each_protocol() {
    for (path, body, expected) in [
        (
            "/v1/chat/completions?stream=1",
            json!({"messages":[
            {"role":"developer","content":[{"type":"text","text":"[PROJECT: one]"},{"type":"image_url","image_url":{"url":"[PROJECT: hidden]"}}]},
            {"role":"assistant","content":"[priority: two]","tool_calls":[{"function":{"arguments":"[PROJECT: hidden]"}}]},
            {"role":"tool","content":"[priority: three]"}], "tools":[{"description":"[PROJECT: hidden]"}]}),
            vec![
                ("PROJECT", "one", "developer"),
                ("priority", "two", "assistant"),
                ("priority", "three", "tool"),
            ],
        ),
        (
            "/responses",
            json!({"instructions":"[PROJECT: one]", "input":[{"type":"function_call","arguments":"[PROJECT: hidden]"},
            {"type":"function_call_output","output":"[priority: two]"}]}),
            vec![("PROJECT", "one", "system"), ("priority", "two", "tool")],
        ),
        (
            "/v1/messages",
            json!({"max_tokens":128, "system":"[PROJECT: one]", "messages":[{"role":"user","content":[
            {"type":"tool_use","input":{"q":"[PROJECT: hidden]"}},{"type":"tool_result","content":[{"type":"text","text":"[priority: two]"}]}]}]}),
            vec![("PROJECT", "one", "system"), ("priority", "two", "tool")],
        ),
        (
            "/completions",
            json!({"prompt":["[project: hidden][OTHER: hidden]", "[PROJECT: one][priority:]"]}),
            vec![("PROJECT", "one", "user"), ("priority", "", "user")],
        ),
    ] {
        let result = run(path, body.clone());
        assert_eq!(result.state, ExtractionState::Ready, "{path}");
        let actual: Vec<_> = result
            .notes
            .iter()
            .map(|hit| {
                let text = body.pointer(&hit.source_path).unwrap().as_str().unwrap();
                assert!(
                    text[hit.span.start()..hit.span.end()]
                        .starts_with(&format!("[{}:", hit.tag_name))
                );
                (hit.tag_name.as_str(), hit.value.as_str(), hit.role.as_str())
            })
            .collect();
        assert_eq!(actual, expected, "{path}");
    }
}

#[test]
fn syntax_and_budgets_preserve_positions_and_partial_results() {
    let headers = json!({"content-type":"application/json"});
    let body = br#"{"prompt":["\u005bPROJECT: \u00e9 \u005d[priority:][priority:]","[PROJECT: cross","] [priority: x[y]] [PROJECT: n\u0000ul]"]}"#;
    let result = read("/completions", body, &headers, Default::default(), false);
    assert_eq!(
        result
            .notes
            .iter()
            .map(|n| n.value.as_str())
            .collect::<Vec<_>>(),
        ["é", "", ""]
    );
    assert_eq!(result.notes[2].match_ordinal, 2);
    for reason in [
        NoteDiagnostic::NestedMarker,
        NoteDiagnostic::UnclosedMarker,
        NoteDiagnostic::NulValue,
    ] {
        assert!(result.diagnostics.contains(&reason));
    }
    for (limits, reason) in [
        (
            ExtractionLimits::try_new(1, 100, 8, 8, 8, 16),
            NoteDiagnostic::BodyBudget,
        ),
        (
            ExtractionLimits::try_new(1024, 1, 8, 8, 8, 16),
            NoteDiagnostic::ScanBudget,
        ),
        (
            ExtractionLimits::try_new(1024, 1024, 1, 8, 8, 16),
            NoteDiagnostic::FieldBudget,
        ),
        (
            ExtractionLimits::try_new(1024, 1024, 8, 1, 8, 16),
            NoteDiagnostic::ValueBudget,
        ),
        (
            ExtractionLimits::try_new(1024, 1024, 8, 8, 1, 16),
            NoteDiagnostic::CountBudget,
        ),
        (
            ExtractionLimits::try_new(1024, 1024, 8, 3, 8, 3),
            NoteDiagnostic::TotalValueBudget,
        ),
    ] {
        let result = read(
            "/completions",
            br#"{"prompt":["[priority: abc][priority: def]","next"]}"#,
            &headers,
            limits.unwrap(),
            false,
        );
        assert_eq!(result.state, ExtractionState::Partial);
        assert!(result.diagnostics.contains(&reason), "{reason:?}");
    }
    assert!(
        read("/completions", body, &headers, Default::default(), true)
            .diagnostics
            .contains(&NoteDiagnostic::Cancelled)
    );
}

#[test]
fn incomplete_and_unsupported_inputs_report_the_reason() {
    let result = run(
        "/responses",
        json!({"previous_response_id":"outside", "input":[{"role":"user","content":[
        {"type":"input_text","text":"[PROJECT: visible]"},{"type":"unknown","text":"[priority: hidden]"}]}]}),
    );
    assert!(result.external_history);
    assert_eq!(result.state, ExtractionState::Partial);
    assert_eq!(result.notes.len(), 1);
    assert_eq!(
        run("/metrics/messages", json!({"messages":[]})).state,
        ExtractionState::Unsupported
    );
    let json_headers = json!({"content-type":"application/json"});
    for (path, headers, body, reason) in [
        (
            "/embeddings",
            json_headers.clone(),
            &b"{}"[..],
            NoteDiagnostic::UnsupportedProtocol,
        ),
        (
            "/responses",
            json!({}),
            b"{}",
            NoteDiagnostic::MissingRequestContentType,
        ),
        (
            "/responses",
            json!({"content-type":"application/json","content-encoding":"gzip"}),
            b"{}",
            NoteDiagnostic::UnsupportedEncoding,
        ),
        (
            "/responses",
            json_headers.clone(),
            b"\xff",
            NoteDiagnostic::InvalidUtf8,
        ),
        (
            "/responses",
            json_headers,
            b"{",
            NoteDiagnostic::InvalidJson,
        ),
    ] {
        let result = read(path, body, &headers, Default::default(), false);
        assert!(result.notes.is_empty() && result.diagnostics.contains(&reason));
    }
}
