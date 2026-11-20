use super::*;
use culprit_collector::analytical::test_support::{FullTextPopulationCase, full_text_population_evidence};
use culprit_engine::LocalCaptureAnalysisKind;
#[tokio::test]
async fn actual_local_mcp_full_text_population_route_keeps_the_same_original_canonical_metrics_and_gaps() {
    for case in FullTextPopulationCase::ALL {
        let (source, records) = full_text_population_evidence(case);
        let SourceObservation::FullTextPopulations { target, snapshot, .. } = &source.observation else { panic!() };
        let target = target.clone();
        let snapshot = snapshot.clone();
        let mut fixture = LocalCaptureFixture::snapshot(source.clone(), records.clone()).await;
        drop(fixture.outbox.take());
        let calls = Arc::new(AtomicUsize::new(0));
        let server = server(&fixture, calls.clone());
        let (error, value) =
            analyze(&server, &fixture.manifest.capture.observation_id, LocalCaptureAnalysisKind::FullTextPopulations)
                .await;
        assert!(!error, "{case:?}: {value}");
        assert_eq!(value["historical_context"], true);
        let report = value.get("full_text_populations").expect("named original full-text population analysis");
        assert_eq!(report["source_state"], json!(snapshot.sources[0].state));
        assert_eq!(report["native_rows"], snapshot.records);
        assert_eq!(report["observed_rows"], records.len());
        assert_eq!(
            report["selected_target"],
            json!({"database_id":target.table.database_id,"object_id":target.table.object_id,"catalog_id":target.catalog_id,"unique_index_id":target.unique_index_id,"schema":"dbo","table":"OriginalDocuments"})
        );
        let rows = records
            .iter()
            .map(|row| {
                let TelemetryRecord::Operational(row) = row else { panic!() };
                row
            })
            .collect::<Vec<_>>();
        let canonical = culprit_checks::full_text_populations::explain(
            culprit_checks::full_text_populations::FullTextPopulationEvidence {
                source: &source,
                observations: &rows,
                read_complete: true,
            },
        )
        .unwrap();
        assert_eq!(report["summary"], canonical.summary);
        assert_eq!(report["limitations"], json!(canonical.limitations));
        if let Some(entry) = canonical.entries.first() {
            assert_eq!(report["entries"][0]["metrics"], json!(entry.metrics));
        }
        for private in [
            "native-machine",
            "native-member",
            "original-process-and-db",
            "manifest_sha256",
            "source_sha256",
            "original.invalid",
        ] {
            assert!(!value.to_string().contains(private), "{private}");
        }
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }
}
#[tokio::test]
async fn wrong_family_or_missing_original_population_body_never_falls_back_to_a_live_provider() {
    let (source, records) = full_text_population_evidence(FullTextPopulationCase::Processing);
    let mut fixture = LocalCaptureFixture::snapshot(source, records).await;
    let file = fixture
        .manifest
        .files
        .iter()
        .find(|file| file.stream == TelemetryStream::OperationalSamples)
        .unwrap()
        .file
        .clone();
    drop(fixture.outbox.take());
    let calls = Arc::new(AtomicUsize::new(0));
    let server = server(&fixture, calls.clone());
    let id = &fixture.manifest.capture.observation_id;
    assert!(analyze(&server, id, LocalCaptureAnalysisKind::TableFullText).await.0);
    std::fs::remove_file(fixture.capture_directory.join(file)).unwrap();
    assert!(analyze(&server, id, LocalCaptureAnalysisKind::FullTextPopulations).await.0);
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}
