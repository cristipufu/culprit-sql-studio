use super::*;
use crate::{Engine, MeasurementScope};
use serde_json::json;

fn observation() -> MssqlOutOfMemoryObservation {
    MssqlOutOfMemoryObservation {
        position: 0,
        event_time_without_offset: "2026-11-13T10:00:00.003".into(),
        oom_cause: 1,
        oom_cause_desc: "native reported cause".into(),
        oom_factor: 2,
        oom_factor_desc: "native diagnostic factor".into(),
        available_physical_memory_mb: -1,
        process_memory_usage_mb: i32::MAX,
        non_sos_memory_usage_mb: 15,
        committed_memory_target_mb: 20,
        committed_memory_mb: 18,
        allocation_potential_memory_mb: i32::MIN,
        initial_job_object_memory_limit_mb: None,
        current_job_object_memory_limit_mb: Some(-1),
        possible_non_sos_leaked_memory_mb: Some(0),
        oom_resource_pools: Some(MemoryFailureJsonDocument::from_raw("null").unwrap()),
        top_memory_clerks: Some(
            MemoryFailureJsonDocument::from_raw(
                r#"[{"clerk_type_name":"native-clerk","page_allocated_mb":-1,"future_member":{"value":null}}]"#,
            )
            .unwrap(),
        ),
        top_resource_pools: None,
        possible_leaked_memory_clerks: Some(MemoryFailureJsonDocument::from_raw("{unfinished").unwrap()),
    }
}

fn resource(kind: SharedResourceKind) -> SharedResourceIdentity {
    SharedResourceIdentity {
        engine: Engine::Mssql,
        kind,
        namespace: NativeIdentity::new("native-resource-namespace"),
        native: NativeIdentity::new("native-resource"),
    }
}

fn snapshot(observations: Vec<MssqlOutOfMemoryObservation>) -> MemoryFailureSnapshot {
    MemoryFailureSnapshot {
        source: crate::source_id::MEMORY_FAILURES.into(),
        observed_from: "2026-11-13T08:00:00Z".into(),
        observed_until: "2026-11-13T08:00:01Z".into(),
        scope: MeasurementScope::Server,
        resource: None,
        observations,
    }
}

fn source(records: u32) -> CollectionDescription {
    CollectionDescription {
        target: CollectionTarget::Shared(resource(SharedResourceKind::Instance)),
        source: crate::source_id::MEMORY_FAILURES.into(),
        continuity: SourceContinuity::Unverified,
        observation: SourceObservation::Snapshot(StateSnapshotDescription {
            dataset: SnapshotDataset::MemoryFailures,
            observed_from: "2026-11-13T08:00:00Z".into(),
            observed_until: "2026-11-13T08:00:01Z".into(),
            records,
            sources: vec![StateSourceCoverage::collected(crate::source_id::MEMORY_FAILURES.into(), records)],
        }),
    }
}

#[test]
fn signed_native_measurements_nulls_and_unknown_json_members_roundtrip_exactly() {
    let native = observation();
    assert!(native.is_valid());
    let restored: MssqlOutOfMemoryObservation = serde_json::from_value(serde_json::to_value(&native).unwrap()).unwrap();
    assert_eq!(restored, native);
    assert_eq!(restored.available_physical_memory_mb, -1);
    assert_eq!(restored.allocation_potential_memory_mb, i32::MIN);
    assert_eq!(restored.process_memory_usage_mb, i32::MAX);
    assert_eq!(restored.initial_job_object_memory_limit_mb, None);
    assert_eq!(restored.current_job_object_memory_limit_mb, Some(-1));
    assert_eq!(restored.possible_non_sos_leaked_memory_mb, Some(0));
    assert!(restored.top_memory_clerks.unwrap().raw.contains("future_member"));
}

#[test]
fn malformed_native_documents_preserve_scalar_evidence_and_have_explicit_qualification() {
    let native = observation();
    assert!(native.is_valid());
    assert_eq!(native.oom_resource_pools.as_ref().unwrap().qualification, MemoryFailureJsonQualification::Parsed);
    let malformed = native.possible_leaked_memory_clerks.as_ref().unwrap();
    assert_eq!(malformed.raw, "{unfinished");
    assert_eq!(malformed.qualification, MemoryFailureJsonQualification::Unparsed);
    for raw in ["", "not-json", "{} trailing", "{} []", "["] {
        let document = MemoryFailureJsonDocument::from_raw(raw).unwrap();
        assert!(document.is_valid());
        assert_eq!(document.raw, raw);
        assert_eq!(document.qualification, MemoryFailureJsonQualification::Unparsed);
    }
    assert!(snapshot(vec![native]).is_valid());
}

#[test]
fn sql_null_json_null_and_empty_native_document_are_distinct() {
    let null = MemoryFailureJsonDocument::from_raw("null").unwrap();
    let empty = MemoryFailureJsonDocument::from_raw("").unwrap();
    assert_eq!(null.qualification, MemoryFailureJsonQualification::Parsed);
    assert_eq!(empty.qualification, MemoryFailureJsonQualification::Unparsed);
    assert_ne!(
        serde_json::to_value(Some(null)).unwrap(),
        serde_json::to_value(None::<MemoryFailureJsonDocument>).unwrap()
    );
    assert_ne!(serde_json::to_value(Some(empty)).unwrap(), json!(null));
}

#[test]
fn native_document_bounds_use_utf16_units_and_never_truncate() {
    for raw in [format!("\"{}\"", "界".repeat(3998)), format!("\"{}\"", "😀".repeat(1999))] {
        assert_eq!(raw.encode_utf16().count(), MAX_MEMORY_FAILURE_JSON_UTF16);
        let document = MemoryFailureJsonDocument::from_raw(raw.clone()).unwrap();
        assert!(document.is_valid());
        assert_eq!(document.raw, raw);
        assert_eq!(document.qualification, MemoryFailureJsonQualification::Parsed);
    }
    for raw in ["a".repeat(4001), "😀".repeat(2001), "界".repeat(4001)] {
        assert!(MemoryFailureJsonDocument::from_raw(raw.clone()).is_err());
        assert!(!MemoryFailureJsonDocument { raw, qualification: MemoryFailureJsonQualification::Unparsed }.is_valid());
    }
}

#[test]
fn document_qualification_cannot_be_forged_or_skip_trailing_input() {
    for (raw, qualification) in [
        ("{broken", MemoryFailureJsonQualification::Parsed),
        ("{}", MemoryFailureJsonQualification::Unparsed),
        ("{} []", MemoryFailureJsonQualification::Parsed),
    ] {
        assert!(!MemoryFailureJsonDocument { raw: raw.into(), qualification }.is_valid());
    }
}

#[test]
fn source_local_datetime_is_never_promoted_to_a_utc_event_time() {
    for invalid in [
        "2026-11-13T10:00:00Z",
        "2026-11-13T10:00:00+02:00",
        "2026-11-13 10:00:00",
        "2026-11-13T10:00:00.1234",
        "1752-12-31T23:59:59.997",
        "10000-01-01T00:00:00",
        "2026-02-30T10:00:00",
        "2026-12-31T23:59:60",
    ] {
        let mut native = observation();
        native.event_time_without_offset = invalid.into();
        assert!(!native.is_valid(), "{invalid}");
    }
    for valid in ["1753-01-01T00:00:00", "9999-12-31T23:59:59.997", "2026-11-13T10:00:00.000"] {
        let mut native = observation();
        native.event_time_without_offset = valid.into();
        assert!(native.is_valid(), "{valid}");
        assert!(snapshot(vec![native]).is_valid(), "local times are not compared with UTC capture bounds");
    }
}

#[test]
fn native_codes_preserve_unknown_values_and_enforce_sql_widths() {
    let mut native = observation();
    native.oom_cause = u8::MAX;
    native.oom_factor = u8::MAX;
    native.oom_cause_desc = "😀".repeat(15);
    assert!(native.is_valid());
    native.oom_cause_desc.push('x');
    assert!(!native.is_valid());
    for (field, value) in [
        ("oom_cause", json!(-1)),
        ("oom_factor", json!(256)),
        ("process_memory_usage_mb", json!(i64::from(i32::MAX) + 1)),
        ("available_physical_memory_mb", json!(i64::from(i32::MIN) - 1)),
    ] {
        let mut row = serde_json::to_value(observation()).unwrap();
        row[field] = value;
        assert!(serde_json::from_value::<MssqlOutOfMemoryObservation>(row).is_err(), "{field}");
    }
}

#[test]
fn identical_retained_rows_remain_distinct_only_within_their_capture_positions() {
    let first = observation();
    let mut second = first.clone();
    second.position = 1;
    assert_ne!(first.key().unwrap(), second.key().unwrap());
    assert!(snapshot(vec![first.clone(), second]).is_valid());
    assert!(!snapshot(vec![first.clone(), first]).is_valid());
    let mut changed = observation();
    let key = changed.key().unwrap();
    changed.committed_memory_mb += 1;
    assert_eq!(key, changed.key().unwrap(), "a payload fingerprint is not an occurrence ID");
}

#[test]
fn operational_record_identity_keeps_capture_separate_from_local_native_time() {
    let native = observation();
    let record = TelemetryRecord::Operational(OperationalObservation {
        source: crate::source_id::MEMORY_FAILURES.into(),
        key: native.key().unwrap(),
        fact: OperationalFact::MssqlOutOfMemory(Box::new(native.clone())),
    });
    assert!(record.is_valid());
    let first = ExportedRecord::new(
        &source(1),
        &CaptureIdentity { observation_id: "first-capture".into(), revision: None },
        record.clone(),
    )
    .unwrap();
    let next = ExportedRecord::new(
        &source(1),
        &CaptureIdentity { observation_id: "next-capture".into(), revision: None },
        record,
    )
    .unwrap();
    assert_ne!(first.record_id, next.record_id, "the buffer snapshot is not an exactly-once event feed");
    let TelemetryRecord::Operational(row) = first.record else { panic!() };
    let OperationalFact::MssqlOutOfMemory(restored) = row.fact else { panic!() };
    assert_eq!(*restored, native, "export preserves source-native metadata and malformed bounded JSON");
}

#[test]
fn native_keys_and_declared_source_membership_authenticate_embedded_memory_failure_rows() {
    let native = observation();
    let mut row = OperationalObservation {
        source: crate::source_id::MEMORY_FAILURES.into(),
        key: native.key().unwrap(),
        fact: OperationalFact::MssqlOutOfMemory(Box::new(native)),
    };
    assert!(row.is_valid());
    row.key = observation().key().unwrap();
    let changed_key = {
        let OperationalFact::MssqlOutOfMemory(native) = &mut row.fact else { panic!() };
        native.position = 1;
        native.key().unwrap()
    };
    assert!(!row.is_valid());
    row.key = changed_key;
    assert!(row.is_valid());
    row.source = "embedding-native-memory-source".into();
    assert!(row.is_valid(), "source names are not native fact semantics");
    let capture = CaptureIdentity { observation_id: "embedded-memory-capture".into(), revision: None };
    let record = TelemetryRecord::Operational(row.clone());
    assert!(ExportedRecord::new(&source(1), &capture, record.clone()).is_err(), "the source was not declared");
    let mut declared = source(1);
    let SourceObservation::Snapshot(snapshot) = &mut declared.observation else { panic!() };
    snapshot.sources[0].source = row.source;
    let exported = ExportedRecord::new(&declared, &capture, record.clone()).unwrap();
    assert_eq!(exported.record, record, "the exact declared native source is preserved");
}

#[test]
fn hosting_scope_never_falls_back_to_the_selected_database() {
    let mut captured = snapshot(vec![observation()]);
    assert!(captured.is_valid(), "None resource explicitly retains unowned shared context");
    captured.scope = MeasurementScope::Database;
    assert!(!captured.is_valid());
    captured.scope = MeasurementScope::Pool;
    assert!(!captured.is_valid(), "pool scope requires a verified pool identity");
    captured.scope = MeasurementScope::Server;
    captured.resource = Some(resource(SharedResourceKind::Instance));
    assert!(captured.is_valid());
    captured.resource.as_mut().unwrap().kind = SharedResourceKind::Pool;
    assert!(!captured.is_valid());
    captured.scope = MeasurementScope::Pool;
    assert!(captured.is_valid());
    captured.resource.as_mut().unwrap().engine = Engine::Postgres;
    assert!(!captured.is_valid());
}

#[test]
fn sharing_hides_physical_pool_identity_without_changing_native_memory_failure_evidence() {
    let mut captured = snapshot(vec![observation()]);
    captured.scope = MeasurementScope::Pool;
    let mut pool = resource(SharedResourceKind::Pool);
    pool.namespace = NativeIdentity::new("private-namespace-canary").qualified("cluster", "private-cluster-canary");
    pool.native = NativeIdentity::new("private-pool-canary").qualified("native_group", "private-group-canary");
    captured.resource = Some(pool.clone());
    assert!(captured.is_valid());
    let mut original: crate::Snapshot =
        serde_json::from_str(include_str!("../../../../../fixtures/mssql-demo.json")).unwrap();
    original.operations.memory_failures = Some(captured.clone());

    let shared = crate::redact::redact_snapshot(&original);
    let serialized = serde_json::to_string(&shared).unwrap();
    assert!(!serialized.contains("-canary"));
    let evidence = shared.operations.memory_failures.as_ref().unwrap();
    assert!(evidence.is_valid());
    assert_eq!(evidence.scope, MeasurementScope::Pool);
    assert_eq!(evidence.source, captured.source);
    assert_eq!(evidence.observed_from, captured.observed_from);
    assert_eq!(evidence.observed_until, captured.observed_until);
    assert_eq!(evidence.observations, captured.observations);
    let identity = evidence.resource.as_ref().unwrap();
    assert_eq!(identity.engine, pool.engine);
    assert_eq!(identity.kind, SharedResourceKind::Pool);
    for (private, public) in [(&pool.namespace, &identity.namespace), (&pool.native, &identity.native)] {
        assert_ne!(private.id, public.id);
        assert!(public.id.starts_with("redacted-"));
        assert_eq!(private.qualifiers.keys().collect::<Vec<_>>(), public.qualifiers.keys().collect::<Vec<_>>());
        for (name, value) in &public.qualifiers {
            assert_ne!(value, &private.qualifiers[name]);
            assert!(value.starts_with("redacted-"));
        }
    }
    assert_eq!(original.operations.memory_failures.as_ref().unwrap(), &captured);
    assert_eq!(
        serde_json::to_value(&shared).unwrap(),
        serde_json::to_value(crate::redact::redact_snapshot(&shared)).unwrap()
    );
}

#[test]
fn empty_buffer_retains_capture_metadata_without_a_history_completeness_claim() {
    let captured = snapshot(Vec::new());
    assert!(captured.is_valid());
    let value = serde_json::to_value(&captured).unwrap();
    assert_eq!(value["observations"], json!([]));
    assert!(value.get("history_complete").is_none());
    assert!(value.get("query_id").is_none() && value.get("database_id").is_none());
    let mut invalid = captured;
    invalid.observed_from = "2026-11-14T00:00:00Z".into();
    assert!(!invalid.is_valid());
    invalid.observed_from = "not-a-clock".into();
    assert!(!invalid.is_valid());
}

#[test]
fn observation_and_snapshot_counts_have_an_explicit_bound() {
    let mut native = observation();
    native.position = MAX_MEMORY_FAILURE_OBSERVATIONS as u32;
    assert!(!native.is_valid() && native.key().is_err());
    let mut captured = snapshot(Vec::new());
    captured.observations = (0..MAX_MEMORY_FAILURE_OBSERVATIONS)
        .map(|position| {
            let mut native = observation();
            native.position = position as u32;
            native
        })
        .collect();
    assert!(captured.is_valid());
    captured.observations.push(observation());
    assert!(!captured.is_valid());
}

#[test]
fn native_buffers_are_omitted_from_the_counters_only_projection() {
    let operations = crate::operations::OperationalStats {
        memory_failures: Some(snapshot(vec![observation()])),
        ..Default::default()
    };
    assert!(serde_json::to_value(&operations).unwrap().get("memory_failures").is_some());
    assert!(serde_json::to_value(operations.counters_only()).unwrap().get("memory_failures").is_none());
    assert!(
        serde_json::to_value(crate::operations::OperationalStats::default()).unwrap().get("memory_failures").is_none()
    );
    assert_eq!(SnapshotDataset::MemoryFailures.streams(), &[TelemetryStream::OperationalSamples]);
}
