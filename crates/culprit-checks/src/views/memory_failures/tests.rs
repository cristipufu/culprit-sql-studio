use super::*;
use culprit_core::MeasurementScope;
use culprit_core::telemetry::MemoryFailureJsonDocument;

fn observation(position: u32, cause: u8) -> MssqlOutOfMemoryObservation {
    MssqlOutOfMemoryObservation {
        position,
        event_time_without_offset: "2026-11-13T11:30:00.123".into(),
        oom_cause: cause,
        oom_cause_desc: "native-classification".into(),
        oom_factor: 11,
        oom_factor_desc: "native-internal-factor".into(),
        available_physical_memory_mb: -1,
        process_memory_usage_mb: i32::MAX,
        non_sos_memory_usage_mb: 64,
        committed_memory_target_mb: 1024,
        committed_memory_mb: 1000,
        allocation_potential_memory_mb: -1,
        initial_job_object_memory_limit_mb: None,
        current_job_object_memory_limit_mb: Some(0),
        possible_non_sos_leaked_memory_mb: None,
        oom_resource_pools: Some(MemoryFailureJsonDocument::from_raw("null").expect("native JSON null")),
        top_memory_clerks: Some(
            MemoryFailureJsonDocument::from_raw("{\"unrecognized-key\":\"raw-owned-once\"}").expect("opaque JSON"),
        ),
        top_resource_pools: None,
        possible_leaked_memory_clerks: None,
    }
}

fn evidence(observations: Vec<MssqlOutOfMemoryObservation>) -> MemoryFailureSnapshot {
    MemoryFailureSnapshot {
        source: "native-memory-failures".into(),
        observed_from: "2026-11-13T08:00:00Z".into(),
        observed_until: "2026-11-13T08:00:01Z".into(),
        scope: MeasurementScope::Server,
        resource: None,
        observations,
    }
}

fn source(evidence: &MemoryFailureSnapshot, state: SourceState) -> SourceOutcome {
    SourceOutcome { source: evidence.source.clone(), state, reason: None, reason_message: None }
}

#[test]
fn every_native_cause_selects_its_own_bounded_investigation_from_the_code_only() {
    let expected = [
        NativeMemoryCause::Unknown,
        NativeMemoryCause::InMemoryOltp,
        NativeMemoryCause::EngineMemory,
        NativeMemoryCause::OperatingSystem,
        NativeMemoryCause::OtherInstances,
        NativeMemoryCause::NonSosLeak,
        NativeMemoryCause::ServerlessReclamation,
        NativeMemoryCause::SosLeak,
        NativeMemoryCause::BufferPoolShrink,
        NativeMemoryCause::InternalPool,
        NativeMemoryCause::SystemPool,
        NativeMemoryCause::QueryGrants,
        NativeMemoryCause::ReplicaWorkload,
    ];
    let evidence = evidence((0..=12).map(|code| observation(u32::from(code), code)).collect());
    let report = explain(&evidence, Some(&source(&evidence, SourceState::Collected))).expect("native observations");
    assert_eq!(report.entries.len(), expected.len());
    for (code, (entry, expected)) in report.entries.iter().zip(expected).enumerate() {
        assert_eq!(NativeMemoryCause::of(code as u8), expected);
        assert_eq!(entry.position, code as u32);
        assert_eq!(entry.title, culprit_core::i18n::t(expected.messages().0, &[]));
        assert!(!entry.explanation.is_empty());
        assert!(!entry.investigation.is_empty());
        assert!(!entry.verify.is_empty());
    }
    assert_eq!(report.source_state, Some(SourceState::Collected));
    assert!(report.limitations.iter().any(|text| text.contains("heuristic")));
    let serialized = serde_json::to_string(&report).expect("derived report");
    assert!(!serialized.contains("raw-owned-once"), "the report references raw rows instead of cloning their JSON");
    assert!(!serialized.contains("current database opportunity"));
}

#[test]
fn unknown_and_future_codes_cannot_inherit_a_cause_from_native_text_values_or_internal_factor() {
    let mut unknown = observation(0, 0);
    unknown.oom_cause_desc = "QUERY_MEMORY_GRANTS".into();
    unknown.oom_factor = 10;
    unknown.oom_factor_desc = "QUERY_MEM_QUEUE".into();
    let unknown_evidence = evidence(vec![unknown]);
    let report = explain(&unknown_evidence, None).expect("unknown is valid evidence");
    assert_eq!(report.entries[0].title, t!("view.memory_failures.cause.unknown.title"));
    let future = evidence(vec![observation(0, 255)]);
    let report = explain(&future, None).expect("unrecognized native code is retained");
    assert_eq!(report.entries[0].title, t!("view.memory_failures.cause.unrecognized.title"));
    assert_eq!(NativeMemoryCause::of(255), NativeMemoryCause::Unrecognized);
}

#[test]
fn duplicate_native_rows_preserve_positions_without_incident_count_or_utc_window_claims() {
    let mut first = observation(8, 11);
    // A native clock token can appear later than capture UTC: no offset was established.
    first.event_time_without_offset = "2099-01-01T00:00:00.003".into();
    let mut second = first.clone();
    second.position = 2;
    let evidence = evidence(vec![first, second]);
    let report = explain(&evidence, Some(&source(&evidence, SourceState::Collected))).expect("distinct frozen rows");
    assert_eq!(report.entries.iter().map(|entry| entry.position).collect::<Vec<_>>(), vec![8, 2]);
    assert!(report.summary.contains("2 retained observations"));
    assert!(report.limitations.iter().any(|text| text.contains("UTC offset")));
    assert!(report.limitations.iter().any(|text| text.contains("query owner")));
    assert_eq!(evidence.observations[0].available_physical_memory_mb, -1);
    assert_eq!(evidence.observations[0].process_memory_usage_mb, i32::MAX);
}

#[test]
fn empty_partial_and_unavailable_reads_never_establish_failure_absence_or_current_pressure() {
    let empty = evidence(vec![]);
    for state in [SourceState::Empty, SourceState::Partial, SourceState::Unavailable, SourceState::NotApplicable] {
        let mut source = source(&empty, state);
        source.reason = Some("native source qualification".into());
        let report = explain(&empty, Some(&source)).expect("empty source qualification");
        assert!(report.entries.is_empty());
        assert_eq!(report.source_state, Some(state));
        assert_eq!(report.source_reason.as_deref(), source.reason.as_deref());
        assert!(report.summary.contains("does not establish"));
    }
    let retained = evidence(vec![observation(0, 11)]);
    let report = explain(&retained, Some(&source(&retained, SourceState::Partial))).expect("partial positive evidence");
    assert_eq!(report.entries.len(), 1);
    assert!(report.limitations.iter().any(|text| text.contains("partly read")));
    let report = explain(&retained, None).expect("unknown qualification does not erase a positive observation");
    assert_eq!(report.source_state, None);
    assert!(report.limitations.iter().any(|text| text.contains("not established")));
}

#[test]
fn malformed_scope_generation_clock_or_source_cannot_be_rendered_as_qualified_evidence() {
    let mut invalid = evidence(vec![observation(0, 11)]);
    for state in [SourceState::Empty, SourceState::Unavailable, SourceState::NotApplicable] {
        assert!(explain(&invalid, Some(&source(&invalid, state))).is_err());
    }
    let mismatched = SourceOutcome { source: "another-source".into(), ..source(&invalid, SourceState::Collected) };
    assert!(explain(&invalid, Some(&mismatched)).is_err());
    invalid.observations.push(invalid.observations[0].clone());
    assert!(explain(&invalid, None).is_err(), "positions are capture-local keys and must be unique");
    invalid.observations.truncate(1);
    invalid.scope = MeasurementScope::Database;
    assert!(explain(&invalid, None).is_err(), "hosting-engine OOM cannot be assigned to the current database");
    invalid.scope = MeasurementScope::Server;
    invalid.observed_until = "2020-01-01T00:00:00Z".into();
    assert!(explain(&invalid, None).is_err());
}

#[test]
fn public_pipeline_preserves_the_captured_custom_sources_partial_qualification() {
    let captured = evidence(vec![observation(0, 11)]);
    let mut snapshot = crate::waits::snapshot_with(culprit_core::Engine::Mssql);
    snapshot.operations.memory_failures = Some(captured.clone());
    let mut custom = source(&captured, SourceState::Partial);
    custom.reason = Some("bounded native read was partial".into());
    snapshot.sources = vec![custom.clone()];
    let report = crate::analyze(snapshot);
    let memory = report.memory_failures.expect("same pure view in the ordinary pipeline");
    assert_eq!(memory.source_state, Some(SourceState::Partial));
    assert_eq!(memory.source_reason, custom.reason);
    assert_eq!(memory.entries[0].position, 0);
    assert_eq!(report.snapshot.operations.memory_failures, Some(captured));
}

#[test]
fn malformed_native_capture_withholds_only_its_derived_view_and_retains_raw_evidence_with_a_localized_note() {
    let mut captured = evidence(vec![observation(0, 11)]);
    captured.observed_until = "2020-01-01T00:00:00Z".into();
    let mut snapshot = crate::waits::snapshot_with(culprit_core::Engine::Mssql);
    snapshot.operations.memory_failures = Some(captured.clone());
    let canonical = SourceOutcome {
        source: culprit_core::source_id::MEMORY_FAILURES.into(),
        state: SourceState::Collected,
        reason: None,
        reason_message: None,
    };
    snapshot.sources = vec![source(&captured, SourceState::Collected), canonical.clone()];
    let report = crate::analyze(snapshot);
    assert!(report.memory_failures.is_none());
    assert_eq!(report.snapshot.operations.memory_failures, Some(captured.clone()));
    let custom = report.snapshot.sources.iter().find(|source| source.source == captured.source).expect("actual source");
    assert_eq!(custom.state, SourceState::Unavailable);
    assert_eq!(custom.reason.as_deref(), Some(t!("view.memory_failures.invalid_capture").as_str()));
    assert!(custom.reason_message.is_some());
    assert!(report.snapshot.sources.contains(&canonical), "another source must not be overwritten");
    assert!(report.snapshot.notes.iter().any(|note| {
        note.collector == captured.source
            && note.message == t!("view.memory_failures.invalid_capture")
            && matches!(&note.message_source, Some(culprit_core::i18n::Message::Catalog { key, .. }) if key == "view.memory_failures.invalid_capture")
    }));
}
