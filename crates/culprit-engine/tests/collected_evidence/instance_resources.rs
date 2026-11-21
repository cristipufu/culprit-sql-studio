use super::fixture::Fixture;
use culprit_collector::analytical::{
    evidence::{EvidenceFamily, EvidenceGapReason},
    test_support::{self, InstanceResourceCase},
};
use culprit_core::{SourceState, telemetry::*};

#[tokio::test]
async fn original_processor_cpu_source_replays_separately_from_the_instance_resource_point_without_requery() {
    use test_support::{ProcessCpuCase, instance_resource_cpu_evidence};
    for case in ProcessCpuCase::ALL {
        let (source, rows) = instance_resource_cpu_evidence(case);
        let fixture = Fixture::new(source.clone(), rows.clone()).await;
        let opened = fixture.recovery_object_reads();
        let capture = fixture.read_families(&[EvidenceFamily::OperationalSamples]).await;
        test_support::assert_operational_capture(&capture, &source, &rows);
        let report = culprit_engine::collected_resources::instance_resources(&capture).unwrap().report;
        let cpu = report.process_cpu.as_ref().unwrap();
        assert_eq!(report.native_source.as_ref().unwrap().source, INSTANCE_RESOURCES_SOURCE);
        assert_eq!(cpu.native_source.as_ref().unwrap().source, culprit_core::source_id::PROCESSOR_TOPOLOGY);
        if case == ProcessCpuCase::Denied {
            assert_eq!(cpu.source_state, SourceState::Unavailable);
            assert!(cpu.fields.is_empty() && cpu.observed_at.is_none());
        } else {
            assert_eq!(cpu.observed_at.as_deref(), Some("2020-01-01T12:00:00.5Z"));
            assert_ne!(cpu.observed_at, report.observed_at);
            assert_eq!(
                cpu.source_state,
                if case == ProcessCpuCase::Partial { SourceState::Partial } else { SourceState::Collected }
            );
            let expected = match case {
                ProcessCpuCase::Exact | ProcessCpuCase::Partial => Some("9007199254740993"),
                ProcessCpuCase::Zero => Some("0"),
                ProcessCpuCase::Null => None,
                _ => unreachable!(),
            };
            assert_eq!(cpu.fields.iter().find(|field| field.key == "user_time_ms").unwrap().value.as_deref(), expected);
        }
        assert_eq!(fixture.recovery_object_reads(), opened);
    }
}

#[tokio::test]
async fn missing_cpu_body_preserves_declared_original_source_and_never_borrows_the_memory_point_clock() {
    let (source, rows) = test_support::instance_resource_cpu_evidence(test_support::ProcessCpuCase::Exact);
    let fixture = Fixture::new(source, rows).await;
    fixture.remove_row(TelemetryStream::OperationalSamples, 4);
    let capture = fixture.read_families(&[EvidenceFamily::OperationalSamples]).await;
    assert!(!capture.is_read_complete());
    let report = culprit_engine::collected_resources::instance_resources(&capture).unwrap().report;
    let cpu = report.process_cpu.as_ref().unwrap();
    assert_eq!(cpu.source_state, SourceState::Partial);
    assert_eq!(cpu.native_source.as_ref().unwrap().records, 4);
    assert_eq!(cpu.native_source.as_ref().unwrap().state, SourceState::Collected);
    assert!(cpu.observed_at.is_none() && cpu.fields.is_empty());
    assert!(report.observed_at.is_some());
}

#[tokio::test]
async fn instance_points_replay_exact_original_scope_populations_flags_and_pure_meanings_without_database_work() {
    for case in InstanceResourceCase::ALL {
        let (source, rows) = test_support::instance_resource_evidence(case);
        let fixture = Fixture::new(source.clone(), rows.clone()).await;
        let opened = fixture.recovery_object_reads();
        let capture = fixture.read_families(&[EvidenceFamily::OperationalSamples]).await;
        test_support::assert_operational_capture(&capture, &source, &rows);
        let analysis = culprit_engine::collected_resources::instance_resources(&capture).unwrap();
        let original = rows
            .into_iter()
            .map(|row| {
                let TelemetryRecord::Operational(row) = row else { panic!() };
                row
            })
            .collect::<Vec<_>>();
        let read = InstanceResourceRead::from_source(&source, &original, true).unwrap();
        let expected = culprit_checks::instance_resources::explain(&read).unwrap();
        assert_eq!(analysis.context.source, source);
        assert_eq!(*analysis.report, expected);
        let context = analysis.report.context.as_ref().unwrap().native().unwrap();
        assert!(matches!(context.target, CollectionTarget::Shared(_)));
        assert_eq!(context.partition, source.partition);
        assert!(context.target.database().is_none());
        assert_eq!(fixture.recovery_object_reads(), opened);
    }
}

#[tokio::test]
async fn absent_instance_body_preserves_original_native_row_count_and_never_hydrates_or_fabricates_zero() {
    let (source, rows) = test_support::instance_resource_evidence(InstanceResourceCase::Exact);
    let fixture = Fixture::new(source.clone(), rows).await;
    fixture.remove_first_row(TelemetryStream::OperationalSamples);
    let opened = fixture.recovery_object_reads();
    let capture = fixture.read_families(&[EvidenceFamily::OperationalSamples]).await;
    assert!(!capture.is_read_complete());
    assert!(capture.gaps().iter().any(|gap| gap.reason == EvidenceGapReason::MissingRows));
    let analysis = culprit_engine::collected_resources::instance_resources(&capture).unwrap();
    assert_eq!(analysis.context.source, source);
    assert_eq!(analysis.report.source_state, SourceState::Partial);
    assert_eq!(analysis.report.native_source.as_ref().unwrap().records, 1);
    assert_eq!(analysis.report.native_source.as_ref().unwrap().state, SourceState::Collected);
    assert_eq!(analysis.report.read_complete, Some(false));
    assert!(analysis.report.groups.is_empty() && analysis.report.observed_at.is_none());
    assert_eq!(analysis.report.native_populations_complete, None);
    assert_eq!(fixture.recovery_object_reads(), opened);
}

#[tokio::test]
async fn original_per_scheduler_rows_replay_all_states_exact_fields_and_independent_native_source_without_requery() {
    use test_support::{SchedulerCase, instance_resource_scheduler_evidence};
    for case in SchedulerCase::ALL {
        let (source, rows) = instance_resource_scheduler_evidence(case);
        let fixture = Fixture::new(source.clone(), rows.clone()).await;
        let opened = fixture.recovery_object_reads();
        let capture = fixture.read_families(&[EvidenceFamily::OperationalSamples]).await;
        test_support::assert_operational_capture(&capture, &source, &rows);
        let report = culprit_engine::collected_resources::instance_resources(&capture).unwrap().report;
        let schedulers = report.schedulers.as_ref().unwrap();
        assert_eq!(report.native_source.as_ref().unwrap().source, INSTANCE_RESOURCES_SOURCE);
        assert_eq!(schedulers.native_source.as_ref().unwrap().source, culprit_core::source_id::SCHEDULERS);
        assert_eq!(schedulers.context.as_ref().unwrap().native().unwrap().partition, source.partition);
        assert_eq!(schedulers.read_complete, Some(true));
        assert_eq!(schedulers.observed_rows, schedulers.native_rows.unwrap());
        if case == SchedulerCase::Denied {
            assert_eq!(schedulers.source_state, SourceState::Unavailable);
            assert!(schedulers.entries.is_empty());
        } else {
            assert_eq!(schedulers.entries[0].scheduler_id, "0");
            assert!(
                schedulers.entries[0]
                    .fields
                    .iter()
                    .any(|field| field.native_name == "status" && field.value.as_deref() == Some("VISIBLE ONLINE"))
            );
            let queue =
                schedulers.entries[0].fields.iter().find(|field| field.native_name == "work_queue_count").unwrap();
            assert_eq!(
                queue.value.as_deref(),
                Some(if case == SchedulerCase::MeasuredZero { "0" } else { "9007199254740993" })
            );
            assert_eq!(queue.nature, Some(CounterNature::Gauge));
            assert_eq!(queue.reset, Some(culprit_core::ResetMarker::Unknown));
            let expected_state = if matches!(case, SchedulerCase::NativePartial | SchedulerCase::UnqualifiedOwner) {
                SourceState::Partial
            } else {
                SourceState::Collected
            };
            assert_eq!(schedulers.source_state, expected_state);
        }
        if case == SchedulerCase::NativeNull {
            assert!(
                schedulers.entries[0]
                    .fields
                    .iter()
                    .any(|field| field.availability == culprit_core::SchedulerFieldAvailability::NativeNull)
            );
        }
        if case == SchedulerCase::ColumnUnavailable {
            assert!(
                schedulers.entries[0]
                    .fields
                    .iter()
                    .any(|field| field.availability == culprit_core::SchedulerFieldAvailability::ColumnUnavailable)
            );
        }
        if case == SchedulerCase::UnqualifiedOwner {
            assert!(!schedulers.context.as_ref().unwrap().native().unwrap().has_qualified_local_owner());
            assert!(schedulers.limitations.contains(&culprit_core::t!("view.schedulers.limit.owner")));
        }
        assert_eq!(schedulers.entries_limited, case == SchedulerCase::Limited);
        assert!(schedulers.entries.len() <= 256);
        assert_eq!(fixture.recovery_object_reads(), opened);
    }
}

#[tokio::test]
async fn missing_scheduler_bodies_preserve_the_original_count_without_borrowing_aggregate_population_or_clock() {
    let (source, rows) = test_support::instance_resource_scheduler_evidence(test_support::SchedulerCase::Exact);
    let fixture = Fixture::new(source.clone(), rows).await;
    fixture.remove_row(TelemetryStream::OperationalSamples, 1);
    let opened = fixture.recovery_object_reads();
    let capture = fixture.read_families(&[EvidenceFamily::OperationalSamples]).await;
    assert!(!capture.is_read_complete());
    let report = culprit_engine::collected_resources::instance_resources(&capture).unwrap().report;
    let schedulers = report.schedulers.as_ref().unwrap();
    assert_eq!(schedulers.source_state, SourceState::Partial);
    assert_eq!(schedulers.native_source.as_ref().unwrap().records, 2);
    assert_eq!(schedulers.native_source.as_ref().unwrap().state, SourceState::Collected);
    assert_eq!(schedulers.observed_rows, 1);
    assert_eq!(schedulers.entries[0].scheduler_id, "1");
    assert!(report.observed_at.is_some());
    assert_eq!(fixture.recovery_object_reads(), opened);
}

#[tokio::test]
async fn successful_scheduler_originals_survive_an_independently_unavailable_instance_resource_family() {
    let (mut source, mut rows) = test_support::instance_resource_scheduler_evidence(test_support::SchedulerCase::Exact);
    rows.remove(0);
    let SourceObservation::Snapshot(header) = &mut source.observation else { panic!() };
    header.records -= 1;
    header.sources[0] = StateSourceCoverage::unavailable(
        INSTANCE_RESOURCES_SOURCE.into(),
        culprit_core::ErrorCode::PermissionDenied,
        None,
    );
    assert!(source.partition_is_consistent());
    let fixture = Fixture::new(source.clone(), rows.clone()).await;
    let capture = fixture.read_families(&[EvidenceFamily::OperationalSamples]).await;
    test_support::assert_operational_capture(&capture, &source, &rows);
    let report = culprit_engine::collected_resources::instance_resources(&capture).unwrap().report;
    assert_eq!(report.source_state, SourceState::Unavailable);
    assert!(report.observed_at.is_none() && report.groups.is_empty());
    let schedulers = report.schedulers.as_deref().unwrap();
    assert_eq!(schedulers.source_state, SourceState::Collected);
    assert_eq!(schedulers.native_rows, Some(2));
    assert_eq!(schedulers.observed_rows, 2);
    assert_eq!(schedulers.entries[0].scheduler_id, "0");
}

#[tokio::test]
async fn modern_scheduler_originals_keep_layout_absence_nulls_and_unqualified_spinlock_zero_or_positive_separate() {
    use test_support::{SchedulerCase, instance_resource_scheduler_evidence};
    for case in [
        SchedulerCase::Exact,
        SchedulerCase::Sql2019Layout,
        SchedulerCase::Sql2022Layout,
        SchedulerCase::ModernNulls,
        SchedulerCase::SpinlockRawPositive,
    ] {
        let (source, rows) = instance_resource_scheduler_evidence(case);
        let fixture = Fixture::new(source.clone(), rows.clone()).await;
        let opened = fixture.recovery_object_reads();
        let capture = fixture.read_families(&[EvidenceFamily::OperationalSamples]).await;
        test_support::assert_operational_capture(&capture, &source, &rows);
        let analysis = culprit_engine::collected_resources::instance_resources(&capture).unwrap();
        let report = analysis.report.schedulers.as_ref().unwrap();
        let fields = &report.entries[0].fields;
        assert_eq!(fields.len(), 29);
        for spec in MSSQL_SCHEDULER_FIELDS.iter().filter(|field| field.introduced_in.is_some()) {
            let field = fields.iter().find(|field| field.native_name == spec.name).unwrap();
            if case == SchedulerCase::ModernNulls {
                assert_eq!(field.availability, culprit_core::SchedulerFieldAvailability::NativeNull);
            } else if case == SchedulerCase::Sql2019Layout
                || case == SchedulerCase::Sql2022Layout && spec.introduced_in == Some(17)
            {
                assert_eq!(field.availability, culprit_core::SchedulerFieldAvailability::ColumnUnavailable);
            } else if matches!(spec.kind, MssqlNativeFieldKind::UnqualifiedCounter { .. }) {
                assert_eq!(field.availability, culprit_core::SchedulerFieldAvailability::InstrumentationUnqualified);
                assert!(field.value.is_none());
                let raw = field.unqualified.as_ref().unwrap();
                assert_eq!(raw.native_value, if case == SchedulerCase::SpinlockRawPositive { "7" } else { "0" });
                assert_eq!(raw.required_global_trace_flag, 8134);
            } else {
                assert_eq!(field.availability, culprit_core::SchedulerFieldAvailability::Observed);
                assert!(field.value.is_some() && field.unqualified.is_none());
                assert_eq!(field.reset, Some(culprit_core::ResetMarker::Unknown));
            }
        }
        assert_eq!(fixture.recovery_object_reads(), opened);
    }
}

#[tokio::test]
async fn original_numa_populations_keep_exact_fields_coverage_clocks_and_nulls_without_source_or_recovery() {
    use culprit_collector::analytical::test_support::{NumaNodeCase, numa_node_evidence};
    for case in NumaNodeCase::ALL {
        let (source, rows) = numa_node_evidence(case);
        let fixture = Fixture::new(source.clone(), rows.clone()).await;
        let opened = fixture.recovery_object_reads();
        let capture =
            fixture.read_families(&[culprit_collector::analytical::evidence::EvidenceFamily::OperationalSamples]).await;
        let analysis = culprit_engine::collected_resources::instance_resources(&capture).unwrap();
        assert_eq!(analysis.context.source, source);
        assert_eq!(analysis.context.records.len(), rows.len());
        assert_eq!(fixture.recovery_object_reads(), opened);
        let nodes = analysis.report.numa_nodes.as_ref().unwrap();
        assert_eq!(nodes.populations.len(), 2);
        assert_eq!(nodes.context.as_ref().unwrap().native().unwrap().partition, source.partition);
        assert!(analysis.report.groups.is_empty() && analysis.report.observed_at.is_none());
        for population in &nodes.populations {
            for entry in &population.entries {
                for field in &entry.fields {
                    if field.unit.is_some() {
                        assert_eq!(field.nature, Some(CounterNature::Gauge));
                        assert_eq!(field.reset, Some(culprit_core::ResetMarker::Unknown));
                    }
                }
            }
        }
        if case == NumaNodeCase::ColumnUnavailable {
            assert!(nodes.populations[1].entries[0].fields.iter().any(|field| field.native_name == "target_kb"
                && field.availability == culprit_core::NumaNodeFieldAvailability::ColumnUnavailable));
        }
        if case == NumaNodeCase::Limited {
            assert_eq!(nodes.populations[0].observed_rows, 257);
            assert_eq!(nodes.populations[0].entries.len(), 256);
            assert!(nodes.populations[0].entries_limited);
        }
    }
}
#[tokio::test]
async fn missing_original_numa_body_keeps_native_counts_and_withdraws_complete_read_instead_of_zero() {
    let (source, rows) = culprit_collector::analytical::test_support::numa_node_evidence(
        culprit_collector::analytical::test_support::NumaNodeCase::Exact,
    );
    let fixture = Fixture::new(source, rows).await;
    fixture.remove_row(TelemetryStream::OperationalSamples, 1);
    let capture =
        fixture.read_families(&[culprit_collector::analytical::evidence::EvidenceFamily::OperationalSamples]).await;
    let report = culprit_engine::collected_resources::instance_resources(&capture).unwrap().report;
    let nodes = report.numa_nodes.unwrap();
    assert!(!nodes.read_complete);
    assert_eq!(nodes.populations[1].native_rows, 1);
    assert_eq!(nodes.populations[1].observed_rows, 0);
    assert_eq!(nodes.populations[1].source_state, culprit_core::SourceState::Partial);
}
#[test]
fn foreign_or_unknown_native_node_owner_dataset_or_counter_semantics_cannot_export() {
    use culprit_collector::analytical::test_support::{NumaNodeCase, numa_node_evidence};
    for change in ["foreign", "unknown", "dataset", "unit"] {
        let (mut source, mut rows) = numa_node_evidence(NumaNodeCase::Exact);
        match change {
            "foreign" => {
                let CollectionTarget::Shared(resource) = &mut source.target else { panic!() };
                resource.engine = culprit_core::Engine::Postgres;
                let SourcePartition::InstanceLocal { member: Some(member), .. } = &mut source.partition else {
                    panic!()
                };
                member.engine = culprit_core::Engine::Postgres;
            }
            "unknown" => source.partition = SourcePartition::InstanceLocal { member: None, epoch: None },
            "dataset" => {
                let SourceObservation::Snapshot(header) = &mut source.observation else { panic!() };
                header.dataset = SnapshotDataset::Operational;
            }
            _ => {
                let TelemetryRecord::Operational(row) = &mut rows[0] else { panic!() };
                let OperationalFact::CounterSet(sample) = &mut row.fact else { panic!() };
                sample.counters[0].unit = CounterUnit::Bytes;
            }
        }
        assert!(
            ExportedRecord::new(
                &source,
                &CaptureIdentity { observation_id: "original-nodes".into(), revision: None },
                rows.remove(0)
            )
            .is_err(),
            "{change}"
        );
    }
}

#[tokio::test]
async fn boxed_cpu_original_pairs_preserve_exact_numerators_rosters_reset_and_capacity_without_requery() {
    use test_support::{ProcessCpuIntervalCase, process_cpu_interval_evidence};
    for case in ProcessCpuIntervalCase::ALL {
        let [(a, ar), (b, br)] = process_cpu_interval_evidence(case);
        let first = Fixture::with_sequence(a.clone(), ar.clone(), 100_001).await;
        let second = Fixture::with_sequence(b.clone(), br.clone(), 100_002).await;
        let opened = (first.recovery_object_reads(), second.recovery_object_reads());
        let left = first.read_families(&[EvidenceFamily::OperationalSamples]).await;
        let right = second.read_families(&[EvidenceFamily::OperationalSamples]).await;
        test_support::assert_operational_capture(&left, &a, &ar);
        test_support::assert_operational_capture(&right, &b, &br);
        let report = culprit_engine::collected_resources::instance_resources(&right).unwrap().report;
        let cpu = report.process_cpu.as_ref().unwrap();
        assert_eq!(cpu.populations.len(), 2);
        assert!(cpu.capture.is_some());
        let interval = culprit_engine::collected_resources::process_cpu_interval(&left, &right).unwrap();
        assert_eq!(interval.available, case.comparable(), "{case:?}");
        if case.comparable() {
            let total = interval.fields.iter().find(|field| field.key == "engine_cpu_delta_ms").unwrap();
            assert_eq!(total.value.as_deref(), Some(if case == ProcessCpuIntervalCase::Zero { "0" } else { "2000" }));
            assert_eq!(
                interval.fields.iter().find(|field| field.key == "capacity_processor_ms").unwrap().value.as_deref(),
                Some("4000")
            );
        }
        if case == ProcessCpuIntervalCase::LinuxIdle || case == ProcessCpuIntervalCase::UnknownPlatform {
            let fields = &cpu.populations[1].entries[0].fields;
            assert_eq!(fields.iter().find(|field| field.key == "idle_cpu_percent").unwrap().value, None);
            assert!(fields.iter().find(|field| field.key == "raw_system_idle").unwrap().value.is_some());
        }
        assert!(!culprit_engine::collected_resources::process_cpu_interval(&left, &left).unwrap().available);
        assert_eq!((first.recovery_object_reads(), second.recovery_object_reads()), opened);
    }
}
#[tokio::test]
async fn missing_pool_body_cannot_become_measured_cpu_but_independent_ring_history_remains_visible() {
    let [(a, rows), (b, other)] =
        test_support::process_cpu_interval_evidence(test_support::ProcessCpuIntervalCase::Exact);
    let first = Fixture::with_sequence(a, rows, 100_003).await;
    let second = Fixture::with_sequence(b, other, 100_004).await;
    first.remove_row(TelemetryStream::OperationalSamples, 5);
    let left = first.read_families(&[EvidenceFamily::OperationalSamples]).await;
    let right = second.read_families(&[EvidenceFamily::OperationalSamples]).await;
    assert!(!left.is_read_complete());
    assert!(!culprit_engine::collected_resources::process_cpu_interval(&left, &right).unwrap().available);
    let report = culprit_engine::collected_resources::instance_resources(&left).unwrap().report;
    let cpu = report.process_cpu.as_ref().unwrap();
    assert_eq!(cpu.source_state, SourceState::Partial);
    assert_eq!(cpu.populations[0].native_rows, 2);
    assert_eq!(cpu.populations[0].observed_rows, 1);
    assert_eq!(cpu.populations[1].entries.len(), 1);
}
#[test]
fn imported_cpu_original_conflicts_withdraw_interval_even_beyond_display_cap_and_sharing_withdraws_proof() {
    let [a, b] = test_support::process_cpu_interval_originals(test_support::ProcessCpuIntervalCase::Exact);
    let mut snapshot: Box<culprit_core::Snapshot> =
        serde_json::from_str(include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../fixtures/mssql-demo.json")))
            .unwrap();
    snapshot.operations.process_cpu_originals = vec![
        RecordedOperationalRead::Captured { read: Box::new(a.clone()) },
        RecordedOperationalRead::Captured { read: Box::new(b) },
    ];
    let report = replay_cpu_originals(&snapshot);
    assert!(report.process_cpu_interval.as_ref().unwrap().available);
    let mut conflict = a;
    conflict.observations.remove(0);
    conflict.origin.positions.remove(0);
    conflict.read_complete = false;
    assert!(conflict.is_valid());
    for _ in 0..MAX_RECORDED_OPERATIONAL_READS {
        snapshot
            .operations
            .process_cpu_originals
            .push(RecordedOperationalRead::Unavailable { reason: culprit_core::message!("view.process_cpu.invalid") });
    }
    snapshot.operations.process_cpu_originals.push(RecordedOperationalRead::Captured { read: Box::new(conflict) });
    let report = replay_cpu_originals(&snapshot);
    assert!(!report.process_cpu_interval.as_ref().unwrap().available);
    assert_eq!(snapshot.operations.process_cpu_originals.len(), MAX_RECORDED_OPERATIONAL_READS + 3);
    let shared = culprit_core::redact::redact_snapshot(&snapshot);
    assert!(
        shared
            .operations
            .process_cpu_originals
            .iter()
            .all(|original| matches!(original, RecordedOperationalRead::Unavailable { .. }))
    );
    let shared = culprit_checks::analyze(shared);
    let text = serde_json::to_string(&shared.instance_resources).unwrap();
    assert!(!text.contains("controlled-cpu") && !text.contains("total_cpu_usage_ms"));
}

#[tokio::test]
async fn boxed_cpu_nonadvancing_ticks_overlapping_acquisition_or_inconsistent_pool_cohorts_refuse_the_interval() {
    for alternative in ["equal-ticks", "backward-ticks", "overlapping-acquisition", "inconsistent-capacity"] {
        let [(a, rows), (mut b, mut other)] =
            test_support::process_cpu_interval_evidence(test_support::ProcessCpuIntervalCase::Exact);
        if alternative == "overlapping-acquisition" {
            let SourceObservation::Snapshot(header) = &mut b.observation else { panic!() };
            header.observed_from = "2020-01-01T12:00:00Z".into();
        }
        for row in &mut other {
            let TelemetryRecord::Operational(row) = row else { panic!() };
            if row.source != MSSQL_CPU_POOLS_SOURCE {
                continue;
            }
            let OperationalFact::CounterSet(sample) = &mut row.fact else { panic!() };
            match alternative {
                "equal-ticks" => {
                    sample.properties.insert("sample_ms_ticks".into(), "1000".into());
                }
                "backward-ticks" => {
                    sample.properties.insert("sample_ms_ticks".into(), "999".into());
                }
                "inconsistent-capacity" if sample.identity.id == "2" => {
                    sample.properties.insert("host_logical_processors".into(), "8".into());
                }
                _ => {}
            }
        }
        let left = Fixture::with_sequence(a, rows, 100_005).await;
        let right = Fixture::with_sequence(b, other, 100_006).await;
        let left = left.read_families(&[EvidenceFamily::OperationalSamples]).await;
        let right = right.read_families(&[EvidenceFamily::OperationalSamples]).await;
        assert!(left.is_read_complete() && right.is_read_complete());
        let comparison = culprit_engine::collected_resources::process_cpu_interval(&left, &right).unwrap();
        assert!(!comparison.available, "{alternative}");
        assert!(comparison.fields.is_empty());
    }
}
#[test]
fn changed_native_cpu_counter_or_owner_cannot_borrow_an_original_structural_receipt() {
    let [first, second] = test_support::process_cpu_interval_originals(test_support::ProcessCpuIntervalCase::Exact);
    let mut changed = second.clone();
    let row = changed.observations.iter_mut().find(|row| row.source == MSSQL_CPU_POOLS_SOURCE).unwrap();
    let OperationalFact::CounterSet(sample) = &mut row.fact else { panic!() };
    sample.counters[0].value = MeasurementValue::Integer("2001".into());
    assert!(!changed.is_valid());
    assert!(culprit_checks::instance_resources::compare_cpu_originals(&first, &changed).is_err());
    let mut foreign = second;
    let SourcePartition::InstanceLocal { epoch, .. } = &mut foreign.description.partition else { panic!() };
    *epoch = Some(digest(b"foreign-process"));
    assert!(!foreign.is_valid());
    assert!(culprit_checks::instance_resources::compare_cpu_originals(&first, &foreign).is_err());
}

fn replay_cpu_originals(snapshot: &culprit_core::Snapshot) -> Box<culprit_core::InstanceResourceReport> {
    let mut report = culprit_checks::analyze(snapshot.clone());
    report.instance_resources.take().unwrap()
}
