use super::*;
use culprit_core::telemetry::{DefinitionPayload, PostgresFailureKind};
use serde_json::{Value, json};
use std::io::Write;

fn record(code: &str) -> Value {
    json!({
        "timestamp":"2020-01-01 01:02:03.123 UTC", "state_code":code, "error_severity":"ERROR",
        "dbname":"foreign-db", "pid":539, "session_id":"5e0be100.21b", "session_start":"2020-01-01 00:00:00 UTC", "line_num":7,
        "query_id":-9007199254740993_i64,"message":"private-error-token","detail":"private-detail-token",
        "context":"private-context-token","user":"private-user-token","remote_host":"private-host-token",
        "application_name":"private-app-token","statement":"SELECT 'private-sql-token' FROM items WHERE id=42"
    })
}
fn input(records: &[Value]) -> Vec<u8> {
    records.iter().map(Value::to_string).collect::<Vec<_>>().join("\n").into_bytes()
}
fn analyze(input: &[u8]) -> NativeLogAnalysis {
    analyze_bytes(input, NativeLogFormat::PostgresJson, NativeLogLimits::default()).unwrap()
}

#[test]
fn supplied_native_records_use_the_same_pure_view_and_omit_private_values_without_database_ownership() {
    let bytes = input(&[record("53200"), record("57014")]);
    let analysis = analyze(&bytes);
    assert!(analysis.is_read_complete());
    assert!(analysis.evidence.resource.is_none());
    assert_eq!(analysis.input.sha256, digest(&bytes));
    assert_eq!(analysis.input.bytes, bytes.len() as u64);
    assert_eq!(analysis.evidence.observations.len(), 2);
    assert_eq!(
        analysis.report,
        culprit_checks::native_failures::explain(&analysis.evidence, Some(&analysis.source)).unwrap()
    );
    let NativeFailureRecord::PostgresLog(row) = &analysis.evidence.observations[0] else { panic!("PG format") };
    assert_eq!(row.database.as_deref(), Some("foreign-db"));
    assert_eq!(row.query_id.as_deref(), Some("-9007199254740993"));
    assert_eq!(row.timestamp_utc.as_deref(), Some("2020-01-01T01:02:03.123000000Z"));
    assert_ne!(
        row.timestamp_utc.as_deref(),
        Some(analysis.evidence.observed_from.as_str()),
        "capture and event clocks are independent"
    );
    assert!(matches!(row.statement, DefinitionPayload::Available { .. }));
    assert_eq!(analysis.report.entries[1].title, t!("view.native_failures.postgres.canceled.title"));
    let serialized = serde_json::to_string(&analysis).unwrap();
    for private in [
        "private-error",
        "private-detail",
        "private-context",
        "private-user",
        "private-host",
        "private-app",
        "private-sql",
    ] {
        assert!(!serialized.contains(private), "{private}");
    }
}

#[test]
fn malformed_records_keep_prior_and_later_conditions_with_explicit_source_partial() {
    let first = record("53200").to_string();
    let mut recycled = record("57014");
    recycled["session_id"] = json!("5e0bf100.21b");
    recycled["session_start"] = json!("2020-01-01 02:00:00 UTC");
    recycled["message"] = json!("canceling statement due to statement timeout");
    let bytes = format!("{first}\n{{broken\n{recycled}").into_bytes();
    let analysis = analyze(&bytes);
    assert!(!analysis.is_read_complete());
    assert_eq!(analysis.source.state, SourceState::Partial);
    assert_eq!(analysis.records_examined, 3);
    assert_eq!(analysis.evidence.observations.iter().map(NativeFailureRecord::position).collect::<Vec<_>>(), [0, 2]);
    assert_eq!(analysis.issues[0].position, 1);
    assert_eq!(analysis.issues[0].problem, NativeLogProblem::MalformedRecord);
    let NativeFailureRecord::PostgresLog(second) = &analysis.evidence.observations[1] else { panic!("PG format") };
    assert_eq!(second.kind(), PostgresFailureKind::QueryCanceled);
    assert_eq!(second.process_id.as_deref(), Some("539"));
    assert_eq!(second.session_id.as_deref(), Some("5e0bf100.21b"));
    assert!(
        analysis.evidence.resource.is_none(),
        "reused PIDs and native database names do not establish a feed/resource"
    );
}

#[test]
fn equal_rows_and_unqualified_clocks_preserve_native_context_without_incident_deduplication() {
    let mut native = record("53200");
    native["timestamp"] = json!("2020-01-01 01:02:03 EET");
    let analysis = analyze(&input(&[native.clone(), native]));
    assert_eq!(analysis.evidence.observations.len(), 2);
    assert_eq!(analysis.report.entries.iter().map(|entry| entry.position).collect::<Vec<_>>(), [0, 1]);
    assert!(analysis.report.limitations.contains(&t!("view.native_failures.postgres.limit.clock")));
    for row in &analysis.evidence.observations {
        let NativeFailureRecord::PostgresLog(row) = row else { panic!("PG format") };
        assert!(row.timestamp_utc.is_none());
        assert_eq!(row.timestamp, "2020-01-01 01:02:03 EET");
    }
}

#[test]
fn native_deadlock_graph_is_explained_without_derived_ids_prose_or_another_participants_statement() {
    let mut row = record("40P01");
    row["detail"] = json!(
        "Process 539 waits for ShareLock on transaction 1; blocked by process 545.\nProcess 545 waits for ShareLock on transaction 2; blocked by process 539.\nProcess 545: SELECT 'private-other-query'"
    );
    let analysis = analyze(&input(&[row]));
    let NativeFailureRecord::PostgresLog(row) = &analysis.evidence.observations[0] else { panic!("PG format") };
    let graph = row.deadlock.as_ref().unwrap();
    assert_eq!(graph.processes.len(), 2);
    assert!(graph.processes.iter().find(|process| process.process_id == "545").unwrap().statement.is_none());
    assert_eq!(
        analysis.report.entries[0].explanation,
        t!("view.native_failures.postgres.deadlock.graph", pattern = graph.pattern.label())
    );
    assert!(!serde_json::to_string(&analysis).unwrap().contains("private-other-query"));
}

#[test]
fn byte_prefix_and_record_stops_keep_independent_provenance_and_positive_rows() {
    let line = record("53200").to_string();
    let bytes = format!("{line}\n{line}\n").into_bytes();
    let limits =
        NativeLogLimits { input_bytes: line.len() + 2, record_bytes: line.len() + 1, field_bytes: 128, records: 1 };
    let analysis = analyze_bytes(&bytes, NativeLogFormat::PostgresJson, limits).unwrap();
    assert_eq!(analysis.evidence.observations.len(), 1);
    assert!(!analysis.input.complete);
    assert_eq!(analysis.input.limitation, Some(NativeLogProblem::InputLimit));
    assert_eq!(analysis.stopped, Some(NativeLogProblem::RecordLimit));
    assert_eq!(analysis.input.sha256, digest(&bytes[..limits.input_bytes]));
    assert_ne!(analysis.input.sha256, digest(&bytes), "a prefix digest cannot identify the whole artifact");
    assert!(!analysis.is_read_complete());
    assert_eq!(analysis.source.state, SourceState::Partial);
}

#[test]
fn invalid_bytes_and_native_shape_are_partial_instead_of_empty_healthy_evidence() {
    let bad = analyze(&[0xff]);
    assert_eq!(bad.source.state, SourceState::Partial);
    assert_eq!(bad.stopped, Some(NativeLogProblem::InvalidUtf8));
    assert!(!bad.is_read_complete());
    assert!(bad.evidence.observations.is_empty());
    let mut bad_clock = record("53200");
    bad_clock["timestamp"] = json!("x".repeat(65));
    let partial = analyze(&input(&[bad_clock, record("53200")]));
    assert_eq!(partial.issues[0].problem, NativeLogProblem::NativeShape);
    assert_eq!(partial.evidence.observations[0].position(), 1);
    assert_eq!(partial.source.state, SourceState::Partial);
    assert!(
        analyze_bytes(&[], NativeLogFormat::PostgresJson, NativeLogLimits { records: 0, ..Default::default() })
            .is_err()
    );
    let empty = analyze(&[]);
    assert_eq!(empty.source.state, SourceState::Empty);
    assert!(empty.is_read_complete());
    assert!(
        empty.report.limitations.contains(&t!("view.native_failures.limit.coverage")),
        "complete local empty file proves no native absence"
    );
}

#[test]
fn selected_regular_file_and_supplied_bytes_follow_the_same_safe_parser_and_report() {
    let bytes = input(&[record("53200"), record("25P04")]);
    let mut file = tempfile::NamedTempFile::new().unwrap();
    file.write_all(&bytes).unwrap();
    file.flush().unwrap();
    let local = analyze_file(file.path(), NativeLogFormat::PostgresJson, NativeLogLimits::default()).unwrap();
    let supplied = analyze(&bytes);
    assert_eq!(local.input, supplied.input);
    assert_eq!(local.evidence.observations, supplied.evidence.observations);
    assert_eq!(local.report, supplied.report);
    assert!(local.is_read_complete());
    assert!(
        analyze_file(file.path().parent().unwrap(), NativeLogFormat::PostgresJson, NativeLogLimits::default()).is_err()
    );
    #[cfg(unix)]
    assert!(analyze_file(Path::new("/dev/null"), NativeLogFormat::PostgresJson, NativeLogLimits::default()).is_err());
    assert!(!serde_json::to_string(&local).unwrap().contains(&file.path().display().to_string()));
}

#[test]
fn selected_large_file_retains_a_bounded_prefix_without_claiming_native_feed_completeness() {
    let row = record("53200").to_string();
    let mut file = tempfile::NamedTempFile::new().unwrap();
    write!(file, "{row}\n{row}\n").unwrap();
    let limits =
        NativeLogLimits { input_bytes: row.len() + 1, record_bytes: row.len() + 1, field_bytes: 128, records: 1024 };
    let analysis = analyze_file(file.path(), NativeLogFormat::PostgresJson, limits).unwrap();
    assert_eq!(analysis.input.bytes, limits.input_bytes as u64);
    assert_eq!(analysis.evidence.observations.len(), 1);
    assert_eq!(analysis.input.limitation, Some(NativeLogProblem::InputLimit));
    assert_eq!(analysis.source.state, SourceState::Partial);
    assert!(!analysis.is_read_complete());
}

#[test]
fn declared_csv_native_versions_follow_the_public_analysis_route_without_inferred_layout() {
    let mut columns = vec![String::new(); 26];
    for (column, value) in [
        (0, "2020-01-01 01:02:03 UTC"),
        (2, "foreign-db"),
        (3, "539"),
        (5, "5e0be100.21b"),
        (6, "7"),
        (8, "2020-01-01 00:00:00 UTC"),
        (11, "ERROR"),
        (12, "53200"),
        (13, "private-error-token"),
        (19, "SELECT 42"),
        (23, "client backend"),
        (24, "0"),
        (25, "-9007199254740993"),
    ] {
        columns[column] = value.into();
    }
    for (format, length) in [(NativeLogFormat::PostgresCsv13, 24), (NativeLogFormat::PostgresCsv, 26)] {
        let encoded = columns[..length].join(",");
        let analysis = analyze_bytes(encoded.as_bytes(), format, NativeLogLimits::default()).unwrap();
        assert!(analysis.is_read_complete());
        assert_eq!(analysis.evidence.observations.len(), 1);
        let NativeFailureRecord::PostgresLog(row) = &analysis.evidence.observations[0] else { panic!("PG format") };
        assert_eq!(row.kind(), PostgresFailureKind::AllocationFailure);
        assert_eq!(row.query_id.as_deref(), if length == 26 { Some("-9007199254740993") } else { None });
    }
    let truncated = columns[..24].join(",");
    let analysis =
        analyze_bytes(truncated.as_bytes(), NativeLogFormat::PostgresCsv, NativeLogLimits::default()).unwrap();
    assert_eq!(analysis.source.state, SourceState::Partial);
    assert!(analysis.evidence.observations.is_empty());
    assert_eq!(analysis.issues[0].problem, NativeLogProblem::CsvColumnCount);
}
