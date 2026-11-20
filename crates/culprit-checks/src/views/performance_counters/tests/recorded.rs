use super::*;

fn original() -> CapturedOperationalRead {
    let (description, row) = database_evidence();
    let identity = CaptureIdentity {
        observation_id: "a".repeat(32),
        revision: Some(CaptureRevision { outbox_id: "b".repeat(32), owner_epoch: "3".into(), sequence: "4".into() }),
    };
    let capture = DiagnosticCaptureRef {
        capture: identity.clone(),
        manifest_sha256: digest(b"original-immutable-manifest"),
        source_scope_id: description.captured_range().unwrap().scope_id,
    };
    let exported = ExportedRecord::new(&description, &identity, TelemetryRecord::Operational(row.clone())).unwrap();
    let positions = vec![CapturedSnapshotPosition::from_export(&exported, 0).unwrap()];
    CapturedOperationalRead::captured(description, capture, positions, vec![row], true).unwrap()
}
fn snapshot(recorded: RecordedPerformanceCounters) -> Snapshot {
    let mut snapshot: Snapshot =
        serde_json::from_str(include_str!("../../../../../../fixtures/mssql-demo.json")).unwrap();
    snapshot.sources = vec![outcome(SourceState::Collected)];
    snapshot.pressure.get_or_insert_with(Default::default).resource_counters = Some(vec![sample()]);
    snapshot.operations.database_performance_counters = Some(recorded);
    snapshot
}
fn captured(read: CapturedOperationalRead) -> RecordedPerformanceCounters {
    RecordedPerformanceCounters::Captured { read: Box::new(read) }
}

#[test]
fn saved_counter_populations_keep_distinct_original_counts_scope_and_clocks() {
    let original = original();
    let expected = explain_database_captured(&original).unwrap();
    let mut scan = snapshot(captured(original.clone()));
    scan.server.database = "a-new-current-database".into();
    scan.identity = None;
    let report = crate::analyze(scan);
    assert_eq!(report.performance_counters.len(), 2);
    let shared = &report.performance_counters[0];
    let selected = &report.performance_counters[1];
    assert_eq!(shared.population, PerformanceCounterPopulation::SharedServer);
    assert_eq!(selected.population, PerformanceCounterPopulation::SelectedDatabase);
    assert_eq!(shared.observed_counter_rows, 2);
    assert_eq!(selected.observed_counter_rows, 1);
    assert_eq!(selected.database.as_deref(), Some("app"));
    assert_eq!(selected.context.as_ref().unwrap().native(), Some(&original.context().unwrap()));
    assert_eq!(serde_json::to_value(selected).unwrap(), serde_json::to_value(expected).unwrap());
    assert!(selected.limitations.contains(&t!("view.performance_counters.limit.historical")));
    assert!(selected.entries.iter().all(|entry| entry.scope == MeasurementScope::Database));
    assert!(shared.entries.iter().all(|entry| entry.scope == MeasurementScope::Server));
}

#[test]
fn edited_original_scope_member_epoch_clock_body_and_receipt_withdraw_only_the_selected_population() {
    for cause in ["scope", "member", "epoch", "clock", "body", "receipt", "dataset"] {
        let mut read = original();
        match cause {
            "scope" => {
                let CollectionTarget::Database(database) = &mut read.description.target else { panic!() };
                database.database = "foreign".into();
            }
            "member" => {
                let SourcePartition::InstanceLocal { member: Some(member), .. } = &mut read.description.partition
                else {
                    panic!()
                };
                member.native.id = "foreign".into();
            }
            "epoch" => read.description.partition = SourcePartition::Target,
            "clock" => {
                let SourceObservation::DatabasePerformanceCounters { snapshot, .. } = &mut read.description.observation
                else {
                    panic!()
                };
                snapshot.observed_until = "2030-01-01T00:00:00Z".into();
            }
            "body" => {
                let OperationalFact::CounterSet(set) = &mut read.observations[0].fact else { panic!() };
                set.counters[0].value = MeasurementValue::Integer("1".into());
            }
            "receipt" => read.origin.positions[0].payload_sha256 = digest(b"foreign-body"),
            _ => {
                let SourceObservation::DatabasePerformanceCounters { snapshot, .. } = &mut read.description.observation
                else {
                    panic!()
                };
                snapshot.dataset = SnapshotDataset::Operational;
            }
        }
        assert!(explain_database_captured(&read).is_err(), "{cause}");
        let scan = snapshot(captured(read));
        let original_source = scan.sources[0].clone();
        let report = crate::analyze(scan);
        assert_eq!(report.performance_counters.len(), 2, "{cause}");
        assert_eq!(
            report.snapshot.sources.iter().find(|source| source.source == original_source.source),
            Some(&original_source)
        );
        assert_eq!(report.performance_counters[0].source_state, Some(SourceState::Collected));
        let unavailable = &report.performance_counters[1];
        assert_eq!(unavailable.population, PerformanceCounterPopulation::SelectedDatabase);
        assert_eq!(unavailable.source_state, Some(SourceState::Unavailable));
        assert_eq!(unavailable.native_observations, None);
        assert!(unavailable.database.is_none() && unavailable.context.is_none() && unavailable.entries.is_empty());
        assert!(matches!(
            report.snapshot.operations.database_performance_counters,
            Some(RecordedPerformanceCounters::Unavailable { .. })
        ));
    }
}

#[test]
fn missing_original_body_preserves_native_counts_without_zero_or_a_counter_lifetime() {
    let mut read = original();
    read.observations.clear();
    read.origin.positions.clear();
    read.read_complete = false;
    assert!(read.is_valid());
    let report = explain_database_captured(&read).unwrap();
    assert_eq!(report.population, PerformanceCounterPopulation::SelectedDatabase);
    assert_eq!(report.source_state, Some(SourceState::Partial));
    assert_eq!(report.native_observations, Some(1));
    assert_eq!(report.native_counter_rows, None);
    assert_eq!(report.read_complete, Some(false));
    assert!(report.entries.is_empty() && report.ratios.is_none());
    assert_eq!(report.database.as_deref(), Some("app"));
}

#[test]
fn sharing_withdraws_the_indivisible_original_with_a_typed_selected_gap_and_no_shared_source_transfer() {
    let scan = snapshot(captured(original()));
    let shared = culprit_core::redact::redact_snapshot(&scan);
    let marker = shared.operations.database_performance_counters.as_ref().unwrap();
    assert_eq!(
        marker,
        &RecordedPerformanceCounters::Unavailable {
            reason: culprit_core::message!("view.performance_counters.sharing_withheld"),
        }
    );
    assert_eq!(
        shared
            .sources
            .iter()
            .find(|source| source.source == culprit_core::source_id::PERFORMANCE_COUNTERS)
            .unwrap()
            .state,
        SourceState::Collected
    );
    let restored: Snapshot = serde_json::from_value(serde_json::to_value(&shared).unwrap()).unwrap();
    let report = crate::analyze(restored);
    assert_eq!(report.performance_counters.len(), 2);
    let selected = &report.performance_counters[1];
    assert_eq!(selected.population, PerformanceCounterPopulation::SelectedDatabase);
    assert_eq!(selected.source_state, Some(SourceState::Unavailable));
    assert!(selected.context.is_none() && selected.database.is_none() && selected.native_database_id.is_none());
    assert!(selected.entries.is_empty());
    let again = culprit_core::redact::redact_snapshot(&shared);
    assert_eq!(again.operations.database_performance_counters, shared.operations.database_performance_counters);
    let text = serde_json::to_string(&shared).unwrap();
    for private in
        ["original-immutable-manifest", "database_guid", "recovery_fork_guid", "database_file_guid", "instance_epoch"]
    {
        assert!(!text.contains(private), "{private}");
    }
}

#[test]
fn unavailable_marker_roundtrips_without_acquiring_original_identity_or_shared_scope() {
    let recorded = RecordedPerformanceCounters::Unavailable {
        reason: culprit_core::message!("view.performance_counters.sharing_withheld"),
    };
    let decoded = serde_json::from_slice(&serde_json::to_vec(&recorded).unwrap()).unwrap();
    let report = explain_recorded(&decoded).unwrap();
    assert_eq!(report.population, PerformanceCounterPopulation::SelectedDatabase);
    assert_eq!(report.source_state, Some(SourceState::Unavailable));
    assert_eq!(report.native_counter_rows, None);
    assert!(report.database.is_none() && report.native_database_id.is_none() && report.context.is_none());
}

#[test]
fn malformed_shared_counter_rows_cannot_withdraw_or_relabel_an_authenticated_selected_population() {
    let mut scan = snapshot(captured(original()));
    scan.pressure.as_mut().unwrap().resource_counters.as_mut().unwrap()[0].scope = MeasurementScope::Database;
    let report = crate::analyze(scan);
    assert_eq!(report.performance_counters.len(), 2);
    assert_eq!(report.performance_counters[0].population, PerformanceCounterPopulation::SharedServer);
    assert_eq!(report.performance_counters[0].source_state, Some(SourceState::Unavailable));
    assert!(report.performance_counters[0].entries.is_empty());
    assert_eq!(report.performance_counters[1].population, PerformanceCounterPopulation::SelectedDatabase);
    assert_eq!(report.performance_counters[1].source_state, Some(SourceState::Partial));
    assert_eq!(report.performance_counters[1].entries.len(), 1);
    assert!(matches!(
        report.snapshot.operations.database_performance_counters,
        Some(RecordedPerformanceCounters::Captured { .. })
    ));
}
