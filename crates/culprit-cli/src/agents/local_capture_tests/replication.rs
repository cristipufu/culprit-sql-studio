use super::*;
use culprit_collector::analytical::test_support::{ReplicationCase, replication_evidence};
use culprit_engine::LocalCaptureAnalysisKind;
#[tokio::test]
async fn actual_local_replication_mcp_keeps_original_role_topology_and_caveats_without_source_or_native_proof() {
    for case in [
        ReplicationCase::MssqlHyperscale,
        ReplicationCase::MssqlHyperscaleNulls,
        ReplicationCase::MssqlHyperscaleUnavailable,
        ReplicationCase::MssqlHyperscalePartial,
        ReplicationCase::MssqlAzureLocalState,
        ReplicationCase::MssqlAzureLocalNulls,
        ReplicationCase::MssqlAzureLocalUnavailable,
        ReplicationCase::MssqlAzureLocalAndTopology,
    ] {
        let (source, records) = replication_evidence(case);
        let native = source.observation.snapshot().unwrap().clone();
        let mut fixture = LocalCaptureFixture::snapshot(source.clone(), records.clone()).await;
        let before = std::fs::read(fixture.capture_directory.join("manifest.json")).unwrap();
        drop(fixture.outbox.take());
        let calls = Arc::new(AtomicUsize::new(0));
        let server = server(&fixture, calls.clone());
        let (error, value) =
            analyze(&server, &fixture.manifest.capture.observation_id, LocalCaptureAnalysisKind::Replication).await;
        assert!(!error, "{value}");
        assert_eq!(value["analysis"], "replication");
        assert_eq!(value["historical_context"], true);
        let view = &value["replication"];
        assert_eq!(view["native_rows"], native.records);
        let local=records.iter().any(|row|matches!(row,TelemetryRecord::Replication(ReplicationObservation {fact:ReplicationFact::AzureReplicaState(point),..}) if point.progress.state==Some(0)));
        assert_eq!(view["state_rows"], usize::from(local));
        assert_eq!(view["entries"].as_array().unwrap().len(), records.len());
        let snapshot = ReplicationSnapshot::from_source(
            &source,
            records
                .iter()
                .map(|row| {
                    let TelemetryRecord::Replication(row) = row else { panic!() };
                    row.clone()
                })
                .collect(),
        )
        .unwrap();
        let expected = culprit_checks::replication::explain(&snapshot, true).unwrap();
        for (entry, expected) in view["entries"].as_array().unwrap().iter().zip(&expected.entries) {
            assert_eq!(entry["reference"].as_str().unwrap().len(), 64);
            assert_eq!(entry["title"], expected.title);
            assert_eq!(entry["explanation"], expected.explanation);
            assert_eq!(entry["investigation"], expected.investigation);
            assert_eq!(entry["verify"], expected.verify);
            assert_eq!(entry["conditions"], json!(expected.conditions));
            assert_eq!(entry["fields"], json!(expected.fields));
            assert_eq!(entry["groups"], json!(expected.groups));
        }
        for source in &native.sources {
            let key = if source.source == MSSQL_AZURE_REPLICA_STATE_SOURCE {
                "view.replication.mssql.azure_local.limit"
            } else {
                "view.replication.mssql.hyperscale.limit"
            };
            assert!(view["limitations"].as_array().unwrap().contains(&json!(culprit_core::i18n::t(key, &[]))));
        }
        for key in ["manifest_sha256", "source_scope_id", "payload_sha256", "qualifiers", "original-native-process"] {
            assert!(!value.to_string().contains(key), "{key}");
        }
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        assert_eq!(std::fs::read(fixture.capture_directory.join("manifest.json")).unwrap(), before);
        assert_eq!(
            analyze(&server, &fixture.manifest.capture.observation_id, LocalCaptureAnalysisKind::Replication).await.1,
            value
        );
    }
}
