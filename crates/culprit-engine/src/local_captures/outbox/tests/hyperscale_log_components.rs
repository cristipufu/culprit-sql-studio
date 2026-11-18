use super::*;
use culprit_collector::analytical::test_support::hyperscale_log_component_evidence_cases;
use culprit_core::SourceState;

async fn analyze(fixture: &LocalCaptureFixture) -> Result<LocalHyperscaleLogComponents> {
    let reader =
        OutboxCaptureReader::delivery(fixture.outbox.as_ref().unwrap().delivery(), CompletedCaptureBudget::default())?;
    let guard = OperationGuard::new(reader.timeout())?;
    let result = reader
        .analyze(LocalCaptureRequest {
            connection: "local-evidence",
            observation_id: &fixture.manifest.capture.observation_id,
            analysis: LocalCaptureAnalysisKind::HyperscaleLogComponents,
            operation: guard.handle(),
        })
        .await?;
    let LocalCapturedAnalysis::HyperscaleLogComponents(result) = result else { panic!("requested component capture") };
    Ok(result)
}
#[tokio::test]
async fn local_component_analysis_reuses_original_counts_native_clocks_and_the_shared_interpreter() {
    for (source, rows) in hyperscale_log_component_evidence_cases() {
        let fixture = LocalCaptureFixture::snapshot(source.clone(), rows).await;
        let original = analyze(&fixture).await.unwrap();
        assert_eq!(original.connection(), "local-evidence");
        assert_eq!(original.read().description, source);
        assert!(original.read().is_valid());
        assert_eq!(original.bytes_read(), fixture.original_bytes());
        assert_eq!(original.collected_from(), fixture.manifest.started_at);
        assert_eq!(Some(original.collected_until()), fixture.manifest.finished_at.as_deref());
        let report = original.report();
        let coverage = &source.observation.snapshot().unwrap().sources[0];
        assert_eq!(report.source_state, coverage.state);
        assert_eq!(report.native_rows, coverage.records);
        assert!(report.read_complete);
        assert_eq!(report.observed_rows, coverage.records);
        if let Some(first) = report.entries.first() {
            assert!(matches!(first.current_max_log_rate_bps.as_str(), "9007199254740993" | "0"));
            assert_eq!(first.last_reported_time_without_offset.as_deref(), Some("2026-11-18T12:00:00.003"));
        }
        let recomputed = culprit_checks::hyperscale_log_components::explain_captured(original.read()).unwrap();
        assert_eq!(serde_json::to_value(report).unwrap(), serde_json::to_value(recomputed).unwrap());
    }
}
#[tokio::test]
async fn corrupt_or_unconfirmed_components_refuse_instead_of_falling_back_to_an_empty_report() {
    for failure in ["corrupt", "unconfirmed"] {
        let (source, rows) = hyperscale_log_component_evidence_cases().remove(0);
        let shutdown = if failure == "unconfirmed" { SourceShutdown::Unconfirmed } else { SourceShutdown::Quiescent };
        let fixture = LocalCaptureFixture::with_shutdown(source, rows, shutdown).await;
        if failure == "corrupt" {
            let file = fixture.capture_directory.join(&fixture.manifest.files[0].file);
            let before = std::fs::read_to_string(&file).unwrap();
            let changed = before.replace("9007199254740993", "9007199254740994");
            assert_ne!(before, changed);
            assert_eq!(before.len(), changed.len());
            std::fs::write(file, changed).unwrap();
        }
        assert!(analyze(&fixture).await.is_err(), "{failure}");
    }
}
#[tokio::test]
async fn component_analysis_cannot_borrow_the_operational_dataset_or_a_foreign_connection() {
    let (source, rows) = hyperscale_log_component_evidence_cases().remove(0);
    let fixture = LocalCaptureFixture::snapshot(source, rows).await;
    let reader =
        OutboxCaptureReader::delivery(fixture.outbox.as_ref().unwrap().delivery(), CompletedCaptureBudget::default())
            .unwrap();
    for (connection, analysis) in [
        ("local-evidence", LocalCaptureAnalysisKind::MemoryClerks),
        ("other-connection", LocalCaptureAnalysisKind::HyperscaleLogComponents),
    ] {
        let guard = OperationGuard::new(reader.timeout()).unwrap();
        assert!(
            reader
                .analyze(LocalCaptureRequest {
                    connection,
                    observation_id: &fixture.manifest.capture.observation_id,
                    analysis,
                    operation: guard.handle()
                })
                .await
                .is_err()
        );
    }
    assert_eq!(analyze(&fixture).await.unwrap().report().source_state, SourceState::Collected);
}
