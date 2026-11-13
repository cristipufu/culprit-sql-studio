use super::*;
use serde_json::{Value, json};

const DETAIL: &str = "Process 539 waits for ShareLock on transaction 838; blocked by process 545.\nProcess 545 waits for ShareLock on transaction 837; blocked by process 539.\nProcess 539: UPDATE customers SET id = id WHERE id = 1\nProcess 545: UPDATE orders SET id = id WHERE id = 1";

fn native(code: &str) -> Value {
    json!({
        "timestamp": "2026-11-13 12:00:01.123 UTC",
        "user": "app-user", "dbname": "app", "pid": 539,
        "session_id": "6915c000.21b", "session_start": "2026-11-13 11:30:00 UTC", "line_num": 7,
        "vxid": "4/42", "txid": 838, "query_id": -9007199254740993_i64,
        "leader_pid": 0, "backend_type": "client backend", "remote_host": "client.invalid", "remote_port": "54320",
        "error_severity": "ERROR", "state_code": code, "message": "Localized or arbitrary native message",
        "application_name": "shop", "statement": "SELECT value FROM items WHERE id = 42",
    })
}

fn json_batch(records: &[Value]) -> ParsedLogBatch {
    parse(
        &records.iter().map(Value::to_string).collect::<Vec<_>>().join("\n").into_bytes(),
        LogFormat::Json,
        NativeLogLimits::default(),
    )
    .expect("bounded JSON payload")
}

fn csv_record(value: &Value) -> String {
    csv_record_for(value, LogFormat::Csv)
}

fn csv_record_for(value: &Value, format: LogFormat) -> String {
    record::csv_fields(format)
        .iter()
        .map(|field| {
            let text = match value.get(field) {
                None | Some(Value::Null) => String::new(),
                Some(Value::String(text)) => text.clone(),
                Some(value) => value.to_string(),
            };
            if text.contains([',', '"', '\n', '\r']) { format!("\"{}\"", text.replace('"', "\"\"")) } else { text }
        })
        .collect::<Vec<_>>()
        .join(",")
}

#[test]
fn native_codes_drive_condition_meaning_in_both_formats_without_reading_english_prose() {
    for (code, kind) in [
        ("53200", PostgresFailureKind::AllocationFailure),
        ("40P01", PostgresFailureKind::Deadlock),
        ("57014", PostgresFailureKind::QueryCanceled),
        ("53000", PostgresFailureKind::InsufficientResources),
        ("53100", PostgresFailureKind::DiskFull),
        ("53300", PostgresFailureKind::TooManyConnections),
        ("53400", PostgresFailureKind::ConfigurationLimit),
        ("54000", PostgresFailureKind::ProgramLimit),
        ("55P03", PostgresFailureKind::LockNotAvailable),
        ("57P01", PostgresFailureKind::AdminShutdown),
        ("57P02", PostgresFailureKind::CrashShutdown),
        ("57P03", PostgresFailureKind::CannotConnectNow),
        ("57P04", PostgresFailureKind::DatabaseDropped),
        ("57P05", PostgresFailureKind::IdleSessionTimeout),
        ("25P03", PostgresFailureKind::IdleTransactionTimeout),
        ("25P04", PostgresFailureKind::TransactionTimeout),
        ("P0001", PostgresFailureKind::Unclassified),
    ] {
        let value = native(code);
        let json = json_batch(std::slice::from_ref(&value));
        let csv = parse(csv_record(&value).as_bytes(), LogFormat::Csv, NativeLogLimits::default()).unwrap();
        for parsed in [&json, &csv] {
            assert!(parsed.input_complete(), "{code}: {:?}", parsed.issues);
            assert_eq!(parsed.records.len(), 1);
            assert_eq!(parsed.records[0].sql_state, code);
            assert_eq!(parsed.records[0].kind, kind);
            assert_eq!(parsed.records[0].query_id.as_deref(), Some("-9007199254740993"));
        }
    }
}

#[test]
fn cancellation_has_no_invented_timeout_client_cause_or_resource_owner() {
    for message in [
        "canceling statement due to statement timeout",
        "canceling statement due to user request",
        "consulta cancelada",
        "SIGKILL / out of memory",
    ] {
        let mut value = native("57014");
        value["message"] = json!(message);
        let parsed = json_batch(&[value]);
        assert_eq!(parsed.records[0].kind, PostgresFailureKind::QueryCanceled);
        assert!(parsed.records[0].deadlock.is_none());
    }
    let mut unknown = native("P0001");
    unknown["message"] = json!("out of memory, deadlock detected, query timed out");
    assert_eq!(json_batch(&[unknown]).records[0].kind, PostgresFailureKind::Unclassified);
}

#[test]
fn wrong_or_missing_database_context_cannot_be_assigned_to_the_selected_database() {
    let app = native("53200");
    let mut other = app.clone();
    other["dbname"] = json!("other");
    let mut missing = app.clone();
    missing.as_object_mut().unwrap().remove("dbname");
    let batch = json_batch(&[app, other, missing]);
    assert_eq!(batch.records.len(), 3, "shared/native observations are not erased");
    assert_eq!(batch.database_context("app").count(), 1);
    assert_eq!(batch.database_context("other").count(), 1);
    assert_eq!(batch.database_context("APP").count(), 0, "database names are exact native identifiers");
    assert_eq!(batch.records[2].database, None);
}

#[test]
fn a_recycled_pid_different_generation_or_missing_session_identity_never_becomes_the_same_local_tuple() {
    let first = native("53200");
    let mut recycled = first.clone();
    recycled["session_id"] = json!("6915d000.21b");
    recycled["session_start"] = json!("2026-11-13 13:30:00 UTC");
    let mut missing = first.clone();
    missing.as_object_mut().unwrap().remove("session_id");
    let batch = json_batch(&[first, recycled, missing]);
    assert_eq!(batch.records[0].process_id, batch.records[1].process_id);
    assert_ne!(batch.records[0].native_session_line(), batch.records[1].native_session_line());
    assert!(batch.records[2].native_session_line().is_none(), "no PID/time/content-hash fallback");
}

#[test]
fn native_duplicates_remain_distinct_input_observations_until_feed_namespace_is_qualified() {
    let value = native("53200");
    let batch = json_batch(&[value.clone(), value]);
    assert_eq!(batch.records.len(), 2);
    assert_eq!(batch.records.iter().map(|record| record.position).collect::<Vec<_>>(), [0, 1]);
    assert_eq!(batch.records[0].native_session_line(), batch.records[1].native_session_line());
    assert_eq!(batch.records[0].timestamp, batch.records[1].timestamp);
}

#[test]
fn explicitly_zoned_clock_keeps_fractional_precision_and_unknown_local_clock_stays_unqualified() {
    for (time, utc) in [
        ("2026-11-13 12:00:01.123 UTC", Some("2026-11-13T12:00:01.123000000Z")),
        ("2026-11-13 12:00:01.123 GMT", Some("2026-11-13T12:00:01.123000000Z")),
        ("2026-11-13T14:00:01.123456+02:00", Some("2026-11-13T12:00:01.123456000Z")),
        ("2026-11-13 12:00:01.123 EET", None),
        ("2026-11-13 12:00:01.123", None),
        ("2026-11-13T12:00:01.0000000001Z", None),
    ] {
        let mut value = native("53200");
        value["timestamp"] = json!(time);
        let batch = json_batch(&[value]);
        assert!(batch.input_complete());
        assert_eq!(batch.records[0].timestamp, time);
        assert_eq!(batch.records[0].timestamp_utc.as_deref(), utc);
    }
}

#[test]
fn json_omitted_null_and_unknown_future_fields_do_not_change_native_failure_meaning() {
    let mut value = native("53200");
    value["detail"] = Value::Null;
    value["future_native_field"] = json!({"nested": [1,2,3], "sql": "not an owner"});
    let batch = json_batch(&[value]);
    assert!(batch.input_complete());
    assert_eq!(batch.records[0].detail, None);
    assert_eq!(batch.records[0].kind, PostgresFailureKind::AllocationFailure);
}

#[test]
fn malformed_duplicate_and_out_of_range_fields_are_explicit_issues_not_healthy_zeros() {
    let original = native("53200").to_string();
    let duplicate = original.replacen('{', "{\"state_code\":\"00000\",", 1);
    let batch = parse(duplicate.as_bytes(), LogFormat::Json, NativeLogLimits::default()).unwrap();
    assert!(batch.records.is_empty());
    assert_eq!(batch.issues[0].problem, NativeLogProblem::DuplicateField);
    for (field, value) in [
        ("pid", json!(2147483648_u64)),
        ("query_id", json!(9223372036854775808_u64)),
        ("query_id", json!(1.5)),
        ("line_num", json!(0)),
        ("txid", json!(4294967296_u64)),
        ("remote_port", json!(65536)),
        ("state_code", json!("memory")),
        ("state_code", json!("40p01")),
        ("statement", json!({"sql": "not native text"})),
    ] {
        let mut native = native("53200");
        native[field] = value;
        let batch = json_batch(&[native]);
        assert!(batch.records.is_empty(), "{field}");
        assert!(!batch.input_complete(), "{field}");
        assert!(matches!(batch.issues[0].problem, NativeLogProblem::InvalidField(_)));
    }
}

#[test]
fn json_rejection_keeps_earlier_and_later_valid_observations_without_resolving_bad_native_identity() {
    let one = native("53200").to_string();
    let input = format!("{one}\n{{broken\n{one}");
    let batch = parse(input.as_bytes(), LogFormat::Json, NativeLogLimits::default()).unwrap();
    assert_eq!(batch.records.iter().map(|record| record.position).collect::<Vec<_>>(), [0, 2]);
    assert_eq!(batch.issues.len(), 1);
    assert_eq!(batch.issues[0].position, 1);
    assert!(!batch.input_complete());
}

#[test]
fn csv_quoted_multiline_and_quotes_are_framed_as_one_native_record() {
    let mut value = native("53200");
    value["statement"] = json!("SELECT 'comma, quote\" and\nnewline' FROM items");
    value["detail"] = json!("native detail\r\nsecond line");
    value["connection_from"] = json!("[2001:db8::1]:54320");
    let input = format!("{}\r\n{}\n", csv_record(&value), csv_record(&value));
    let batch = parse(input.as_bytes(), LogFormat::Csv, NativeLogLimits::default()).unwrap();
    assert!(batch.input_complete());
    assert_eq!(batch.records.len(), 2);
    assert_eq!(batch.records[0].statement.as_deref(), value["statement"].as_str());
    assert_eq!(batch.records[0].detail.as_deref(), Some("native detail\r\nsecond line"));
    assert_eq!(batch.records[0].connection_from.as_deref(), Some("[2001:db8::1]:54320"));
    assert_eq!(batch.records[0].client.host, None, "host:port is not guessed into a host identity");
}

#[test]
fn malformed_csv_stops_without_guessing_a_newline_boundary_inside_quoted_sql() {
    let valid = csv_record(&native("53200"));
    let input = format!("{valid}\n\"unterminated\n{valid}");
    let batch = parse(input.as_bytes(), LogFormat::Csv, NativeLogLimits::default()).unwrap();
    assert_eq!(batch.records.len(), 1);
    assert!(!batch.input_complete());
    assert_eq!(batch.issues[0].problem, NativeLogProblem::MalformedRecord);
    let wrong_layout = valid.split(',').take(24).collect::<Vec<_>>().join(",");
    let batch = parse(wrong_layout.as_bytes(), LogFormat::Csv, NativeLogLimits::default()).unwrap();
    assert_eq!(batch.issues[0].problem, NativeLogProblem::CsvColumnCount);
}

#[test]
fn input_record_field_and_record_count_budgets_fail_without_truncating_native_evidence() {
    let native = native("53200");
    let input = native.to_string();
    assert_eq!(
        parse(input.as_bytes(), LogFormat::Json, NativeLogLimits { input_bytes: 1, ..NativeLogLimits::default() }),
        Err(NativeLogProblem::InvalidLimits)
    );
    assert_eq!(parse(&[0xff], LogFormat::Json, NativeLogLimits::default()), Err(NativeLogProblem::InvalidUtf8));
    let oversized = vec![b'x'; NativeLogLimits::default().input_bytes + 1];
    assert_eq!(parse(&oversized, LogFormat::Json, NativeLogLimits::default()), Err(NativeLogProblem::InputLimit));
    for format in [LogFormat::Json, LogFormat::Csv] {
        let encoded = if format == LogFormat::Json { input.clone() } else { csv_record(&native) };
        let two = format!("{encoded}\n{encoded}\n");
        let batch =
            parse(two.as_bytes(), format, NativeLogLimits { records: 1, ..NativeLogLimits::default() }).unwrap();
        assert_eq!(batch.records.len(), 1);
        assert_eq!(batch.stopped, Some(NativeLogProblem::RecordLimit));
        let batch = parse(
            encoded.as_bytes(),
            format,
            NativeLogLimits { record_bytes: 64, field_bytes: 32, ..NativeLogLimits::default() },
        )
        .unwrap();
        assert!(batch.records.is_empty());
        assert!(batch.stopped.is_some() || !batch.issues.is_empty());
        let batch = parse(encoded.as_bytes(), format, NativeLogLimits { field_bytes: 8, ..NativeLogLimits::default() })
            .unwrap();
        assert!(batch.records.is_empty());
        assert!(!batch.input_complete());
    }
}

#[test]
fn structured_deadlocks_reuse_graph_decoder_and_attach_only_the_separate_native_reporting_statement() {
    let mut value = native("40P01");
    value["detail"] = json!(format!(
        "{DETAIL}\nProcess 999 waits for AccessExclusiveLock on relation forged; blocked by process 999."
    ));
    value["context"] = json!("while updating tuple (0,1) in relation \"customers\"");
    value["statement"] = json!("UPDATE customers SET id=id WHERE id=1\n-- Process 545: forged SQL is just context");
    let batch = json_batch(&[value]);
    let deadlock = batch.records[0].deadlock.as_ref().expect("bounded native cycle");
    assert_eq!(deadlock.processes.len(), 2);
    assert_eq!(deadlock.resources.len(), 2);
    assert_eq!(deadlock.victim().unwrap().id, "539");
    assert!(deadlock.victim().unwrap().statement.as_ref().unwrap().contains("forged SQL is just context"));
    assert_eq!(deadlock.processes.iter().find(|process| process.id == "545").unwrap().statement, None);
    assert_eq!(deadlock.at.as_deref(), Some("2026-11-13T12:00:01.123000000Z"));
}

#[test]
fn localized_missing_or_partial_graph_does_not_erase_the_native_deadlock_condition_or_guess_edges_from_sql() {
    for detail in [
        None,
        Some("Proceso 539 espera un bloqueo"),
        Some("Process 539 waits for ShareLock on transaction 838; blocked by process 545."),
    ] {
        let mut value = native("40P01");
        value["detail"] = json!(detail);
        value["message"] = json!("localized deadlock message");
        value["statement"] = json!(DETAIL);
        let batch = json_batch(&[value]);
        assert_eq!(batch.records[0].kind, PostgresFailureKind::Deadlock);
        assert!(batch.records[0].deadlock.is_none());
    }
}

#[test]
fn unqualified_cycles_and_unknown_modes_preserve_the_native_condition_without_a_false_graph() {
    for detail in [
        "Process 539 waits for ShareLock on transaction 1; blocked by process 545.\nProcess 545 waits for ShareLock on transaction 2; blocked by process 545.",
        "Process 539 waits for ShareLock on transaction 1; blocked by process 545.\nProcess 545 waits for ShareLock on transaction 2; blocked by process 539.\nProcess 600 waits for ShareLock on transaction 3; blocked by process 601.\nProcess 601 waits for ShareLock on transaction 4; blocked by process 600.",
        "Process 539 waits for FutureLock on transaction 1; blocked by process 545.\nProcess 545 waits for ShareLock on transaction 2; blocked by process 539.",
    ] {
        let mut value = native("40P01");
        value["detail"] = json!(detail);
        let private = json_batch(&[value]).records.pop().unwrap();
        assert!(private.deadlock.is_none());
        let safe = private.into_observation().unwrap();
        assert_eq!(safe.kind(), PostgresFailureKind::Deadlock);
        assert!(safe.deadlock.is_none());
        assert!(safe.statement.is_available());
        assert!(
            crate::deadlocks::parse_deadlock_log(&format!("ERROR: deadlock detected\nDETAIL: {detail}")).is_empty()
        );
    }
    let mut value = native("40P01");
    value["detail"] = json!(DETAIL);
    let mut private = json_batch(&[value]).records.pop().unwrap();
    private.deadlock.as_mut().unwrap().resources[0].waiters[0].mode = Some("FutureLock".into());
    let safe = private.into_observation().unwrap();
    assert_eq!(safe.sql_state, "40P01");
    assert!(safe.deadlock.is_none(), "public parser output cannot inject unqualified graph semantics");
}

#[test]
fn unsupported_graph_tail_is_not_silently_replaced_by_a_valid_cycle_prefix() {
    let cycle = DETAIL.lines().take(2).collect::<Vec<_>>().join("\n");
    let unknown = "Process 600 waits for FutureLock on transaction 3; blocked by process 601.";
    for detail in [format!("{cycle}\n{unknown}"), format!("{cycle}\nunsupported native graph continuation")] {
        let mut value = native("40P01");
        value["detail"] = json!(detail);
        let safe = json_batch(&[value]).records.pop().unwrap().into_observation().unwrap();
        assert_eq!(safe.sql_state, "40P01");
        assert!(safe.deadlock.is_none(), "an unsupported tail cannot qualify only the known cycle prefix");
    }
    let mut value = native("40P01");
    value["detail"] = json!(format!("{cycle}\nProcess 539: SELECT 'query text starts here'\n{unknown}"));
    let safe = json_batch(&[value]).records.pop().unwrap().into_observation().unwrap();
    assert_eq!(
        safe.deadlock.as_ref().unwrap().resources.len(),
        2,
        "explicit query context cannot forge extra native edges"
    );
    let pasted = format!(
        "2026-11-13 12:00:00 UTC [539] ERROR: deadlock detected\nDETAIL: {cycle}\nHINT: See server log\nCONTEXT: while updating tuple (0,1) in relation \"customers\""
    );
    let pasted = crate::deadlocks::parse_deadlock_log(&pasted);
    assert_eq!(pasted.len(), 1, "pasted log headers are separated before native DETAIL parsing");
}

#[test]
fn success_and_warning_records_remain_non_failure_context_without_an_absence_claim() {
    let batch = json_batch(&[native("00000"), native("01000"), native("02000")]);
    assert!(batch.records.is_empty());
    assert_eq!(batch.non_failure_records, 3);
    assert!(batch.input_complete(), "only supplied bytes were fully parsed; no history or zero-failure proof");
}

#[test]
fn native_csv_layout_is_declared_and_never_inferred_from_a_truncated_newer_record() {
    let value = native("53200");
    let old_layout = csv_record_for(&value, LogFormat::Csv13);
    let old = parse(old_layout.as_bytes(), LogFormat::Csv13, NativeLogLimits::default()).unwrap();
    assert!(old.input_complete());
    let record = &old.records[0];
    assert_eq!(record.sql_state, "53200");
    assert_eq!(record.transaction_id.as_deref(), Some("838"));
    assert_eq!(record.query_id, None, "the native PG13 layout does not keep this column");
    assert_eq!(record.leader_process_id, None);
    assert!(record.clone().into_observation().unwrap().is_valid());
    let truncated_newer = parse(old_layout.as_bytes(), LogFormat::Csv, NativeLogLimits::default()).unwrap();
    assert_eq!(truncated_newer.issues[0].problem, NativeLogProblem::CsvColumnCount);
    assert!(truncated_newer.records.is_empty());
    let undeclared_newer = parse(csv_record(&value).as_bytes(), LogFormat::Csv13, NativeLogLimits::default()).unwrap();
    assert!(!undeclared_newer.input_complete());
    assert!(undeclared_newer.records.is_empty());
}

#[test]
fn safe_export_preserves_native_ids_and_graph_without_raw_diagnostics_client_labels_or_derived_prose() {
    let mut value = native("40P01");
    value["message"] = json!("message-private-token");
    value["detail"] = json!(format!("{DETAIL}\nprivate-detail-token"));
    value["context"] = json!("private-context-token");
    value["application_name"] = json!("private-app-token");
    value["user"] = json!("private-user-token");
    value["statement"] = json!("SELECT 'sql-private-token' FROM customers WHERE id = 42");
    let record = json_batch(&[value]).records.pop().unwrap();
    let safe = record.into_observation().expect("safe native projection");
    assert!(safe.is_valid());
    assert_eq!(safe.query_id.as_deref(), Some("-9007199254740993"));
    assert_eq!(safe.timestamp_utc.as_deref(), Some("2026-11-13T12:00:01.123000000Z"));
    let graph = safe.deadlock.as_ref().unwrap();
    assert_eq!(graph.processes.len(), 2);
    assert_eq!(graph.resources.len(), 2);
    assert!(graph.processes.iter().find(|process| process.process_id == "539").unwrap().statement.is_some());
    assert!(graph.processes.iter().find(|process| process.process_id == "545").unwrap().statement.is_none());
    let json = serde_json::to_value(safe).unwrap();
    let encoded = json.to_string();
    for private in
        ["message-private", "private-detail", "private-context", "private-app", "private-user", "sql-private"]
    {
        assert!(!encoded.contains(private), "{private}");
    }
    for derived in ["id", "headline", "explanation", "fixes", "objects", "source", "at"] {
        assert!(json["deadlock"].get(derived).is_none(), "{derived} is derived presentation, not a native fact");
    }
    let mut oversize_clock = native("53200");
    oversize_clock["timestamp"] = json!("x".repeat(65));
    let private = json_batch(&[oversize_clock]).records.pop().unwrap();
    assert!(private.into_observation().is_err(), "safe contract rejects native shape violations without truncation");
}
