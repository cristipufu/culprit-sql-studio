use super::*;
use culprit_core::{MeasurementScope, Pressure, SourceOutcome};
use std::collections::BTreeMap;

fn sample(id: u32) -> NativeCounterSet {
    let mut sample = NativeCounterSet {
        identity: NativeIdentity::new(id.to_string()).qualified("native_object", MSSQL_SCHEDULER_OBJECT),
        scope: MeasurementScope::Server,
        counters: vec![],
        properties: BTreeMap::new(),
        ratios: vec![],
        ratio_gaps: vec![],
    };
    for field in MSSQL_SCHEDULER_FIELDS {
        match field.kind {
            MssqlNativeFieldKind::Identity => (),
            MssqlNativeFieldKind::IntegerProperty | MssqlNativeFieldKind::BooleanProperty => {
                sample.properties.insert(field.name.into(), "0".into());
            }
            MssqlNativeFieldKind::TextProperty => {
                sample.properties.insert(field.name.into(), "HIDDEN ONLINE".into());
            }
            MssqlNativeFieldKind::UnqualifiedCounter { .. } => {
                sample.properties.insert(field.name.into(), "0".into());
            }
            MssqlNativeFieldKind::Counter { unit, nature } => sample.counters.push(NativeCounterSample {
                key: field.name.into(),
                value: MeasurementValue::Integer(
                    if field.name == "work_queue_count" { "9007199254740993" } else { "0" }.into(),
                ),
                unit,
                nature,
                scope: MeasurementScope::Server,
                reset: ResetMarker::Unknown,
                native_object: MSSQL_SCHEDULER_OBJECT.into(),
                native_name: field.name.into(),
                native_instance: None,
                native_type: None,
            }),
        }
    }
    sample
}
fn source(rows: u32) -> CollectionDescription {
    let owner = SharedResourceIdentity {
        engine: Engine::Mssql,
        kind: SharedResourceKind::Instance,
        namespace: NativeIdentity::new("private-scheduler-namespace"),
        native: NativeIdentity::new("private-scheduler-instance"),
    };
    CollectionDescription {
        target: CollectionTarget::Shared(owner.clone()),
        source: "shared-operational-state".into(),
        partition: SourcePartition::InstanceLocal { member: Some(owner), epoch: Some("private-process-epoch".into()) },
        continuity: SourceContinuity::Unverified,
        observation: SourceObservation::Snapshot(StateSnapshotDescription {
            dataset: SnapshotDataset::Operational,
            observed_from: "2020-01-01T00:00:00Z".into(),
            observed_until: "2020-01-01T00:00:01Z".into(),
            records: rows,
            sources: vec![StateSourceCoverage::collected(culprit_core::source_id::SCHEDULERS.into(), rows)],
        }),
    }
}
fn observation(sample: NativeCounterSet) -> OperationalObservation {
    OperationalObservation::from_counters(culprit_core::source_id::SCHEDULERS, sample).unwrap()
}
fn explain(source: &CollectionDescription, rows: &[OperationalObservation], complete: bool) -> Result<SchedulerReport> {
    let coverage = &source.observation.snapshot().unwrap().sources[0];
    explain_source(source, coverage, &rows.iter().collect::<Vec<_>>(), complete)
}
fn field<'a>(report: &'a SchedulerReport, name: &str) -> &'a SchedulerField {
    report.entries[0].fields.iter().find(|field| field.native_name == name).unwrap()
}

#[test]
fn original_scheduler_rows_preserve_local_identity_all_native_states_exact_values_and_separate_source_bounds() {
    let source = source(1);
    let report = explain(&source, &[observation(sample(0))], true).unwrap();
    assert_eq!(report.entries[0].scheduler_id, "0");
    assert_eq!(report.native_rows, Some(1));
    assert_eq!(report.observed_rows, 1);
    assert_eq!(report.native_source.as_ref().unwrap().source, culprit_core::source_id::SCHEDULERS);
    assert_eq!(report.context.as_ref().unwrap().bounds(), ("2020-01-01T00:00:00Z", "2020-01-01T00:00:01Z"));
    assert_eq!(field(&report, "status").value.as_deref(), Some("HIDDEN ONLINE"));
    assert_eq!(field(&report, "work_queue_count").value.as_deref(), Some("9007199254740993"));
    assert_eq!(field(&report, "pending_disk_io_count").value.as_deref(), Some("0"));
    assert_eq!(field(&report, "total_cpu_usage_ms").unit, Some(CounterUnit::Milliseconds));
    assert_eq!(field(&report, "total_cpu_usage_ms").nature, Some(CounterNature::Cumulative));
    assert_eq!(field(&report, "total_cpu_usage_ms").reset, Some(ResetMarker::Unknown));
    assert_eq!(field(&report, "yield_count").unit, Some(CounterUnit::Unknown));
    assert_eq!(report.entries[0].fields.len(), MSSQL_SCHEDULER_FIELDS.len() - 1);
    assert!(report.limitations.contains(&t!("view.schedulers.limit.scope")));
    assert!(!report.entries[0].fields.iter().any(|field| field.native_name.contains("percent")));
}

#[test]
fn native_null_column_unavailability_and_measured_zero_have_different_typed_output() {
    let mut sample = sample(0);
    sample.properties.remove("failed_to_create_worker");
    sample.properties.insert("native_null_columns".into(), "[\"failed_to_create_worker\"]".into());
    sample.counters.retain(|counter| counter.key != "ideal_workers_limit");
    sample.properties.insert("unavailable_columns".into(), "[\"ideal_workers_limit\"]".into());
    let report = explain(&source(1), &[observation(sample)], true).unwrap();
    assert_eq!(field(&report, "failed_to_create_worker").availability, SchedulerFieldAvailability::NativeNull);
    assert_eq!(field(&report, "ideal_workers_limit").availability, SchedulerFieldAvailability::ColumnUnavailable);
    assert_eq!(field(&report, "pending_disk_io_count").availability, SchedulerFieldAvailability::Observed);
    assert!(field(&report, "failed_to_create_worker").value.is_none());
    assert!(field(&report, "ideal_workers_limit").value.is_none());
}

#[test]
fn missing_retained_rows_stay_partial_and_do_not_replace_original_native_population_with_zero() {
    let source = source(2);
    let report = explain(&source, &[observation(sample(0))], false).unwrap();
    assert_eq!(report.source_state, SourceState::Partial);
    assert_eq!(report.native_source.as_ref().unwrap().state, SourceState::Collected);
    assert_eq!(report.native_rows, Some(2));
    assert_eq!(report.observed_rows, 1);
    assert_eq!(report.read_complete, Some(false));
    let absent = explain(&source, &[], false).unwrap();
    assert_eq!(absent.source_state, SourceState::Partial);
    assert!(absent.entries.is_empty());
    assert!(explain(&source, &[], true).is_err());
}

#[test]
fn denied_source_remains_unavailable_independently_of_other_successful_native_families() {
    let mut source = source(1);
    let SourceObservation::Snapshot(header) = &mut source.observation else { panic!() };
    header.records = 0;
    header.sources[0] = StateSourceCoverage::unavailable(
        culprit_core::source_id::SCHEDULERS.into(),
        culprit_core::ErrorCode::PermissionDenied,
        None,
    );
    let report = explain(&source, &[], true).unwrap();
    assert_eq!(report.source_state, SourceState::Unavailable);
    assert_eq!(report.native_rows, Some(0));
    assert!(report.entries.is_empty());
    assert_eq!(report.native_source.as_ref().unwrap().error_code, Some(culprit_core::ErrorCode::PermissionDenied));
    assert!(report.source_reason.is_some());
}

#[test]
fn foreign_engine_scope_source_and_unordered_bounds_refuse_measured_scheduler_context() {
    for mutation in ["engine", "scope", "source", "clock"] {
        let mut source = source(1);
        match mutation {
            "engine" => {
                let CollectionTarget::Shared(target) = &mut source.target else { panic!() };
                target.engine = Engine::Postgres;
                let SourcePartition::InstanceLocal { member: Some(member), .. } = &mut source.partition else {
                    panic!()
                };
                member.engine = Engine::Postgres;
            }
            "scope" => {
                let CollectionTarget::Shared(target) = &mut source.target else { panic!() };
                target.kind = SharedResourceKind::Pool;
                let SourcePartition::InstanceLocal { member: Some(member), .. } = &mut source.partition else {
                    panic!()
                };
                member.kind = SharedResourceKind::Pool;
            }
            "source" => {
                let SourceObservation::Snapshot(header) = &mut source.observation else { panic!() };
                header.sources[0].source = "other-source".into();
            }
            "clock" => {
                let SourceObservation::Snapshot(header) = &mut source.observation else { panic!() };
                header.observed_until = "2019-01-01T00:00:00Z".into();
            }
            _ => unreachable!(),
        }
        assert!(explain(&source, &[observation(sample(0))], true).is_err(), "{mutation}");
    }
}

#[test]
fn duplicate_or_invalid_tail_rows_are_rejected_before_the_presentation_cap() {
    let mut samples = (0..=MAX_PRESENTED_SCHEDULERS).map(|id| sample(id as u32)).collect::<Vec<_>>();
    let refs = samples.iter().collect::<Vec<_>>();
    let report = render(&refs, SourceState::Collected).unwrap();
    assert_eq!(report.observed_rows, 257);
    assert_eq!(report.presented_rows, 256);
    assert!(report.entries_limited);
    let last = samples.len() - 1;
    samples[last] = samples[0].clone();
    assert!(render(&samples.iter().collect::<Vec<_>>(), SourceState::Collected).is_err());
    samples[last] = sample(last as u32);
    samples[last].properties.insert("unknown-private-label".into(), "private SQL canary".into());
    assert!(render(&samples.iter().collect::<Vec<_>>(), SourceState::Collected).is_err());
}

#[test]
fn ordinary_snapshot_rows_have_no_invented_native_read_interval_and_sharing_withdraws_private_native_details() {
    let mut snapshot: Snapshot =
        serde_json::from_str(include_str!("../../../../../../fixtures/mssql-demo.json")).unwrap();
    snapshot.pressure = Some(Pressure { resource_counters: Some(vec![sample(0)]), ..Default::default() });
    snapshot.sources.retain(|source| source.source != culprit_core::source_id::SCHEDULERS);
    snapshot.sources.push(SourceOutcome {
        source: culprit_core::source_id::SCHEDULERS.into(),
        state: SourceState::Collected,
        reason: None,
        reason_message: None,
    });
    let report = explain_snapshot(&snapshot).unwrap().unwrap();
    assert!(report.context.is_none() && report.read_complete.is_none() && report.native_rows.is_none());
    let original = serde_json::to_value(&snapshot).unwrap();
    let shared = culprit_core::redact::redact_snapshot(&snapshot);
    assert!(explain_snapshot(&shared).is_err());
    let parent = super::super::explain_snapshot(&shared).unwrap().unwrap();
    let withheld = parent.schedulers.as_deref().unwrap();
    assert_eq!(withheld.source_state, SourceState::Unavailable);
    assert!(withheld.entries.is_empty());
    assert!(withheld.limitations.contains(&t!("view.schedulers.invalid")));
    assert_eq!(serde_json::to_value(&snapshot).unwrap(), original);
    let json = serde_json::to_string(&shared).unwrap();
    assert!(!json.contains("HIDDEN ONLINE"));
    assert!(!json.contains("sys.dm_os_schedulers"));
}

#[test]
fn modern_counters_keep_native_populations_while_unqualified_zero_and_positive_spinlock_values_are_withheld() {
    for value in ["0", "7"] {
        let mut sample = sample(0);
        for field in MSSQL_SCHEDULER_FIELDS
            .iter()
            .filter(|field| matches!(field.kind, MssqlNativeFieldKind::UnqualifiedCounter { .. }))
        {
            sample.properties.insert(field.name.into(), value.into());
        }
        let report = explain(&source(1), &[observation(sample)], true).unwrap();
        for name in ["spinlock_wait_time_ms", "spinlock_max_wait_time_ms", "spinlock_wait_count"] {
            let field = field(&report, name);
            assert_eq!(field.availability, SchedulerFieldAvailability::InstrumentationUnqualified);
            assert!(field.value.is_none());
            let raw = field.unqualified.as_ref().unwrap();
            assert_eq!(raw.native_value, value);
            assert_eq!(raw.required_global_trace_flag, 8134);
            assert!(field.explanation.as_ref().unwrap().contains("8134"));
        }
        assert_eq!(field(&report, "queued_disk_io_count").nature, Some(CounterNature::Gauge));
        assert_eq!(field(&report, "total_signal_time").unit, Some(CounterUnit::Milliseconds));
        assert_eq!(field(&report, "total_signal_time").reset, Some(ResetMarker::Unknown));
        assert_eq!(field(&report, "total_completed_tasks").explanation, Some(t!("view.schedulers.counter.removed")));
    }
}
