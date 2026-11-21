use super::*;
use culprit_core::{Engine, ProviderReadDisposition, ProviderReadKind, ProviderReadObservation, telemetry::*};
fn report(readers: usize, complete: bool) -> ProviderAcquisitionReport {
    let owner =
        ProviderReadObservation::begin("0123456789abcdef0123456789abcdef".into(), ProviderReadKind::Scan).unwrap();
    let operation = owner.operation().unwrap();
    let context = operation.context();
    if complete {
        context.complete_reader_coverage();
    }
    for _ in 0..readers {
        let reader = context.reader(Engine::Postgres);
        reader.setup_included();
        let metrics = ReadMetricsRecorder::new(Engine::Postgres);
        let id = metrics.begin("SELECT 'private-source-sql-9264'");
        metrics.finish(
            id.as_deref(),
            std::time::Duration::from_millis(1),
            &ReadProgress::default(),
            &ReadTaskOutcome::default(),
        );
        reader.checkpoint(1, metrics.snapshot());
        reader.finish(SourceShutdown::Quiescent);
    }
    let summary = operation
        .source_finished(&Ok::<(), culprit_core::Error>(()), ProviderReadDisposition::Quiescent, Ok(()))
        .unwrap();
    culprit_engine::explain_provider_acquisition(&summary)
}
#[test]
fn current_text_retains_original_accounting_and_bounds_without_capture_or_sql_payloads() {
    let report = report(1, true);
    let text = provider_acquisition(&report);
    let ProviderAcquisitionReport::Available { evidence, readers, total_admissions, .. } = &report else {
        panic!("available")
    };
    assert_eq!(total_admissions.as_deref(), Some("1"));
    assert!(text.contains(&evidence.client_started_at));
    assert!(text.contains(&evidence.client_finished_at));
    assert!(text.contains(&readers[0].work.summary));
    assert!(text.contains(&evidence.readers[0].work.as_ref().unwrap().templates[0].template_id));
    let public = serde_json::to_value(&report).unwrap();
    assert!(public["evidence"].get("capture").is_none());
    assert!(public["evidence"].get("source").is_none());
    assert!(public["evidence"].get("manifest_sha256").is_none());
    assert!(!text.contains("private-source-sql-9264"));
    assert!(!public.to_string().contains("private-source-sql-9264"));
}
#[test]
fn partial_accounting_withholds_total_and_the_brief_does_not_dump_templates() {
    let report = report(1, false);
    let ProviderAcquisitionReport::Available { evidence, total_admissions, .. } = &report else { panic!("available") };
    assert!(total_admissions.is_none());
    let text = provider_acquisition(&report);
    assert!(text.contains(&t!("view.provider_acquisition.coverage.partial")));
    assert!(!text.contains(&t!("view.provider_acquisition.total", count = "0")));
    let brief = provider_acquisition_brief(&report);
    assert!(!brief.contains(&evidence.readers[0].work.as_ref().unwrap().templates[0].template_id));
    assert!(brief.contains(&t!("view.provider_acquisition.limit.client")));
}
#[test]
fn presentation_cap_and_withdrawal_do_not_replace_or_invent_original_accounting() {
    let report = report(READER_LIMIT + 1, true);
    let text = provider_acquisition(&report);
    assert!(text.contains(&t!("view.provider_acquisition.presentation", shown = "5", recorded = "6")));
    let ProviderAcquisitionReport::Available { readers, .. } = &report else { panic!("available") };
    assert_eq!(readers.len(), 6);
    let withdrawn = ProviderAcquisitionReport::Unavailable { reason: t!("view.provider_acquisition.sharing_withheld") };
    assert_eq!(provider_acquisition(&withdrawn), t!("view.provider_acquisition.sharing_withheld"));
    assert_eq!(provider_acquisition_brief(&withdrawn), t!("view.provider_acquisition.sharing_withheld"));
}
