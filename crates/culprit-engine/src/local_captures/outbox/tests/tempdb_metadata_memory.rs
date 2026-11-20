use super::*;
use culprit_collector::analytical::test_support::{TempdbMetadataMemoryCase, tempdb_metadata_memory_evidence};
use culprit_core::SourceState;

async fn analyze(fixture: &LocalCaptureFixture, budget: CompletedCaptureBudget) -> Result<LocalSnapshotAnalysis> {
    let reader = OutboxCaptureReader::directory(fixture.directory.clone(), budget)?;
    let guard = OperationGuard::new(reader.timeout())?;
    let result = reader
        .analyze(LocalCaptureRequest {
            connection: "local-evidence",
            observation_id: &fixture.manifest.capture.observation_id,
            analysis: LocalCaptureAnalysisKind::TempdbMetadataMemory,
            operation: guard.handle(),
        })
        .await?;
    assert_eq!(result.kind(), LocalCaptureAnalysisKind::TempdbMetadataMemory);
    let LocalCapturedAnalysis::Snapshot(result) = result else { panic!("the existing generic snapshot owner") };
    Ok(result)
}

#[tokio::test]
async fn metadata_action_retains_one_original_owner_and_canonical_feature_usage_and_partial_states() {
    for case in TempdbMetadataMemoryCase::ALL {
        let (source, records) = tempdb_metadata_memory_evidence(case);
        let mut fixture = LocalCaptureFixture::snapshot(source.clone(), records.clone()).await;
        drop(fixture.outbox.take());
        let analysis = analyze(&fixture, CompletedCaptureBudget::default()).await.unwrap();
        let original = analysis.original();
        assert_eq!(original.read().description, source);
        assert_eq!(original.connection(), "local-evidence");
        assert_eq!(original.bytes_read(), fixture.original_bytes());
        assert_eq!(original.read().origin.capture.capture, fixture.manifest.capture);
        assert!(original.read().read_complete && original.read().is_valid());
        assert_eq!(original.read().observations.len(), records.len());
        assert_eq!(original.read().origin.positions.len(), records.len());
        assert_eq!(fixture.manifest.source_queries, 0, "this is retained evidence, not native or cost qualification");
        let LocalSnapshotReport::TempdbMetadataMemory(report) = analysis.report() else { panic!() };
        let expected_rows = records
            .iter()
            .filter_map(|record| match record {
                TelemetryRecord::Operational(row) if row.source == culprit_core::source_id::TEMPDB_METADATA_MEMORY => {
                    Some(row)
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        let expected = culprit_checks::tempdb_metadata_memory::explain(
            culprit_checks::tempdb_metadata_memory::TempdbMetadataMemoryEvidence {
                context: &NativeReadContext::from_snapshot(&source).unwrap(),
                coverage: &source.observation.snapshot().unwrap().sources[0],
                observations: &expected_rows,
                read_complete: true,
            },
        )
        .unwrap();
        assert_eq!(report.as_ref(), &expected);
        assert_eq!(original.read().source_coverage("independent-native-counters").unwrap().records, 1);
        if matches!(case, TempdbMetadataMemoryCase::Reserved | TempdbMetadataMemoryCase::NativePartial) {
            assert_eq!(report.entries[0].allocated_bytes.as_deref(), Some("9007199254740993"));
            assert_eq!(report.entries[0].used_bytes.as_deref(), Some("9007199254740992"));
        }
        if matches!(case, TempdbMetadataMemoryCase::Unknown | TempdbMetadataMemoryCase::NativePartial) {
            assert_eq!(report.source_state, SourceState::Partial);
        }
    }
}

#[tokio::test]
async fn metadata_action_uses_the_same_absolute_local_read_budget_and_cannot_borrow_an_unrelated_operational_source() {
    let (source, rows) = tempdb_metadata_memory_evidence(TempdbMetadataMemoryCase::Reserved);
    let mut fixture = LocalCaptureFixture::snapshot(source, rows).await;
    drop(fixture.outbox.take());
    for budget in [
        CompletedCaptureBudget { max_rows: 1, ..Default::default() },
        CompletedCaptureBudget { max_bytes: fixture.original_bytes() - 1, ..Default::default() },
    ] {
        assert_eq!(
            analyze(&fixture, budget).await.unwrap_err().detail,
            Some(ErrorDetail::Delivery(DeliveryProblem::Capacity))
        );
    }
    assert_eq!(
        analyze(&fixture, CompletedCaptureBudget { max_bytes: fixture.original_bytes(), ..Default::default() })
            .await
            .unwrap()
            .original()
            .bytes_read(),
        fixture.original_bytes()
    );
    let (source, rows) = culprit_collector::analytical::test_support::storage_evidence(
        culprit_collector::analytical::test_support::StorageCase::Exact,
    );
    let mut unrelated = LocalCaptureFixture::snapshot(source, rows).await;
    drop(unrelated.outbox.take());
    assert_eq!(
        analyze(&unrelated, CompletedCaptureBudget::default()).await.unwrap_err().detail,
        Some(ErrorDetail::Delivery(DeliveryProblem::UnsupportedSchema))
    );
}
