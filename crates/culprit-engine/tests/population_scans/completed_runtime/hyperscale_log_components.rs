//! Original component captures use the existing verified local reader and one normal scan owner.
use super::*;
use culprit_collector::analytical::test_support::hyperscale_log_component_evidence_cases;
use culprit_engine::{
    LocalCaptureAnalysisKind, LocalCaptureRequest, LocalCapturedAnalysis, LocalHyperscaleLogComponents,
    OutboxCaptureReader,
};

async fn capture(case: usize) -> Captured {
    let (mut source, rows) = hyperscale_log_component_evidence_cases().remove(case);
    let CollectionTarget::Database(database) = &mut source.target else { panic!() };
    database.database = snapshots().requested.server.database;
    Captured::from_source("shop", source, rows).await
}
fn input<'a>(
    captured: &'a Captured,
    delivery: &'a OutboxDelivery,
    cancellation: &'a CollectionCancellation,
) -> CompletedCaptureScanInput<'a> {
    CompletedCaptureScanInput {
        analysis: CompletedCaptureAnalysis::HyperscaleLogComponents,
        ..captured.input(delivery, cancellation)
    }
}
async fn analyze(workspace: &Workspace, captured: &Captured) -> LocalHyperscaleLogComponents {
    let reader = OutboxCaptureReader::delivery(captured.outbox.delivery(), CompletedCaptureBudget::default()).unwrap();
    let guard = culprit_core::limits::OperationGuard::new(std::time::Duration::from_secs(5)).unwrap();
    let result = workspace
        .analyze_local_capture(
            &reader,
            LocalCaptureRequest {
                connection: "shop",
                observation_id: &captured.manifest.capture.observation_id,
                analysis: LocalCaptureAnalysisKind::HyperscaleLogComponents,
                operation: guard.handle(),
            },
        )
        .await
        .unwrap();
    let LocalCapturedAnalysis::HyperscaleLogComponents(result) = result else { panic!("component analysis") };
    result
}
fn assert_original(report: &culprit_core::Report, source: &CollectionDescription) {
    let read = report.snapshot.operations.hyperscale_log_components.as_deref().unwrap();
    assert!(read.is_valid());
    assert!(read.read_complete);
    assert_eq!(&read.description, source);
    assert_eq!(read.context().unwrap(), NativeReadContext::from_snapshot(source).unwrap());
    let native = &source.observation.snapshot().unwrap().sources[0];
    let view = report.hyperscale_log_components.as_deref().unwrap();
    assert_eq!(view.native_source, *native);
    assert_eq!(view.native_rows, native.records);
    assert_eq!(view.observed_rows, native.records);
    assert_eq!(view.source_state, native.state);
    assert_eq!(view.context.observed_until, "2026-11-18T12:00:00.000000009Z");
    assert_ne!(view.context.observed_until, report.snapshot.taken_at);
    if let Some(point) = view.entries.first() {
        assert_eq!(point.current_max_log_rate_bps, "9007199254740993");
        assert_eq!(point.catchup_rate_bps, "0");
        assert_eq!(point.catchup_distance_bytes, "9223372036854775807");
        assert_eq!(point.last_reported_time_without_offset.as_deref(), Some("2026-11-18T12:00:00.003"));
    }
    assert_eq!(
        report.evidence.as_ref().unwrap().collected.as_ref().unwrap().operations.hyperscale_log_components.as_deref(),
        Some(read)
    );
}
#[tokio::test]
async fn one_completed_component_capture_attaches_once_after_shutdown_and_replays_with_original_receipts() {
    for case in [0, 2, 3, 4] {
        let captured = capture(case).await;
        let source = captured.source().clone();
        let (_home, workspace, state) = workspace(captured.batch(), SourceShutdown::Quiescent);
        let reports = crate::observed::run(&workspace, culprit_core::ProviderReadKind::Scan, async |operation| {
            workspace
                .scan_populations_with_completed_capture(
                    "shop",
                    &options(),
                    input(&captured, &captured.outbox.delivery(), &CollectionCancellation::default()),
                    operation,
                )
                .await
        })
        .await
        .unwrap();
        assert_original(&reports.requested, &source);
        assert_eq!(state.active.load(Ordering::SeqCst), 0);
        assert_eq!(state.acquisitions.load(Ordering::SeqCst), 1);
        assert!(reports.populations.iter().all(|report| report.hyperscale_log_components.is_none()
            && report.snapshot.operations.hyperscale_log_components.is_none()));
        let read = reports.requested.snapshot.operations.hyperscale_log_components.as_deref().unwrap();
        let references = read.captured_references().unwrap();
        for (ordinal, reference) in references.iter().enumerate() {
            assert_eq!(reference.row_ordinal, ordinal as u32);
            let export =
                ExportedRecord::new(&source, &captured.manifest.capture, captured.rows[ordinal].clone()).unwrap();
            let expected = DiagnosticEvidenceRef::from_export(
                &source,
                captured.attempt.manifest_sha256.as_deref().unwrap(),
                &export,
                ordinal as u32,
            )
            .unwrap();
            assert_eq!(reference, &expected);
        }
        let id = reports.requested.snapshot.id.clone();
        let original = read.clone();
        drop(captured);
        let reopened = workspace.report_at("shop", &id).unwrap();
        assert_original(&reopened, &source);
        assert_eq!(reopened.snapshot.operations.hyperscale_log_components.as_deref(), Some(&original));
        assert_eq!(state.factories.load(Ordering::SeqCst), 1, "replay constructs no source");
    }
}
#[tokio::test]
async fn opaque_component_result_allows_a_known_native_alias_but_not_a_foreign_connection() {
    let captured = capture(0).await;
    let mut batch = captured.batch();
    // All snapshots in one native acquisition describe its current route; the earlier capture stays unchanged.
    for snapshot in std::iter::once(&mut batch.requested).chain(batch.populations.iter_mut()) {
        let database = snapshot.identity.as_mut().unwrap();
        database.database = "renamed".into();
        database.location = DatabaseLocation::Server { host: "alias.invalid".into(), port: 1444, instance: None };
        snapshot.server.database = "renamed".into();
    }
    batch.validate(&options()).unwrap();
    let (_home, workspace, state) = workspace(batch, SourceShutdown::Quiescent);
    let original = analyze(&workspace, &captured).await;
    let report = crate::observed::run(&workspace, culprit_core::ProviderReadKind::Scan, async |operation| {
        workspace.scan_with_captured_hyperscale_log_components("shop", &options(), None, original, operation).await
    })
    .await
    .unwrap();
    assert_original(&report, captured.source());
    let original = analyze(&workspace, &captured).await;
    assert!(
        crate::observed::run(&workspace, culprit_core::ProviderReadKind::Scan, async |operation| workspace
            .scan_with_captured_hyperscale_log_components("another-connection", &options(), None, original, operation)
            .await)
        .await
        .is_err()
    );
    assert_eq!(state.factories.load(Ordering::SeqCst), 1);
}
#[tokio::test]
async fn unproven_or_contradictory_current_database_cannot_receive_historical_component_facts() {
    for change in ["unknown", "foreign", "missing", "engine", "identity-label"] {
        let captured = capture(0).await;
        let mut batch = captured.batch();
        match change {
            "unknown" => batch.requested.identity.as_mut().unwrap().incarnation = None,
            "foreign" => batch.requested.identity.as_mut().unwrap().incarnation = Some("replacement-database".into()),
            "missing" => batch.requested.identity = None,
            "engine" => batch.requested.identity.as_mut().unwrap().engine = Engine::Postgres,
            "identity-label" => batch.requested.identity.as_mut().unwrap().database = "wrong-database".into(),
            _ => unreachable!(),
        }
        let (_home, workspace, state) = workspace(batch, SourceShutdown::Quiescent);
        assert!(
            crate::observed::run(&workspace, culprit_core::ProviderReadKind::Scan, async |operation| workspace
                .scan_with_completed_capture(
                    "shop",
                    &options(),
                    input(&captured, &captured.outbox.delivery(), &CollectionCancellation::default()),
                    operation
                )
                .await)
            .await
            .is_err(),
            "{change}"
        );
        assert_eq!(state.active.load(Ordering::SeqCst), 0);
        assert!(state.analyzed.lock().unwrap().is_empty());
        assert!(workspace.report_history("shop").unwrap().is_empty());
    }
}
#[tokio::test]
async fn an_existing_requested_component_population_is_never_overwritten() {
    let captured = capture(0).await;
    let mut batch = captured.batch();
    let read = captured
        .outbox
        .delivery()
        .completed_snapshot(
            &captured.attempt,
            SnapshotDataset::HyperscaleLogComponents,
            CompletedCaptureBudget::default(),
            &CollectionCancellation::default(),
        )
        .await
        .unwrap()
        .into_operational_read()
        .unwrap();
    batch.requested.operations.hyperscale_log_components = Some(Box::new(read));
    let (_home, workspace, state) = workspace(batch, SourceShutdown::Quiescent);
    let error = crate::observed::run(&workspace, culprit_core::ProviderReadKind::Scan, async |operation| {
        workspace
            .scan_with_completed_capture(
                "shop",
                &options(),
                input(&captured, &captured.outbox.delivery(), &CollectionCancellation::default()),
                operation,
            )
            .await
    })
    .await
    .unwrap_err();
    assert_eq!(error.detail, Some(culprit_core::ErrorDetail::Delivery(DeliveryProblem::Conflict)));
    assert_eq!(state.active.load(Ordering::SeqCst), 0);
    assert!(state.analyzed.lock().unwrap().is_empty());
}
#[tokio::test]
async fn component_capture_tamper_connection_cancel_and_local_bounds_refuse_before_native_work() {
    for failure in ["body", "manifest", "cancel", "bytes", "connection"] {
        let captured = capture(0).await;
        let (_home, workspace, state) = workspace(captured.batch(), SourceShutdown::Quiescent);
        let cancellation = CollectionCancellation::default();
        let delivery = captured.outbox.delivery();
        let mut request = input(&captured, &delivery, &cancellation);
        let mut attempt = captured.attempt.clone();
        match failure {
            "body" => std::fs::write(captured.body(), b"{}\n").unwrap(),
            "manifest" => {
                attempt.manifest_sha256 = Some(digest(b"foreign"));
                request.attempt = &attempt;
            }
            "cancel" => cancellation.cancel(),
            "bytes" => request.budget.max_bytes = 1,
            "connection" => {}
            _ => unreachable!(),
        }
        let connection = if failure == "connection" { "another-saved-connection" } else { "shop" };
        assert!(
            crate::observed::run(&workspace, culprit_core::ProviderReadKind::Scan, async |operation| workspace
                .scan_with_completed_capture(connection, &options(), request, operation)
                .await)
            .await
            .is_err(),
            "{failure}"
        );
        assert_eq!(state.factories.load(Ordering::SeqCst), 0);
        assert!(workspace.report_history("shop").unwrap().is_empty());
    }
}
#[tokio::test]
async fn unconfirmed_native_scan_cannot_publish_the_independent_component_original() {
    let captured = capture(0).await;
    let (_home, workspace, state) = workspace(captured.batch(), SourceShutdown::Unconfirmed);
    let error = crate::observed::run(&workspace, culprit_core::ProviderReadKind::Scan, async |operation| {
        workspace
            .scan_with_completed_capture(
                "shop",
                &options(),
                input(&captured, &captured.outbox.delivery(), &CollectionCancellation::default()),
                operation,
            )
            .await
    })
    .await
    .unwrap_err();
    assert_eq!(CollectionStop::from_error(&error), CollectionStop::CancellationUnconfirmed);
    assert_eq!(state.active.load(Ordering::SeqCst), 0);
    assert!(state.analyzed.lock().unwrap().is_empty());
    assert!(workspace.report_history("shop").unwrap().is_empty());
}
