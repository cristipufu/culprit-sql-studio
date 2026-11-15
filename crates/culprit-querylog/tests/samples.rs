//! Whole logs as frameworks write them, read the way a scan reads them: the format told from the lines, repeats of one
//! statement shape added up.

use culprit_core::{Engine, Workload};

fn read(name: &str) -> (&'static str, Workload) {
    let text = std::fs::read_to_string(format!("{}/tests/data/{name}", env!("CARGO_MANIFEST_DIR"))).expect("sample");
    let (format, aggregator) = culprit_querylog::read(text.lines(), Engine::Sqlite).expect("a format Culprit reads");
    (format, aggregator.workload(format, 50))
}

#[test]
fn a_rails_log_adds_up_the_listing_query() {
    let (format, w) = read("rails.log");
    assert_eq!(format, "Rails");
    // The same listing for two categories is one shape; another filter combination is another; the cache hit isn't a run.
    let top = &w.queries[0];
    assert_eq!(top.executions, 2.0);
    assert!((top.total_time_ms - 24.3).abs() < 1e-9, "{}", top.total_time_ms);
    assert!(top.text.contains("\"products\".\"category_id\" = ?") && top.text.ends_with("LIMIT ?"), "{}", top.text);
    assert!(w.queries.iter().any(|q| q.text.contains("\"products\".\"color\" = ?") && q.executions == 1.0));
    assert_eq!(w.total_executions, 7.0);
    // Three requests a second apart: a two-second window, dated in UTC.
    assert_eq!(w.window_start.as_deref(), Some("2026-10-15T12:00:01Z"));
    assert!((w.window_hours * 3600.0 - 2.0).abs() < 1e-6, "{}", w.window_hours);
}

#[test]
fn a_django_log_puts_the_values_back_into_one_shape() {
    let (format, w) = read("django.log");
    assert_eq!(format, "Django");
    let listing = w.queries.iter().find(|q| q.text.contains("shop_product")).expect("the listing");
    assert_eq!(listing.executions, 2.0);
    assert!(listing.text.contains("\"category_id\" = ?"), "{}", listing.text);
    // One local timestamp, no zone: no window to claim, and nothing written as UTC.
    assert!(w.window_unknown());
    assert_eq!(w.window_start, None);
    assert_eq!(w.window_phrase(), "the period the Django log covers");
}

#[test]
fn json_lines() {
    let (format, w) = read("statements.jsonl");
    assert_eq!(format, "JSON lines");
    assert_eq!(w.queries[0].executions, 2.0);
    assert!((w.queries[0].total_time_ms - 17.6).abs() < 1e-9);
}

#[test]
fn a_log_with_no_statement_is_no_format() {
    let lines = ["GET /health 200 1ms", "worker started"];
    assert!(culprit_querylog::read(lines.iter().copied(), Engine::Sqlite).is_none());
}

#[test]
fn another_database_s_statements_leave_the_totals_too() {
    let text = std::fs::read_to_string(format!("{}/tests/data/rails.log", env!("CARGO_MANIFEST_DIR"))).expect("sample");
    let (_, mut aggregator) = culprit_querylog::read(text.lines(), Engine::Sqlite).expect("Rails");
    let before = aggregator.entries();
    let left_out = aggregator.retain(|text| !text.contains("\"color\""));
    assert_eq!(left_out, 1);
    assert_eq!(aggregator.entries(), before - 1);
    let w = aggregator.workload("Rails", 50);
    assert!(w.queries.iter().all(|q| !q.text.contains("\"color\"")));
    assert_eq!(w.total_executions, (before - 1) as f64);
}

#[test]
fn structured_errors_without_durations_keep_evidence_without_inventing_fast_runs() {
    let lines = [
        r#"{"sql":"INSERT INTO items(name) VALUES ('private-value')", "error_code":5}"#,
        r#"{"sql":"INSERT INTO items(name) VALUES ('other-private-value')", "error":{"code":"SQLITE_BUSY"}, "at":"2026-10-20T10:00:00Z"}"#,
        r#"{"sql":"SELECT * FROM items", "ms":3, "error_code":0}"#,
        r#"{"sql":"SELECT * FROM items", "ms":2, "error":false}"#,
        r#"{"sql":"INSERT INTO items(name) VALUES ('secret')", "error_code":2067}"#,
    ];
    let (format, aggregator) = culprit_querylog::read(lines, Engine::Sqlite).unwrap();
    let failures = aggregator.failures();
    assert_eq!(failures.iter().map(|e| e.count).sum::<f64>(), 3.0);
    let busy = failures.iter().find(|e| e.code == "SQLITE_BUSY").unwrap();
    assert_eq!(busy.count, 2.0);
    assert_eq!(busy.last_at.as_deref(), Some("2026-10-20T10:00:00Z"));
    assert!(failures.iter().any(|e| e.code == "SQLITE_CONSTRAINT"));
    let exported = serde_json::to_string(&failures).unwrap();
    assert!(!exported.contains("private-value") && !exported.contains("secret"));
    let w = aggregator.workload(format, 50);
    assert_eq!((w.total_executions, w.total_time_ms), (2.0, 5.0));
    assert_eq!(w.queries.len(), 1);
}

#[test]
fn request_counts_need_explicit_nonempty_ids_and_never_export_them() {
    let mut lines = Vec::new();
    for group in 0..3 {
        for value in 0..12 {
            lines.push(format!(
                r#"{{"sql":"SELECT * FROM items WHERE id = {value}","ms":1,"request_id":"private-request-{group}"}}"#
            ));
        }
    }
    for _ in 0..12 {
        lines.push(r#"{"sql":"SELECT * FROM items WHERE id = 1","ms":1,"request_id":""}"#.into());
    }
    let (format, aggregator) = culprit_querylog::read(lines.iter().map(String::as_str), Engine::Sqlite).unwrap();
    let w = aggregator.workload(format, 50);
    let r = w.queries[0].requests.as_ref().unwrap();
    assert_eq!((r.requests, r.repeated_requests, r.most_calls), (3, 3, 12));
    assert!(!serde_json::to_string(&w).unwrap().contains("private-request"));
}

#[test]
fn unrelated_trace_fields_do_not_establish_request_repetition_or_change_observed_work() {
    let records: Vec<_> = (0..12)
        .map(|call| {
            serde_json::json!({
                "sql": format!("SELECT * FROM items WHERE id = {call}"),
                "ms": call + 1,
                "at": format!("2026-10-15T12:00:{call:02}Z"),
            })
        })
        .collect();
    let plain: Vec<_> = records.iter().map(serde_json::Value::to_string).collect();
    let (format, aggregator) = culprit_querylog::read(plain.iter().map(String::as_str), Engine::Sqlite).unwrap();
    let baseline = aggregator.workload(format, 50);
    assert_eq!((baseline.total_executions, baseline.total_time_ms, baseline.queries.len()), (12.0, 78.0, 1));
    assert!(baseline.queries[0].requests.is_none());

    for trace_key in ["trace_id", "traceId"] {
        let decorated: Vec<_> = records
            .iter()
            .map(|record| {
                let mut record = record.clone();
                record[trace_key] = serde_json::json!("private-trace-shared-by-several-requests");
                record.to_string()
            })
            .collect();
        let (format, aggregator) =
            culprit_querylog::read(decorated.iter().map(String::as_str), Engine::Sqlite).unwrap();
        assert_eq!(aggregator.entries(), 12);
        assert!(!aggregator.limited());
        let observed = aggregator.workload(format, 50);
        assert!(observed.queries.iter().all(|query| query.requests.is_none()), "{trace_key}");
        assert_eq!(serde_json::to_value(&observed).unwrap(), serde_json::to_value(&baseline).unwrap(), "{trace_key}");
    }
}

#[test]
fn span_only_json_is_unrecognized_and_cannot_add_work_to_an_ordinary_log() {
    let spans = [
        r#"{"startTimeUnixNano":"1760529600000000000","endTimeUnixNano":"1760529600012500000","attributes":{"db.statement":"SELECT * FROM orders"}}"#,
        r#"{"duration_ms":12.5,"spanAttributes":[{"key":"db.query.text","value":{"stringValue":"SELECT * FROM orders"}}]}"#,
    ];
    assert!(culprit_querylog::read(spans, Engine::Sqlite).is_none());
    let ordinary = r#"{"fields":{"sql":"SELECT * FROM orders WHERE id = 1","duration_ms":2,"request_id":"explicit-request"},"timestamp":"2026-10-15T12:00:00Z"}"#;
    let (format, aggregator) = culprit_querylog::read([spans[0], ordinary, spans[1]], Engine::Sqlite).unwrap();
    assert_eq!(format, "JSON lines");
    assert_eq!(aggregator.entries(), 1);
    assert!(aggregator.failures().is_empty());
    let workload = aggregator.workload(format, 50);
    assert_eq!((workload.total_executions, workload.total_time_ms), (1.0, 2.0));
    assert_eq!(workload.queries.len(), 1);
}

#[test]
fn explicit_request_aliases_keep_repetition_counts_when_a_trace_is_also_present() {
    for request_key in ["request_id", "requestId", "http.request_id"] {
        let mut lines = Vec::new();
        for request in 0..3 {
            for call in 0..12 {
                let mut record = serde_json::json!({
                    "sql": format!("SELECT * FROM items WHERE id = {call}"),
                    "ms": 1,
                    "trace_id": "private-shared-trace",
                    "traceId": "another-private-trace",
                });
                record[request_key] = serde_json::json!(format!("private-explicit-request-{request}"));
                lines.push(record.to_string());
            }
        }
        let (format, aggregator) = culprit_querylog::read(lines.iter().map(String::as_str), Engine::Sqlite).unwrap();
        let workload = aggregator.workload(format, 50);
        assert_eq!((workload.total_executions, workload.total_time_ms, workload.queries.len()), (36.0, 36.0, 1));
        let evidence = workload.queries[0].requests.as_ref().unwrap();
        assert_eq!((evidence.requests, evidence.repeated_requests, evidence.most_calls), (3, 3, 12), "{request_key}");
        assert!(!evidence.sample_truncated);
        let exported = serde_json::to_string(&workload).unwrap();
        assert!(!exported.contains("private-explicit-request") && !exported.contains("private-shared-trace"));
    }
}
