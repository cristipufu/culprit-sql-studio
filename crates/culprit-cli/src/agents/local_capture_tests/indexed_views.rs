use super::*;
use culprit_collector::analytical::test_support::{IndexedViewsCase, indexed_views_evidence};
#[tokio::test]
async fn original_indexed_view_action_preserves_scope_and_never_constructs_a_native_provider() {
    for case in IndexedViewsCase::ALL {
        let (source, records) = indexed_views_evidence(case);
        let mut fixture = LocalCaptureFixture::snapshot(source.clone(), records).await;
        drop(fixture.outbox.take());
        let native = Arc::new(AtomicUsize::new(0));
        let server = server(&fixture, native.clone());
        let (error, value) =
            analyze(&server, &fixture.manifest.capture.observation_id, LocalCaptureAnalysisKind::IndexedViews).await;
        assert!(!error, "{case:?}: {value}");
        let report = &value["indexed_views"];
        assert_eq!(report["historical_context"], true);
        assert_eq!(report["source"]["source"], culprit_core::source_id::INDEXES);
        assert_eq!(report["qualification"], culprit_core::t!("view.indexed_views.qualification"));
        assert_eq!(native.load(Ordering::SeqCst), 0);
        if case == IndexedViewsCase::Denied {
            assert_eq!(report["source"]["state"], "unavailable");
            assert_eq!(report["returned_entries"], 0);
        }
        if case == IndexedViewsCase::Present {
            assert_eq!(report["returned_entries"], 2);
            assert_eq!(report["native_index_rows"], 3);
        }
    }
}
