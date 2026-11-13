use super::*;

fn row(position: u32) -> MysqlErrorLogObservation {
    MysqlErrorLogObservation {
        position,
        logged_at: "2026-11-13T12:00:00.123456Z".into(),
        thread_id: Some(u64::MAX.to_string()),
        priority: "Error".into(),
        error_code: Some("MY-010935".into()),
        subsystem: Some("Server".into()),
        message: Some("private-application-value-canary".into()),
    }
}

fn resource() -> SharedResourceIdentity {
    SharedResourceIdentity {
        engine: crate::Engine::Mysql,
        kind: SharedResourceKind::Instance,
        namespace: NativeIdentity::new("fixture-native-server"),
        native: NativeIdentity::new("fixture-native-uuid"),
    }
}

fn snapshot(observations: Vec<NativeFailureRecord>) -> NativeFailureSnapshot {
    NativeFailureSnapshot {
        source: crate::source_id::NATIVE_FAILURES.into(),
        observed_from: "2026-11-13T12:00:00Z".into(),
        observed_until: "2026-11-13T12:00:01Z".into(),
        resource: Some(resource()),
        observations,
    }
}

#[test]
fn exact_unsigned_native_connection_ids_are_strings_and_not_recycled_thread_joins() {
    let original = row(0);
    assert!(original.is_valid());
    let json = serde_json::to_value(&original).unwrap();
    assert_eq!(json["thread_id"], u64::MAX.to_string());
    let restored: MysqlErrorLogObservation = serde_json::from_value(json).unwrap();
    assert_eq!(restored, original);
    for value in ["0", "9007199254740993", "18446744073709551615"] {
        assert!(MysqlErrorLogObservation { thread_id: Some(value.into()), ..original.clone() }.is_valid());
    }
    for value in ["-1", "01", "+1", "1.0", "18446744073709551616", ""] {
        assert!(!MysqlErrorLogObservation { thread_id: Some(value.into()), ..original.clone() }.is_valid());
    }
    assert!(MysqlErrorLogObservation { thread_id: None, ..original }.is_valid());
}

#[test]
fn native_timestamp_bounds_microseconds_and_explicit_utc_share_one_conversion() {
    let minimum = 1_000_000;
    let maximum = (i32::MAX as u64) * 1_000_000 + 999_999;
    assert_eq!(MysqlErrorLogObservation::logged_at_from_microseconds(minimum).unwrap(), "1970-01-01T00:00:01.000000Z");
    assert_eq!(MysqlErrorLogObservation::logged_at_from_microseconds(maximum).unwrap(), "2038-01-19T03:14:07.999999Z");
    for micros in [minimum, minimum + 1, maximum - 1, maximum] {
        let value = MysqlErrorLogObservation::logged_at_from_microseconds(micros).unwrap();
        assert!(MysqlErrorLogObservation { logged_at: value, ..row(0) }.is_valid());
    }
    for micros in [0, minimum - 1, maximum + 1, u64::MAX] {
        assert!(MysqlErrorLogObservation::logged_at_from_microseconds(micros).is_err());
    }
    for value in [
        "1970-01-01T00:00:00Z",
        "2099-01-01T00:00:00Z",
        "2026-11-13T12:00:00",
        "2026-11-13T13:00:00+01:00",
        "2026-11-13T12:00:00.1234567Z",
        "2016-12-31T23:59:60.000000Z",
    ] {
        assert!(!MysqlErrorLogObservation { logged_at: value.into(), ..row(0) }.is_valid(), "{value}");
    }
}

#[test]
fn capture_positions_preserve_equal_rows_without_a_cross_capture_occurrence_claim() {
    let first = row(0);
    let second = row(1);
    assert_ne!(first.key().unwrap(), second.key().unwrap());
    let observed =
        snapshot(vec![NativeFailureRecord::MysqlErrorLog(first.clone()), NativeFailureRecord::MysqlErrorLog(second)]);
    assert!(observed.is_valid());
    let duplicated =
        snapshot(vec![NativeFailureRecord::MysqlErrorLog(first.clone()), NativeFailureRecord::MysqlErrorLog(first)]);
    assert!(!duplicated.is_valid());
    let invalid = row(MAX_NATIVE_FAILURE_OBSERVATIONS as u32);
    assert!(invalid.key().is_err());
    assert!(!invalid.is_valid());
}

#[test]
fn a_shared_diagnostic_record_cannot_acquire_member_database_or_pool_ownership() {
    let mut observed = snapshot(vec![NativeFailureRecord::MysqlErrorLog(row(0))]);
    assert!(observed.is_valid());
    for kind in [SharedResourceKind::Pool, SharedResourceKind::Node, SharedResourceKind::Cluster] {
        observed.resource.as_mut().unwrap().kind = kind;
        assert!(!observed.is_valid());
    }
    observed.resource.as_mut().unwrap().kind = SharedResourceKind::Instance;
    observed.resource.as_mut().unwrap().engine = crate::Engine::Postgres;
    assert!(!observed.is_valid());
    observed.resource = None;
    assert!(observed.is_valid(), "unowned hosting context does not substitute a member database");
    observed.observed_until = "2026-11-13T11:59:59Z".into();
    assert!(!observed.is_valid());
}

#[test]
fn default_telemetry_export_omits_free_form_messages_before_authenticating_the_native_row() {
    let native = row(0);
    let record = TelemetryRecord::Operational(OperationalObservation {
        source: crate::source_id::NATIVE_FAILURES.into(),
        key: native.key().unwrap(),
        fact: OperationalFact::MysqlErrorLog(Box::new(native.clone())),
    });
    let source = CollectionDescription {
        source: "native-server-diagnostics".into(),
        target: CollectionTarget::Shared(resource()),
        continuity: SourceContinuity::Unverified,
        observation: SourceObservation::Snapshot(StateSnapshotDescription {
            dataset: SnapshotDataset::NativeFailures,
            observed_from: "2026-11-13T12:00:00Z".into(),
            observed_until: "2026-11-13T12:00:01Z".into(),
            records: 1,
            sources: vec![StateSourceCoverage::collected(crate::source_id::NATIVE_FAILURES.into(), 1)],
        }),
    };
    let capture = CaptureIdentity { observation_id: "fixture-capture".into(), revision: None };
    let exported = ExportedRecord::new(&source, &capture, record).unwrap();
    assert!(!serde_json::to_string(&exported).unwrap().contains("private-application-value-canary"));
    let TelemetryRecord::Operational(observation) = &exported.record else { panic!() };
    let OperationalFact::MysqlErrorLog(safe) = &observation.fact else { panic!() };
    assert_eq!(safe.message, None);
    assert_eq!(safe.thread_id, native.thread_id);
    assert_eq!(safe.logged_at, native.logged_at);
    assert_eq!(safe.error_code, native.error_code);
    assert!(exported.record.definition_payloads().next().is_none());
    assert!(native.message.is_some(), "caller-owned native input is unchanged");
}

#[test]
fn new_native_failure_collection_is_independent_of_other_opt_ins_and_windows() {
    let options = crate::SnapshotOptions::default();
    assert!(!options.collect_native_failures);
    let options = crate::SnapshotOptions {
        collect_shared_resources: true,
        collect_request_memory: true,
        collect_memory_failures: true,
        ..options
    };
    assert!(!options.collect_native_failures);
    let enabled = crate::SnapshotOptions { collect_native_failures: true, ..options }.with_window_minutes(90);
    assert!(enabled.collect_native_failures);
    assert_eq!(enabled.window_minutes, 90);
    let operations =
        crate::operations::OperationalStats { native_failures: Some(snapshot(vec![])), ..Default::default() };
    assert!(serde_json::to_value(&operations).unwrap().get("native_failures").is_some());
    assert!(serde_json::to_value(operations.counters_only()).unwrap().get("native_failures").is_none());
}
