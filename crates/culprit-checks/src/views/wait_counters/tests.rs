use super::*;
use culprit_core::telemetry::*;
use std::time::{Duration, SystemTime};

fn read() -> WaitCounterSnapshot {
    let observations = ["WRITELOG", "SLEEP_TASK", "RBIO_RG_STORAGE"]
        .into_iter()
        .map(|name| WaitCounterObservation {
            source: culprit_core::source_id::WAITS.into(),
            wait_type: name.into(),
            mechanism: Engine::Mssql.spec().native_wait_mechanism(name),
            scope: MeasurementScope::Server,
            reset: ResetMarker::Unknown,
            waiting_tasks_count: "9007199254740993".into(),
            wait_time_ms: "9007199254740994".into(),
            signal_wait_time_ms: "1".into(),
            max_wait_time_ms: "2".into(),
        })
        .collect::<Vec<_>>();
    WaitCounterSnapshot {
        pool_current: None,
        context: Some(
            ObservationReadContext::collector(SystemTime::UNIX_EPOCH, SystemTime::UNIX_EPOCH + Duration::from_nanos(1))
                .unwrap(),
        ),
        sources: vec![StateSourceCoverage::collected(culprit_core::source_id::WAITS.into(), observations.len() as u32)],
        observations,
        read_complete: true,
    }
}

#[test]
fn all_native_rows_survive_without_category_filtering_float_rounding_or_current_pressure_claims() {
    let read = read();
    let report = explain(&read, Engine::Mssql).unwrap();
    assert_eq!((report.native_rows, report.observed_rows, report.presented_rows), (3, 3, 3));
    for (entry, original) in report.entries.iter().zip(&read.observations) {
        assert_eq!(&entry.observation, original);
        assert_eq!(entry.resource_wait_time_ms.as_deref(), Some("9007199254740993"));
        assert!(entry.issues.is_empty());
        assert_eq!(entry.metrics[0].text.as_deref().unwrap().replace(',', ""), "9007199254740993");
    }
    assert!(report.entries.iter().any(|entry| entry.title == "SLEEP_TASK"));
    assert_eq!(report.context, read.context);
    assert!(report.limitations.iter().any(|text| text == &t!("view.wait_counters.limit.client_clock")));
}

#[test]
fn native_visibility_and_missing_archive_bodies_cannot_become_empty_or_healthy() {
    let mut read = read();
    read.sources[0] = StateSourceCoverage::observed(culprit_core::source_id::WAITS.into(), 3, false);
    let native_partial = explain(&read, Engine::Mssql).unwrap();
    assert!(native_partial.read_complete);
    assert_eq!(native_partial.sources[0].state, SourceState::Partial);
    read.observations.pop();
    read.read_complete = false;
    let missing = explain(&read, Engine::Mssql).unwrap();
    assert_eq!((missing.native_rows, missing.observed_rows), (3, 2));
    assert!(!missing.read_complete);
    assert_eq!(missing.sources[0].state, SourceState::Partial);
    read.read_complete = true;
    assert!(explain(&read, Engine::Mssql).is_err());
    read.observations.clear();
    read.sources[0] = StateSourceCoverage::unavailable(
        culprit_core::source_id::WAITS.into(),
        culprit_core::ErrorCode::PermissionDenied,
        None,
    );
    let unavailable = explain(&read, Engine::Mssql).unwrap();
    assert_eq!(unavailable.sources[0].state, SourceState::Unavailable);
    assert_eq!(unavailable.summary, t!("view.wait_counters.summary.partial"));
    read.sources[0] = StateSourceCoverage::collected(culprit_core::source_id::WAITS.into(), 0);
    let empty = explain(&read, Engine::Mssql).unwrap();
    assert_eq!(empty.summary, t!("view.wait_counters.summary.empty"));
    assert_ne!(unavailable.summary, empty.summary);
}

#[test]
fn inconsistent_signal_keeps_both_original_values_and_withholds_only_derived_resource_time() {
    let mut read = read();
    read.observations[0].signal_wait_time_ms = "9007199254740995".into();
    let report = explain(&read, Engine::Mssql).unwrap();
    let entry = &report.entries[0];
    assert_eq!(entry.issues, vec![WaitCounterIssue::SignalExceedsTotal]);
    assert_eq!(entry.observation.wait_time_ms, "9007199254740994");
    assert_eq!(entry.observation.signal_wait_time_ms, "9007199254740995");
    assert_eq!(entry.resource_wait_time_ms, None);
    assert_eq!(entry.metrics[4].text.as_deref(), Some(t!("view.wait_counters.unknown").as_str()));
}

#[test]
fn presentation_limits_do_not_shrink_native_population_or_body_completeness() {
    let mut read = read();
    let row = read.observations[0].clone();
    read.observations =
        (0..300).map(|n| WaitCounterObservation { wait_type: format!("CUSTOM_{n}"), ..row.clone() }).collect();
    read.sources[0].records = 300;
    let report = explain(&read, Engine::Mssql).unwrap();
    assert_eq!((report.native_rows, report.observed_rows, report.presented_rows), (300, 300, 256));
    assert!(report.entries_limited && report.read_complete);
    assert_eq!(read.observations.len(), 300);
}

#[test]
fn saved_scan_pipeline_reuses_the_exact_view_without_borrowing_a_later_probe_or_foreign_namespace() {
    let mut snapshot: Snapshot = serde_json::from_str(include_str!("../../../../../fixtures/mssql-demo.json")).unwrap();
    let read = read();
    snapshot.pressure.get_or_insert_with(Default::default).wait_counters = Some(Box::new(read.clone()));
    let report = crate::analyze(snapshot.clone());
    assert_eq!(
        serde_json::to_value(report.wait_counters.unwrap().entries).unwrap(),
        serde_json::to_value(explain(&read, Engine::Mssql).unwrap().entries).unwrap()
    );
    let target = culprit_core::DatabaseIdentity::server(Engine::Postgres, "foreign", "owned.invalid", 5432, None);
    let source = CollectionDescription {
        target: target.into(),
        partition: SourcePartition::Target,
        source: "logical-waits".into(),
        continuity: SourceContinuity::Unverified,
        observation: SourceObservation::Snapshot(StateSnapshotDescription {
            dataset: SnapshotDataset::WaitCounters,
            observed_from: "2020-01-01T00:00:00Z".into(),
            observed_until: "2020-01-01T00:00:01Z".into(),
            records: read.observations.len() as u32,
            sources: read.sources.clone(),
        }),
    };
    let mut foreign = read.clone();
    foreign.context =
        Some(ObservationReadContext::Native { context: Box::new(NativeReadContext::from_snapshot(&source).unwrap()) });
    snapshot.pressure.as_mut().unwrap().wait_counters = Some(Box::new(foreign));
    assert!(explain_snapshot(&snapshot).is_err());
    let denied = crate::analyze(snapshot);
    assert!(denied.wait_counters.is_none());
    assert!(
        denied
            .snapshot
            .sources
            .iter()
            .any(|source| source.source == culprit_core::source_id::WAITS && source.state == SourceState::Unavailable)
    );
}

#[test]
fn unknown_original_namespace_keeps_independent_waits_with_an_explicit_qualification_and_no_clock_substitution() {
    let mut snapshot: Snapshot = serde_json::from_str(include_str!("../../../../../fixtures/mssql-demo.json")).unwrap();
    snapshot.identity = None;
    let mut read = read();
    let source = CollectionDescription {
        target: culprit_core::DatabaseIdentity::server(
            Engine::Mssql,
            &snapshot.server.database,
            "unverified-original.invalid",
            1433,
            None,
        )
        .into(),
        source: "logical-waits".into(),
        partition: SourcePartition::Target,
        continuity: SourceContinuity::Unverified,
        observation: SourceObservation::Snapshot(StateSnapshotDescription {
            dataset: SnapshotDataset::WaitCounters,
            observed_from: "2020-01-01T00:00:00.000000001Z".into(),
            observed_until: "2020-01-01T00:00:00.000000009Z".into(),
            records: read.observations.len() as u32,
            sources: read.sources.clone(),
        }),
    };
    read.context =
        Some(ObservationReadContext::Native { context: Box::new(NativeReadContext::from_snapshot(&source).unwrap()) });
    let original = read.context.clone();
    snapshot.pressure.get_or_insert_with(Default::default).wait_counters = Some(Box::new(read));
    let report = explain_snapshot(&snapshot).unwrap().unwrap();
    assert_eq!(report.context, original);
    assert_eq!(report.entries.len(), 3);
    assert!(report.limitations.contains(&t!("view.observations.limit.namespace")));
    assert!(report.limitations.contains(&t!("view.wait_counters.limit.native_clock")));
    let mechanism = Engine::Mssql.spec().native_wait_mechanism("RBIO_RG_STORAGE").unwrap();
    assert!(report.entries[2].explanation.contains(&mechanism.label()));
    snapshot.pressure.as_mut().unwrap().wait_counters.as_mut().unwrap().context = None;
    let report = explain_snapshot(&snapshot).unwrap().unwrap();
    assert!(report.limitations.contains(&t!("view.wait_counters.limit.clock_unavailable")));
    assert!(report.context.is_none());
}
