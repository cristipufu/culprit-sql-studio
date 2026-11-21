use super::*;
use culprit_core::{Engine, ProviderReadKind, ProviderReadObservation, telemetry::*};
fn summary(separate: bool, complete: bool) -> ProviderAcquisitionSummary {
    let owner =
        ProviderReadObservation::begin("0123456789abcdef0123456789abcdef".into(), ProviderReadKind::Scan).unwrap();
    let operation = owner.operation().unwrap();
    operation.bind_route(SourceAdmissionRoute::SavedConnection { name: "pg-demo".into() });
    operation.bind_engine(Engine::Postgres);
    let context = operation.context();
    if complete {
        context.complete_reader_coverage();
    }
    let reader = context.reader(Engine::Postgres);
    let metrics = ReadMetricsRecorder::new(Engine::Postgres);
    let id = metrics.begin("SELECT 'private-source-canary'");
    metrics.finish(
        id.as_deref(),
        std::time::Duration::from_millis(2),
        &ReadProgress::default(),
        &ReadTaskOutcome::default(),
    );
    reader.checkpoint(3, metrics.snapshot());
    if separate {
        reader.setup_checkpoint(1, ReadMetricsRecorder::new(Engine::Postgres).snapshot());
    } else {
        reader.setup_included();
    }
    reader.finish(SourceShutdown::Quiescent);
    operation.source_finished(&Ok::<(), culprit_core::Error>(()), ProviderReadDisposition::Quiescent, Ok(())).unwrap()
}
#[test]
fn total_counts_setup_once_and_withholds_the_total_for_partial_reader_coverage() {
    for (separate, complete, expected) in [(false, true, Some("3")), (true, true, Some("4")), (false, false, None)] {
        let raw = summary(separate, complete);
        let ProviderAcquisitionReport::Available { total_admissions, evidence, .. } = explain(&raw) else {
            panic!("valid current observation")
        };
        assert_eq!(total_admissions.as_deref(), expected);
        assert_eq!(*evidence, raw);
    }
}
#[test]
fn current_work_uses_the_shared_leaf_without_reconstructing_a_capture_or_native_scope() {
    let raw = summary(false, true);
    let ProviderAcquisitionReport::Available { readers, evidence, limitations, .. } = explain(&raw) else {
        panic!("available")
    };
    assert_eq!(readers[0].work.template_order, [0]);
    assert_eq!(evidence.readers[0].work.as_ref().unwrap().templates[0].completed_tasks, 1);
    assert!(limitations.contains(&t!("view.provider_acquisition.limit.client")));
    let value = serde_json::to_value(explain(&raw)).unwrap();
    let record = value["evidence"].as_object().unwrap();
    for key in ["capture", "manifest_sha256", "source", "native_scope", "database_identity"] {
        assert!(!record.contains_key(key));
    }
    assert!(!value.to_string().contains("private-source-canary"));
}
#[test]
fn malformed_or_edited_reader_metadata_is_unavailable_instead_of_a_healthy_empty_observation() {
    let mut raw = summary(false, true);
    raw.readers[0].reader_id = 0;
    assert!(matches!(explain(&raw), ProviderAcquisitionReport::Unavailable { .. }));
    let mut raw = summary(false, true);
    raw.readers[0].work.as_mut().unwrap().templates[0].statement =
        DefinitionPayload::sql("SELECT private_edit", Engine::Postgres);
    assert!(!raw.is_valid());
    assert!(matches!(explain(&raw), ProviderAcquisitionReport::Unavailable { .. }));
}
#[test]
fn saved_current_observation_rereads_without_credentials_and_sharing_withdraws_it_indivisibly() {
    let raw = summary(false, true);
    let mut snapshot: culprit_core::Snapshot =
        serde_json::from_str(include_str!("../../../../../fixtures/postgres-demo.json")).unwrap();
    snapshot.operations.provider_acquisition =
        Some(Box::new(RecordedProviderAcquisition::Available { summary: Box::new(raw.clone()) }));
    let decoded: culprit_core::Snapshot = serde_json::from_slice(&serde_json::to_vec(&snapshot).unwrap()).unwrap();
    let report = explain_snapshot(&decoded).unwrap();
    assert!(matches!(*report, ProviderAcquisitionReport::Available { .. }));
    let shared = culprit_core::redact::redact_snapshot(&decoded);
    assert!(matches!(
        shared.operations.provider_acquisition.as_deref(),
        Some(RecordedProviderAcquisition::Unavailable { .. })
    ));
    assert!(matches!(*explain_snapshot(&shared).unwrap(), ProviderAcquisitionReport::Unavailable { .. }));
    assert!(!serde_json::to_string(&shared).unwrap().contains(&raw.observation_id));
    assert!(matches!(
        snapshot.operations.provider_acquisition.as_deref(),
        Some(RecordedProviderAcquisition::Available { .. })
    ));
}

#[test]
fn saved_current_acquisition_rejects_a_foreign_logical_route_kind_or_provider_without_native_rebinding() {
    let mut snapshot: culprit_core::Snapshot =
        serde_json::from_str(include_str!("../../../../../fixtures/postgres-demo.json")).unwrap();
    let raw = summary(false, true);
    for change in 0..3 {
        let mut edited = raw.clone();
        match change {
            0 => edited.route = Some(SourceAdmissionRoute::SavedConnection { name: "another-route".into() }),
            1 => edited.kind = ProviderReadKind::Probe,
            _ => edited.engine = Some(Engine::Mssql),
        };
        assert!(edited.is_valid());
        snapshot.operations.provider_acquisition =
            Some(Box::new(RecordedProviderAcquisition::Available { summary: Box::new(edited) }));
        assert!(matches!(*explain_snapshot(&snapshot).unwrap(), ProviderAcquisitionReport::Unavailable { .. }));
    }
}
