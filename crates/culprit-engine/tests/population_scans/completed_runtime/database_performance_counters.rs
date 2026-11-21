//! Original database counter populations attach after the one native scan owner settles; no counter query is added.
use super::*;
use culprit_collector::analytical::test_support::{
    DatabasePerformanceCounterCase as Case, PerformanceCounterCase, database_performance_counter_evidence,
    performance_counter_evidence,
};
use culprit_core::{PerformanceCounterPopulation as Population, PerformanceCounterReport, Report};
use culprit_engine::{
    LocalCaptureAnalysisKind, LocalCaptureRequest, LocalCapturedAnalysis, LocalSnapshotAnalysis, OutboxCaptureReader,
};

fn with_shared_counters() -> PopulationSnapshots {
    let mut batch = snapshots();
    let (_, rows) = performance_counter_evidence(PerformanceCounterCase::Exact);
    let sets = rows
        .into_iter()
        .filter_map(|row| match row {
            TelemetryRecord::Operational(OperationalObservation {
                source,
                fact: OperationalFact::CounterSet(set),
                ..
            }) if source == culprit_core::source_id::PERFORMANCE_COUNTERS => Some(*set),
            _ => None,
        })
        .collect();
    batch.requested.pressure.get_or_insert_with(Default::default).resource_counters = Some(sets);
    batch.requested.sources.retain(|source| source.source != culprit_core::source_id::PERFORMANCE_COUNTERS);
    batch.requested.sources.push(culprit_core::SourceOutcome {
        source: culprit_core::source_id::PERFORMANCE_COUNTERS.into(),
        state: SourceState::Collected,
        reason: None,
        reason_message: None,
    });
    batch
}
fn selected(report: &Report) -> &PerformanceCounterReport {
    report.performance_counters.iter().find(|report| report.population == Population::SelectedDatabase).unwrap()
}
fn original(report: &Report) -> &CapturedOperationalRead {
    let Some(RecordedPerformanceCounters::Captured { read }) =
        &report.snapshot.operations.database_performance_counters
    else {
        panic!("the exact captured original must remain retained")
    };
    read
}
fn assert_original(report: &Report, source: &CollectionDescription) {
    let read = original(report);
    assert!(read.is_valid() && read.read_complete);
    assert_eq!(&read.description, source);
    assert_eq!(read.captured_references().unwrap().len(), read.observations.len());
    let context = read.context().unwrap();
    assert_eq!(selected(report).context.as_ref().unwrap().native(), Some(&context));
    assert_eq!(selected(report).database.as_deref(), Some("shop"));
    assert_eq!(selected(report).native_database_id.as_deref(), Some("7"));
    assert_eq!(selected(report).native_observations, Some(source.observation.snapshot().unwrap().records));
    assert_eq!(context.database_writable, Some(true));
    assert!(source.target.database().unwrap().incarnation.is_none(), "no global namespace is invented");
    assert_ne!(context.observed_from, report.snapshot.taken_at);
    assert!(selected(report).limitations.contains(&culprit_core::t!("view.performance_counters.limit.historical")));
    assert_eq!(
        report.evidence.as_ref().unwrap().collected.as_ref().unwrap().operations.database_performance_counters,
        report.snapshot.operations.database_performance_counters,
    );
}
async fn analyze(workspace: &Workspace, captured: &Captured) -> LocalSnapshotAnalysis {
    let reader = OutboxCaptureReader::delivery(captured.outbox.delivery(), CompletedCaptureBudget::default()).unwrap();
    let guard = culprit_core::limits::OperationGuard::new(std::time::Duration::from_secs(5)).unwrap();
    let analysis = workspace
        .analyze_local_capture(
            &reader,
            LocalCaptureRequest {
                connection: "shop",
                observation_id: &captured.manifest.capture.observation_id,
                analysis: LocalCaptureAnalysisKind::PerformanceCounters,
                operation: guard.handle(),
            },
        )
        .await
        .unwrap();
    let LocalCapturedAnalysis::Snapshot(analysis) = analysis else { panic!("canonical original snapshot analysis") };
    analysis
}

#[tokio::test]
async fn scoped_counter_populations_attach_once_after_shutdown_and_replay_after_original_files_disappear() {
    for case in Case::ALL {
        let (source, rows) = database_performance_counter_evidence(case);
        let captured = Captured::from_source("shop", source.clone(), rows).await;
        let (_home, workspace, state) = workspace(with_shared_counters(), SourceShutdown::Quiescent);
        let reports = crate::observed::run(&workspace, culprit_core::ProviderReadKind::Scan, async |operation| {
            workspace
                .scan_populations_with_completed_capture(
                    "shop",
                    &options(),
                    CompletedCaptureScanInput {
                        analysis: CompletedCaptureAnalysis::DatabasePerformanceCounters,
                        ..captured.input(&captured.outbox.delivery(), &CollectionCancellation::default())
                    },
                    operation,
                )
                .await
        })
        .await
        .unwrap();
        assert_original(&reports.requested, &source);
        assert_eq!(reports.requested.performance_counters.len(), 2);
        let shared = &reports.requested.performance_counters[0];
        assert_eq!(shared.population, Population::SharedServer);
        assert_eq!(shared.native_database_id, None);
        assert_eq!(shared.native_counter_rows, Some(11));
        assert_eq!(shared.source_state, Some(SourceState::Collected));
        let database = selected(&reports.requested);
        assert_eq!(database.source_state, Some(source.observation.snapshot().unwrap().sources[0].state));
        assert_eq!(
            database.observed_counter_rows,
            if case == Case::Denied {
                0
            } else if case == Case::Partial {
                2
            } else {
                3
            }
        );
        assert!(database.ratios.is_none());
        assert!(
            reports.populations.iter().all(|report| report.snapshot.operations.database_performance_counters.is_none()
                && report.performance_counters.iter().all(|report| report.population != Population::SelectedDatabase))
        );
        assert_eq!(state.active.load(Ordering::SeqCst), 0);
        assert_eq!(state.acquisitions.load(Ordering::SeqCst), 1);
        let id = reports.requested.snapshot.id.clone();
        let expected = serde_json::to_value(&reports.requested.performance_counters).unwrap();
        drop(captured);
        let restored = workspace.report_at("shop", &id).unwrap();
        assert_original(&restored, &source);
        assert_eq!(serde_json::to_value(&restored.performance_counters).unwrap(), expected);
        assert_eq!(
            serde_json::to_value(culprit_engine::reports::display(&restored)).unwrap()["performance_counters"],
            expected
        );
        assert_eq!(state.factories.load(Ordering::SeqCst), 1);
    }
}

#[tokio::test]
async fn saved_connection_association_never_borrows_current_database_route_member_or_host_time() {
    for changed in ["route", "database", "member", "unknown-current"] {
        let (mut source, rows) = database_performance_counter_evidence(Case::Exact);
        if changed == "route" {
            let CollectionTarget::Database(database) = &mut source.target else { panic!() };
            database.location =
                DatabaseLocation::Server { host: "old-route.invalid".into(), port: 1444, instance: None };
        }
        if changed == "member" {
            let SourcePartition::InstanceLocal { member: Some(member), .. } = &mut source.partition else { panic!() };
            member.native.id = "another-original-member".into();
        }
        let captured = Captured::from_source("shop", source.clone(), rows).await;
        let mut batch = with_shared_counters();
        if changed == "database" {
            let current =
                DatabaseIdentity::server(Engine::Mssql, "current-database", "current-route.invalid", 1433, None);
            for snapshot in std::iter::once(&mut batch.requested).chain(batch.populations.iter_mut()) {
                snapshot.server.database = current.database.clone();
                snapshot.bind_identity(current.clone());
            }
        } else if changed == "unknown-current" {
            batch.requested.identity = None;
            // An unidentified current scan cannot assert that several runtime populations share a database.
            batch.requested.runtime_populations = None;
            batch.populations.clear();
            batch.catalog = None;
        }
        batch.validate(&options()).expect("the current native population fixture must remain coherent");
        let (_home, workspace, state) = workspace(batch, SourceShutdown::Quiescent);
        let analysis = analyze(&workspace, &captured).await;
        let report = crate::observed::run(&workspace, culprit_core::ProviderReadKind::Scan, async |operation| {
            workspace
                .scan_with_captured_database_performance_counters("shop", &options(), None, analysis, operation)
                .await
        })
        .await
        .unwrap_or_else(|error| panic!("{changed}: {error:?}"));
        assert_original(&report, &source);
        assert_eq!(selected(&report).context.as_ref().unwrap().native().unwrap().partition, source.partition);
        assert_eq!(state.factories.load(Ordering::SeqCst), 1);
    }
}

#[tokio::test]
async fn wrong_saved_connection_or_shared_counter_dataset_refuses_before_the_native_scan() {
    for wrong in ["connection", "dataset"] {
        let (source, rows) = if wrong == "connection" {
            database_performance_counter_evidence(Case::Exact)
        } else {
            performance_counter_evidence(PerformanceCounterCase::Exact)
        };
        let captured = Captured::from_source("shop", source, rows).await;
        let (_home, workspace, state) = workspace(with_shared_counters(), SourceShutdown::Quiescent);
        let analysis = analyze(&workspace, &captured).await;
        let result = crate::observed::run(&workspace, culprit_core::ProviderReadKind::Scan, async |operation| {
            workspace
                .scan_with_captured_database_performance_counters(
                    if wrong == "connection" { "foreign-connection" } else { "shop" },
                    &options(),
                    None,
                    analysis,
                    operation,
                )
                .await
        })
        .await;
        assert!(result.is_err(), "{wrong}");
        assert_eq!(state.factories.load(Ordering::SeqCst), 0);
        assert!(workspace.report_history("shop").unwrap().is_empty());
    }
}

#[tokio::test]
async fn conflict_or_unconfirmed_scan_shutdown_cannot_publish_a_counter_attachment() {
    for conflict in [true, false] {
        let (source, rows) = database_performance_counter_evidence(Case::Exact);
        let captured = Captured::from_source("shop", source, rows).await;
        let mut batch = with_shared_counters();
        if conflict {
            batch.requested.operations.database_performance_counters = Some(RecordedPerformanceCounters::Unavailable {
                reason: culprit_core::message!("view.performance_counters.invalid"),
            });
        }
        let (_home, workspace, state) =
            workspace(batch, if conflict { SourceShutdown::Quiescent } else { SourceShutdown::Unconfirmed });
        let analysis = analyze(&workspace, &captured).await;
        let error = crate::observed::run(&workspace, culprit_core::ProviderReadKind::Scan, async |operation| {
            workspace
                .scan_with_captured_database_performance_counters("shop", &options(), None, analysis, operation)
                .await
        })
        .await
        .unwrap_err();
        if conflict {
            assert_eq!(error.detail, Some(culprit_core::ErrorDetail::Delivery(DeliveryProblem::Conflict)));
        } else {
            assert_eq!(CollectionStop::from_error(&error), CollectionStop::CancellationUnconfirmed);
        }
        assert_eq!(state.active.load(Ordering::SeqCst), 0);
        assert!(state.analyzed.lock().unwrap().is_empty());
        assert!(workspace.report_history("shop").unwrap().is_empty());
    }
}

#[tokio::test]
async fn sharing_keeps_an_independent_typed_database_gap_and_the_shared_population() {
    let (source, rows) = database_performance_counter_evidence(Case::Exact);
    let captured = Captured::from_source("shop", source, rows).await;
    let (_home, workspace, _) = workspace(with_shared_counters(), SourceShutdown::Quiescent);
    let analysis = analyze(&workspace, &captured).await;
    let report = crate::observed::run(&workspace, culprit_core::ProviderReadKind::Scan, async |operation| {
        workspace.scan_with_captured_database_performance_counters("shop", &options(), None, analysis, operation).await
    })
    .await
    .unwrap();
    let shared = culprit_core::redact::redact_snapshot(&report.snapshot);
    assert!(matches!(
        shared.operations.database_performance_counters,
        Some(RecordedPerformanceCounters::Unavailable { .. })
    ));
    let reparsed: Snapshot = serde_json::from_slice(&serde_json::to_vec(&shared).unwrap()).unwrap();
    let replay = culprit_checks::analyze(reparsed);
    assert_eq!(replay.performance_counters.len(), 2);
    assert_eq!(replay.performance_counters[0].population, Population::SharedServer);
    assert_eq!(replay.performance_counters[0].source_state, Some(SourceState::Collected));
    assert_eq!(selected(&replay).population, Population::SelectedDatabase);
    assert_eq!(selected(&replay).source_state, Some(SourceState::Unavailable));
    assert!(selected(&replay).context.is_none() && selected(&replay).native_database_id.is_none());
    assert!(selected(&replay).entries.is_empty());
    let text = serde_json::to_string(&shared).unwrap();
    for private in [
        "private-database-guid-canary",
        "private-fork-guid-canary",
        "private-file-guid-canary",
        "private-process-canary",
        "private-member-canary",
    ] {
        assert!(!text.contains(private), "{private}");
    }
    assert_eq!(
        culprit_core::redact::redact_snapshot(&shared).operations.database_performance_counters,
        shared.operations.database_performance_counters,
    );
}

#[tokio::test]
async fn cancelled_local_verification_or_foreign_capture_receipt_cannot_open_the_native_scan() {
    for cause in ["cancelled", "receipt"] {
        let (source, rows) = database_performance_counter_evidence(Case::Exact);
        let captured = Captured::from_source("shop", source, rows).await;
        let (_home, workspace, state) = workspace(with_shared_counters(), SourceShutdown::Quiescent);
        let cancellation = CollectionCancellation::default();
        let mut attempt = captured.attempt.clone();
        if cause == "cancelled" {
            cancellation.cancel();
        } else {
            attempt.manifest_sha256 = Some(digest(b"foreign-original-manifest"));
        }
        let result = crate::observed::run(&workspace, culprit_core::ProviderReadKind::Scan, async |operation| {
            workspace
                .scan_with_completed_capture(
                    "shop",
                    &options(),
                    CompletedCaptureScanInput {
                        delivery: &captured.outbox.delivery(),
                        analysis: CompletedCaptureAnalysis::DatabasePerformanceCounters,
                        attempt: &attempt,
                        budget: CompletedCaptureBudget::default(),
                        cancellation: &cancellation,
                    },
                    operation,
                )
                .await
        })
        .await;
        assert!(result.is_err(), "{cause}");
        assert_eq!(state.factories.load(Ordering::SeqCst), 0);
        assert!(workspace.report_history("shop").unwrap().is_empty());
    }
}

#[tokio::test]
async fn saved_common_inputs_replay_independently_of_selected_database_counters_and_withdraw_on_sharing() {
    use culprit_collector::analytical::test_support::{
        CommonPerformanceCounterCase, common_performance_counter_evidence,
    };
    let (source, rows) = database_performance_counter_evidence(Case::Exact);
    let captured = Captured::from_source("shop", source, rows).await;
    let mut batch = with_shared_counters();
    let (_, rows) = common_performance_counter_evidence(CommonPerformanceCounterCase::Observed);
    let sets = rows
        .into_iter()
        .filter_map(|row| match row {
            TelemetryRecord::Operational(OperationalObservation {
                source,
                fact: OperationalFact::CounterSet(set),
                ..
            }) if source == culprit_core::source_id::PERFORMANCE_COUNTERS => Some(*set),
            _ => None,
        })
        .collect();
    batch.requested.pressure.as_mut().unwrap().resource_counters = Some(sets);
    batch
        .requested
        .sources
        .iter_mut()
        .find(|source| source.source == culprit_core::source_id::PERFORMANCE_COUNTERS)
        .unwrap()
        .state = SourceState::Partial;
    let (_home, workspace, state) = workspace(batch, SourceShutdown::Quiescent);
    let analysis = analyze(&workspace, &captured).await;
    let report = crate::observed::run(&workspace, culprit_core::ProviderReadKind::Scan, async |operation| {
        workspace.scan_with_captured_database_performance_counters("shop", &options(), None, analysis, operation).await
    })
    .await
    .unwrap();
    let shared = &report.performance_counters[0];
    assert_eq!(shared.source_state, Some(SourceState::Partial));
    assert!(shared.context.is_none(), "scan points cannot borrow the selected capture's original clock or owner");
    assert!(
        shared.entries.iter().any(|entry| entry.field == "common_tempdb_free" && entry.raw_value == "9007199254740993")
    );
    let expected = serde_json::to_value(&report.performance_counters).unwrap();
    let id = report.snapshot.id.clone();
    drop(captured);
    let restored = workspace.report_at("shop", &id).unwrap();
    assert_eq!(serde_json::to_value(&restored.performance_counters).unwrap(), expected);
    assert_eq!(state.factories.load(Ordering::SeqCst), 1, "offline replay does not open a provider");
    let shared_snapshot = culprit_core::redact::redact_snapshot(&restored.snapshot);
    let replay = culprit_checks::analyze(shared_snapshot);
    assert!(replay.performance_counters[0].fields_withheld);
    assert!(replay.performance_counters[0].entries.is_empty());
    assert_eq!(selected(&replay).source_state, Some(SourceState::Unavailable));
    assert!(selected(&replay).entries.is_empty());
}
