use super::*;
use culprit_collector::analytical::test_support::{ReplicationCase, replication_evidence};
async fn analyze(reader: &OutboxCaptureReader, connection: &str, id: &str) -> Result<Box<LocalReplication>> {
    let operation = OperationGuard::new(reader.timeout()).unwrap();
    let result = reader
        .analyze(LocalCaptureRequest {
            connection,
            observation_id: id,
            analysis: LocalCaptureAnalysisKind::Replication,
            operation: operation.handle(),
        })
        .await?;
    let LocalCapturedAnalysis::Replication(result) = result else { panic!("native replication family") };
    Ok(result)
}
#[tokio::test]
async fn original_replication_family_and_all_hyperscale_roles_replay_locally_without_operational_repacking() {
    for case in ReplicationCase::ALL.iter().copied() {
        let (source, rows) = replication_evidence(case);
        let fixture = LocalCaptureFixture::snapshot(source.clone(), rows.clone()).await;
        let reader = OutboxCaptureReader::delivery(
            fixture.outbox.as_ref().unwrap().delivery(),
            CompletedCaptureBudget::default(),
        )
        .unwrap();
        let result = analyze(&reader, "local-evidence", &fixture.manifest.capture.observation_id).await.unwrap();
        let expected = crate::collected_resources::explain_replication_read(
            &source,
            rows.iter()
                .map(|row| {
                    let TelemetryRecord::Replication(row) = row else { panic!() };
                    row.clone()
                })
                .collect(),
            true,
        )
        .unwrap();
        assert_eq!(serde_json::to_value(result.report()).unwrap(), serde_json::to_value(expected).unwrap());
        assert_eq!(result.source(), &source);
        assert_eq!(result.bytes_read(), fixture.original_bytes());
        assert_eq!(result.collected_from(), fixture.manifest.started_at);
        assert_eq!(Some(result.collected_until()), fixture.manifest.finished_at.as_deref());
        for entry in &result.report().entries {
            let reference = result.reference(&entry.reference).unwrap();
            assert_eq!(reference.stream, TelemetryStream::ReplicationSamples);
            assert_eq!(reference.capture, fixture.manifest.capture);
            assert!(rows.iter().any(|row| row.native_identity() == reference.native_key));
        }
        assert_eq!(fixture.manifest.source_queries, 0, "not native cost evidence");
    }
}
#[tokio::test]
async fn local_replication_refuses_wrong_saved_connection_exhausted_budget_and_cancelled_lookup() {
    let (source, rows) = replication_evidence(ReplicationCase::MssqlHyperscale);
    let fixture = LocalCaptureFixture::snapshot(source, rows).await;
    let reader =
        OutboxCaptureReader::delivery(fixture.outbox.as_ref().unwrap().delivery(), CompletedCaptureBudget::default())
            .unwrap();
    let id = &fixture.manifest.capture.observation_id;
    assert!(analyze(&reader, "another-connection", id).await.is_err());
    assert!(analyze(&reader, "local-evidence", id).await.is_ok());
    let mut budget = CompletedCaptureBudget::default();
    budget.max_bytes = 1;
    let bounded = OutboxCaptureReader::delivery(fixture.outbox.as_ref().unwrap().delivery(), budget).unwrap();
    assert!(analyze(&bounded, "local-evidence", id).await.is_err());
    let operation = OperationGuard::new(reader.timeout()).unwrap();
    let handle = operation.handle();
    drop(operation);
    assert!(
        reader
            .analyze(LocalCaptureRequest {
                connection: "local-evidence",
                observation_id: id,
                analysis: LocalCaptureAnalysisKind::Replication,
                operation: handle
            })
            .await
            .is_err()
    );
}

#[tokio::test]
async fn cold_original_topology_replay_refuses_an_edited_native_replica_even_when_the_report_was_previously_valid() {
    let (source, records) = replication_evidence(ReplicationCase::MssqlHyperscale);
    let mut fixture = LocalCaptureFixture::snapshot(source, records).await;
    drop(fixture.outbox.take());
    let reader = OutboxCaptureReader::directory(fixture.directory.clone(), CompletedCaptureBudget::default()).unwrap();
    let id = &fixture.manifest.capture.observation_id;
    assert!(analyze(&reader, "local-evidence", id).await.is_ok());
    let path = fixture.capture_directory.join(&fixture.manifest.files[0].file);
    let original = std::fs::read_to_string(&path).unwrap();
    let changed = original.replace("orders-logical-server", "different-logical-server");
    assert_ne!(changed, original);
    std::fs::write(&path, changed).unwrap();
    assert!(analyze(&reader, "local-evidence", id).await.is_err());
}
