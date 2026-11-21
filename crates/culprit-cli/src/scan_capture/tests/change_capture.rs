//! Actual CLI intake -> sealed local original -> normal scan -> source-free saved replay for both families.
use super::*;
use culprit_collector::analytical::test_support::{CdcCase, ChangeFeedCase, cdc_evidence, change_feed_evidence};
#[test]
fn each_native_history_requires_one_explicit_original_and_an_outbox_without_enabling_acquisition() {
    for flag in ["--cdc-capture", "--change-feed-capture"] {
        assert!(args(&[flag, "original"]).is_err());
        let parsed = args(&["--evidence-outbox", "outbox", flag, "original"]).unwrap();
        assert!(!parsed.snapshot_options().collect_shared_resources);
        let kind = parsed.captured.original().unwrap().unwrap().analysis;
        assert_eq!(
            kind,
            if flag == "--cdc-capture" { LocalCaptureAnalysisKind::Cdc } else { LocalCaptureAnalysisKind::ChangeFeed }
        );
    }
    assert!(args(&["--evidence-outbox", "outbox", "--cdc-capture", "one", "--change-feed-capture", "two"]).is_err());
}
#[tokio::test]
async fn both_cli_histories_retain_original_scope_sources_and_clocks_through_offline_replay() {
    for (flag, (source, rows)) in [
        ("--cdc-capture", cdc_evidence(CdcCase::PartialErrors)),
        ("--change-feed-capture", change_feed_evidence(ChangeFeedCase::NativeNulls)),
    ] {
        let mut fixture = LocalCaptureFixture::snapshot(source.clone(), rows).await;
        drop(fixture.outbox.take());
        let calls = Arc::new(AtomicUsize::new(0));
        let workspace = workspace(&fixture, calls.clone(), None);
        let parsed = args(&[
            "--evidence-outbox",
            fixture.directory.to_str().unwrap(),
            flag,
            &fixture.manifest.capture.observation_id,
            "--plans",
            "0",
        ])
        .unwrap();
        let report = scan(&workspace, &parsed).await.unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        let (originals, view) = if flag == "--cdc-capture" {
            (&report.snapshot.operations.cdc_history, serde_json::to_value(report.cdc.as_ref().unwrap()).unwrap())
        } else {
            (
                &report.snapshot.operations.change_feed_history,
                serde_json::to_value(report.change_feed.as_ref().unwrap()).unwrap(),
            )
        };
        let culprit_core::telemetry::RecordedOperationalRead::Captured { read } = &originals[0] else { panic!() };
        assert_eq!(read.description, source);
        assert_eq!(read.origin.capture.capture, fixture.manifest.capture);
        assert_eq!(view[0]["context"]["observed_from"], source.observation.snapshot().unwrap().observed_from);
        assert_eq!(view[0]["sources"].as_array().unwrap().len(), 3);
        let path = workspace.paths().reports_dir("local-evidence").join(format!("{}.json", report.snapshot.id));
        let offline = Workspace::open_in(fixture.directory.parent().unwrap().join("offline")).with_provider(
            Engine::Mssql,
            ProviderRegistration::new(Arc::new(|_| panic!("saved replay opens no native provider")), vec![]).unwrap(),
        );
        let saved = offline.analyze_file(&path).unwrap();
        let reopened = if flag == "--cdc-capture" {
            serde_json::to_value(saved.cdc).unwrap()
        } else {
            serde_json::to_value(saved.change_feed).unwrap()
        };
        assert_eq!(reopened, view);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        let compact = crate::output::compact_report(&report, &path);
        let key = if flag == "--cdc-capture" { "cdc" } else { "change_feed" };
        assert_eq!(
            compact[key][0]["original_read_bounds"]["from"],
            source.observation.snapshot().unwrap().observed_from
        );
        assert_eq!(compact[key][0]["sources"].as_array().unwrap().len(), 3);
        assert!(compact[key][0]["entries_presented"].as_u64().unwrap() <= 3);
    }
}

#[tokio::test]
async fn cancelled_history_intake_never_enters_the_normal_scan_factory() {
    for (kind, (source, rows)) in [
        (LocalCaptureAnalysisKind::Cdc, cdc_evidence(CdcCase::ErrorAndProgress)),
        (LocalCaptureAnalysisKind::ChangeFeed, change_feed_evidence(ChangeFeedCase::ProgressAndError)),
    ] {
        let fixture = LocalCaptureFixture::snapshot(source, rows).await;
        let calls = Arc::new(AtomicUsize::new(0));
        let workspace = workspace(&fixture, calls.clone(), None);
        let reader = OutboxCaptureReader::delivery(
            fixture.outbox.as_ref().unwrap().delivery(),
            CompletedCaptureBudget::default(),
        )
        .unwrap();
        let cancellation = CollectionCancellation::default();
        cancellation.cancel();
        let error = captured_scan(
            &workspace,
            &args(&[]).unwrap(),
            &reader,
            CapturedOriginal { observation_id: &fixture.manifest.capture.observation_id, analysis: kind },
            &cancellation,
        )
        .await
        .unwrap_err();
        assert_eq!(CollectionStop::from_error(&error), CollectionStop::Cancelled);
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        assert!(workspace.report_history("local-evidence").unwrap().is_empty());
    }
}

#[test]
fn captured_scan_concrete_controllers_and_completed_outputs_are_bounded_before_native_decoding() {
    const MAX_INLINE_BYTES: usize = 4096;
    fn storage<Args, F: Future>(_: impl FnOnce(Args) -> F) -> (usize, usize) {
        (std::mem::size_of::<F>(), std::mem::size_of::<F::Output>())
    }
    // Do not construct a client, read an Outbox or poll a future. These are the actual production bodies,
    // not only the erased wrapper's16 bytes, and include the completion shape carried by each controller.
    let bodies = [
        storage(|(workspace, args): (&'static Workspace, &'static crate::ScanArgs)| scan_body(workspace, args)),
        storage(
            |(workspace, args, reader, original, cancel): (
                &'static Workspace,
                &'static crate::ScanArgs,
                &'static dyn LocalCaptureReader,
                CapturedOriginal<'static>,
                &'static CollectionCancellation,
            )| captured_scan_body(workspace, args, reader, original, cancel),
        ),
        storage(|(cancel, work): (&'static CollectionCancellation, ScanTask<'static>)| {
            scan_until_cancelled(cancel, move || work)
        }),
        storage(|(cancel, work): (&'static CollectionCancellation, ScanTask<'static>)| {
            crate::collection_shutdown::run(cancel, work)
        }),
        storage(|(cancel, work): (&'static CollectionCancellation, ScanTask<'static>)| {
            crate::collection_shutdown::with_signal(cancel, work, std::future::pending::<Result<()>>())
        }),
    ];
    for (future, completed) in bodies {
        assert!(future <= MAX_INLINE_BYTES, "controller retains {future} inline bytes");
        assert!(completed <= MAX_INLINE_BYTES, "completed controller output transports {completed} inline bytes");
    }
    assert!(std::mem::size_of::<Result<Box<Report>>>() <= MAX_INLINE_BYTES);
}
