use super::*;
use culprit_collector::analytical::test_support::{BlockingCase, blocking_evidence, blocking_evidence_cases};
use culprit_core::SourceState;

async fn analyze(reader: &OutboxCaptureReader, id: &str) -> Result<LocalBlockingAnalysis> {
    let guard = OperationGuard::new(reader.timeout()).unwrap();
    let result = reader
        .analyze(LocalCaptureRequest {
            connection: "local-evidence",
            observation_id: id,
            analysis: LocalCaptureAnalysisKind::Blocking,
            operation: guard.handle(),
        })
        .await?;
    assert_eq!(result.kind(), LocalCaptureAnalysisKind::Blocking);
    let LocalCapturedAnalysis::Blocking(result) = result else { panic!("authenticated original and closure") };
    Ok(*result)
}

#[tokio::test]
async fn an_authenticated_pair_reuses_the_same_pure_graph_and_preserves_all_native_binding_states() {
    for (source, rows) in blocking_evidence_cases() {
        let fixture = LocalCaptureFixture::activity(source, rows).await;
        let reader = OutboxCaptureReader::delivery(
            fixture.outbox.as_ref().unwrap().delivery(),
            CompletedCaptureBudget::default(),
        )
        .unwrap();
        let analysis = analyze(&reader, &fixture.manifest.capture.observation_id).await.unwrap();
        let frame = analysis.followup().frame().unwrap();
        let rows = frame.rows().collect::<Vec<_>>();
        let expected = culprit_checks::focused_blocking::explain(frame.source(), &rows, true).unwrap();
        assert_eq!(serde_json::to_value(analysis.report()).unwrap(), serde_json::to_value(expected).unwrap());
        let report = analysis.report();
        let original = analysis.original().frame().unwrap();
        assert_eq!(report.original.capture_ref(), *analysis.original().capture());
        let NativeRecordIdentity::Activity(key) = &report.original.native_key else { panic!("original participant") };
        assert!(original.row(key).is_some());
        assert_eq!(analysis.original().connection(), analysis.followup().connection());
        assert_eq!(analysis.followup().capture().capture, fixture.manifest.capture);
        assert_eq!(report.context, frame.context());
        assert!(report.read_complete);
        assert_eq!(report.observed_rows, frame.native().observations);
        assert_eq!(report.source_state, frame.native().coverage.state);
        assert_eq!(analysis.bytes_read(), analysis.original().bytes_read() + analysis.followup().bytes_read());
        assert_eq!(analysis.followup().bytes_read(), fixture.original_bytes());
        assert_ne!(original.native().at, frame.native().at);
        if report.binding == FocusedRequestBinding::UnverifiedOriginalScope {
            assert_eq!(original.source().partition, SourcePartition::Target);
            assert!(frame.context().has_qualified_local_owner(), "a current local owner does not repair the original");
        }
        if report.source_state == SourceState::Unavailable {
            assert!(report.relationships.is_empty() && report.blocking.is_none());
            assert_eq!(report.membership, FocusedRequestMembership::Unavailable);
        }
    }
}

#[tokio::test]
async fn unavailable_owner_context_and_native_request_end_remain_explicit_after_full_local_verification() {
    for case in [
        BlockingCase::IdleOwner,
        BlockingCase::HiddenOwner,
        BlockingCase::PreparedOwner,
        BlockingCase::LookupLimited,
        BlockingCase::RequestChanged,
        BlockingCase::RequestMissing,
    ] {
        let (source, rows) = blocking_evidence(case);
        let fixture = LocalCaptureFixture::activity(source, rows).await;
        let reader = OutboxCaptureReader::delivery(
            fixture.outbox.as_ref().unwrap().delivery(),
            CompletedCaptureBudget::default(),
        )
        .unwrap();
        let analysis = analyze(&reader, &fixture.manifest.capture.observation_id).await.unwrap();
        let report = analysis.report();
        match case {
            BlockingCase::IdleOwner => {
                let chain = &report.blocking.as_ref().unwrap().chains[0];
                assert_eq!(chain.head_kind, culprit_core::blocking::HeadKind::IdleInTransaction);
                assert!(
                    chain.tree.context.as_ref().unwrap().sql.is_none(),
                    "previous SQL cannot identify the idle transaction owner"
                );
            }
            BlockingCase::HiddenOwner | BlockingCase::PreparedOwner | BlockingCase::LookupLimited => {
                assert!(report.relationships.iter().any(|edge| !edge.owner_observed));
                assert!(report.blocking.as_ref().unwrap().chains[0].tree.context.is_none());
            }
            BlockingCase::RequestChanged => {
                assert_eq!(report.request_end, BlockingRequestState::Changed)
            }
            BlockingCase::RequestMissing => {
                assert_eq!(report.request_end, BlockingRequestState::Missing)
            }
            _ => unreachable!(),
        }
    }
}

#[tokio::test]
async fn cold_reads_retain_the_exact_original_capture_and_refuse_an_ordinary_activity_answer() {
    let (source, rows) = blocking_evidence(BlockingCase::Matched);
    let mut fixture = LocalCaptureFixture::activity(source, rows).await;
    let delivery =
        OutboxCaptureReader::delivery(fixture.outbox.as_ref().unwrap().delivery(), CompletedCaptureBudget::default())
            .unwrap();
    let before = analyze(&delivery, &fixture.manifest.capture.observation_id).await.unwrap();
    let parent = before.original().capture().clone();
    let report = serde_json::to_value(before.report()).unwrap();
    drop(delivery);
    drop(fixture.outbox.take());
    let reader = OutboxCaptureReader::directory(fixture.directory.clone(), CompletedCaptureBudget::default()).unwrap();
    let after = analyze(&reader, &fixture.manifest.capture.observation_id).await.unwrap();
    assert_eq!(after.original().capture(), &parent);
    assert_eq!(serde_json::to_value(after.report()).unwrap(), report);
    assert_eq!(
        analyze(&reader, &parent.capture.observation_id).await.unwrap_err().detail,
        Some(ErrorDetail::Delivery(DeliveryProblem::UnsupportedSchema)),
        "a baseline alone is never a focused blocking closure"
    );
}
