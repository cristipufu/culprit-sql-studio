use super::*;
use culprit_collector::analytical::test_support::{TableFullTextCase, table_full_text_evidence};

#[tokio::test]
async fn table_catalog_action_keeps_the_exact_original_owner_counts_and_local_read_budget() {
    for case in TableFullTextCase::ALL {
        let (source, records) = table_full_text_evidence(case);
        let mut fixture = LocalCaptureFixture::snapshot(source.clone(), records.clone()).await;
        drop(fixture.outbox.take());
        let reader =
            OutboxCaptureReader::directory(fixture.directory.clone(), CompletedCaptureBudget::default()).unwrap();
        let guard = OperationGuard::new(reader.timeout()).unwrap();
        let result = reader
            .analyze(LocalCaptureRequest {
                connection: "local-evidence",
                observation_id: &fixture.manifest.capture.observation_id,
                analysis: LocalCaptureAnalysisKind::TableFullText,
                operation: guard.handle(),
            })
            .await
            .unwrap();
        assert_eq!(result.kind(), LocalCaptureAnalysisKind::TableFullText);
        let LocalCapturedAnalysis::Snapshot(result) = result else { panic!() };
        assert_eq!(result.original().read().description, source);
        assert_eq!(result.original().bytes_read(), fixture.original_bytes());
        assert!(result.original().read().is_valid());
        assert_eq!(result.original().read().observations.len(), records.len());
        let LocalSnapshotReport::TableFullText(report) = result.report() else { panic!() };
        assert_eq!(report.native_rows, Some(records.len() as u32));
        assert_eq!(fixture.manifest.source_queries, 0);
    }
}

#[tokio::test]
async fn table_action_cannot_read_past_its_original_byte_budget_or_borrow_an_operational_dataset() {
    let (source, records) = table_full_text_evidence(TableFullTextCase::Enabled);
    let mut fixture = LocalCaptureFixture::snapshot(source, records).await;
    drop(fixture.outbox.take());
    let reader = OutboxCaptureReader::directory(
        fixture.directory.clone(),
        CompletedCaptureBudget { max_bytes: fixture.original_bytes() - 1, ..Default::default() },
    )
    .unwrap();
    let guard = OperationGuard::new(reader.timeout()).unwrap();
    assert!(
        reader
            .analyze(LocalCaptureRequest {
                connection: "local-evidence",
                observation_id: &fixture.manifest.capture.observation_id,
                analysis: LocalCaptureAnalysisKind::TableFullText,
                operation: guard.handle()
            })
            .await
            .is_err()
    );
    let reader = OutboxCaptureReader::directory(fixture.directory.clone(), CompletedCaptureBudget::default()).unwrap();
    let guard = OperationGuard::new(reader.timeout()).unwrap();
    assert!(
        reader
            .analyze(LocalCaptureRequest {
                connection: "local-evidence",
                observation_id: &fixture.manifest.capture.observation_id,
                analysis: LocalCaptureAnalysisKind::DatabaseProperties,
                operation: guard.handle()
            })
            .await
            .is_err()
    );
}
