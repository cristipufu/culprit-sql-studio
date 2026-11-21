use super::*;
use culprit_collector::analytical::test_support::hyperscale_log_component_evidence_cases;

#[test]
fn component_attachment_requires_exactly_one_explicit_original_and_a_local_outbox() {
    assert!(args(&["--hyperscale-log-components-capture", "original"]).is_err());
    assert!(
        args(&[
            "--evidence-outbox",
            "outbox",
            "--memory-clerks-capture",
            "clerks",
            "--hyperscale-log-components-capture",
            "components"
        ])
        .is_err()
    );
    let parsed = args(&[
        "--evidence-outbox",
        "outbox",
        "--hyperscale-log-components-capture",
        "original",
        "--window",
        "6h",
        "--plans",
        "0",
    ])
    .unwrap();
    assert_eq!(
        parsed.captured.original().unwrap().unwrap().analysis,
        LocalCaptureAnalysisKind::HyperscaleLogComponents
    );
    assert_eq!(parsed.captured.hyperscale_log_components_capture.as_deref(), Some("original"));
    assert!(!parsed.snapshot_options().collect_shared_resources);
    assert_eq!(parsed.snapshot_options().window_minutes, 360);
}
#[tokio::test]
async fn real_cli_component_attachment_saves_exact_originals_and_reloads_without_outbox_or_provider() {
    let (source, rows) = hyperscale_log_component_evidence_cases().remove(0);
    let mut fixture = LocalCaptureFixture::snapshot(source.clone(), rows).await;
    drop(fixture.outbox.take());
    let calls = Arc::new(AtomicUsize::new(0));
    let workspace = workspace_with_incarnation(&fixture, calls.clone(), None, Some("native-hs-logical-database"));
    let parsed = args(&[
        "--evidence-outbox",
        fixture.directory.to_str().unwrap(),
        "--hyperscale-log-components-capture",
        &fixture.manifest.capture.observation_id,
        "--plans",
        "0",
    ])
    .unwrap();
    let report = observed_scan(&workspace, &parsed).await.unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    let read = report.snapshot.operations.hyperscale_log_components.as_deref().unwrap();
    assert_eq!(read.description, source);
    assert_eq!(read.origin.capture.capture, fixture.manifest.capture);
    assert_eq!(read.captured_references().unwrap().len(), 2);
    assert!(read.read_complete);
    let view = report.hyperscale_log_components.as_ref().unwrap();
    assert_eq!(view.entries[0].current_max_log_rate_bps, "9007199254740993");
    assert_eq!(view.entries[0].last_reported_time_without_offset.as_deref(), Some("2026-11-18T12:00:00.003"));
    let path = workspace.paths().reports_dir("local-evidence").join(format!("{}.json", report.snapshot.id));
    let offline = Workspace::open_in(fixture.directory.parent().unwrap().join("offline-workspace")).with_provider(
        Engine::Mssql,
        ProviderRegistration::new(
            Arc::new(|_, _context| panic!("offline reanalysis opens no provider")),
            culprit_engine::ProviderConstructionPolicy::DeferredNativeWork,
            vec![],
        )
        .unwrap(),
    );
    let reopened = offline.analyze_file(&path).unwrap();
    assert_eq!(reopened.snapshot.operations.hyperscale_log_components.as_deref(), Some(read));
    assert_eq!(
        serde_json::to_value(reopened.hyperscale_log_components).unwrap(),
        serde_json::to_value(&report.hyperscale_log_components).unwrap()
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}
#[tokio::test]
async fn cancelled_component_read_never_dispatches_the_native_scan() {
    let (source, rows) = hyperscale_log_component_evidence_cases().remove(0);
    let fixture = LocalCaptureFixture::snapshot(source, rows).await;
    let pending = Arc::new(PendingScan::default());
    let workspace = workspace(&fixture, Arc::new(AtomicUsize::new(0)), Some(pending.clone()));
    let reader =
        OutboxCaptureReader::delivery(fixture.outbox.as_ref().unwrap().delivery(), CompletedCaptureBudget::default())
            .unwrap();
    let selection = CapturedOriginal {
        observation_id: &fixture.manifest.capture.observation_id,
        analysis: LocalCaptureAnalysisKind::HyperscaleLogComponents,
    };
    let cancellation = CollectionCancellation::default();
    cancellation.cancel();
    let error =
        observed_captured_scan(&workspace, &args(&[]).unwrap(), &reader, selection, &cancellation).await.unwrap_err();
    assert_eq!(CollectionStop::from_error(&error), CollectionStop::Cancelled);
    assert_eq!(pending.factories.load(Ordering::SeqCst), 0);
}
