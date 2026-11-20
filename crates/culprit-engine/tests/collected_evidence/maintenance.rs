use super::fixture::Fixture;
use culprit_collector::analytical::{
    evidence::{EvidenceFamily, EvidenceGapReason, RecoveryPayloadBudget},
    test_support::{activity_evidence_cases, maintenance_activity_evidence},
};
use culprit_core::{Engine, MaintenanceDirection, SourceState, telemetry::*};
use culprit_engine::collected_resources;

fn recorded() -> (CollectionDescription, Vec<TelemetryRecord>) {
    let (mut source, mut records) = activity_evidence_cases().remove(0);
    records.truncate(2);
    let TelemetryRecord::Activity(maintenance) = &mut records[0] else { unreachable!() };
    maintenance.statement = DefinitionPayload::sql("ALTER INDEX ix ON dbo.orders REBUILD", Engine::Mssql);
    maintenance.wait = None;
    maintenance.waiting_on_lock = Some(false);
    let owner = maintenance.key.session.clone();
    let TelemetryRecord::Activity(waiter) = &mut records[1] else { unreachable!() };
    waiter.statement = DefinitionPayload::sql("SELECT payload FROM dbo.orders", Engine::Mssql);
    waiter.statement_identity = Some(NativeIdentity::new("waiter-current-statement"));
    waiter.blocking_status = ActivityBlockingStatus::Known;
    waiter.waiting_on_lock = Some(true);
    waiter.wait = Some(ActivityWait {
        class: None,
        native_type: Some("LCK_M_X".into()),
        elapsed_ms: Some("40000".into()),
        owners: vec![ActivityWaitOwner::Session { id: owner.id, generation: Some(owner.generation) }],
        identified_resources: vec![],
        resources: None,
    });
    let SourceObservation::Activity(native) = &mut source.observation else { unreachable!() };
    native.native_rows = 2;
    native.observations = 2;
    native.coverage = StateSourceCoverage::collected(source.source.clone(), 2);
    (source, records)
}

#[tokio::test]
async fn archived_maintenance_uses_exact_original_activity_receipts_without_payload_or_native_rereads() {
    let (source, records) = recorded();
    let fixture = Fixture::new(source.clone(), records.clone()).await;
    let before = fixture.recovery_object_reads();
    let capture = fixture.read_families(&[EvidenceFamily::ActivityObservations]).await;
    let analysis = collected_resources::maintenance(&capture).unwrap();
    assert_eq!(analysis.report.entries.len(), 1);
    assert_eq!(analysis.report.entries[0].direction, MaintenanceDirection::BlockingSession);
    let evidence = analysis.report.entries[0].reference.evidence.as_ref().unwrap();
    assert_eq!(evidence.capture, *capture.capture());
    assert_eq!(evidence.manifest_sha256, capture.capture_manifest_sha256());
    assert_eq!(evidence.row_ordinal, 0);
    assert_eq!(analysis.report.entries[0].affected[0].evidence.as_ref().unwrap().row_ordinal, 1);
    assert_eq!(analysis.context.source, source);
    assert_eq!(analysis.context.records.len(), records.len());
    for (original, retained) in records.iter().zip(&analysis.context.records) {
        assert_eq!(retained.row, ExportedRecord::new(capture.source(), capture.capture(), original.clone()).unwrap());
    }
    assert_eq!(fixture.recovery_object_reads(), before);
    assert_eq!(analysis.context.metrics.recovery_objects, 0);
    let derived = serde_json::to_value(&analysis.report).unwrap();
    assert!(!derived.to_string().contains("ALTER INDEX"));
    assert!(derived.get("snapshot").is_none() && derived.get("workload").is_none());
}

#[tokio::test]
async fn missing_warehouse_participants_preserve_native_coverage_and_withhold_unobserved_links() {
    let (source, records) = recorded();
    let fixture = Fixture::new(source.clone(), records).await;
    fixture.remove_first_row(TelemetryStream::ActivityObservations);
    let capture = fixture.read_families(&[EvidenceFamily::ActivityObservations]).await;
    let analysis = collected_resources::maintenance(&capture).unwrap();
    assert!(analysis.report.entries.is_empty());
    assert_eq!(analysis.report.source_state, SourceState::Partial);
    assert!(!analysis.report.read_complete);
    assert_eq!(analysis.report.observed_rows, 1);
    assert_eq!(analysis.report.native_rows, 2);
    assert!(analysis.context.read_gaps.iter().any(|gap| gap.reason == EvidenceGapReason::MissingRows));
    assert_eq!(analysis.context.source, source, "native coverage is immutable when warehouse rows are missing");
}

#[tokio::test]
async fn unavailable_current_sql_is_unclassified_and_empty_or_unavailable_sources_stay_distinct() {
    let (source, mut records) = recorded();
    let TelemetryRecord::Activity(row) = &mut records[0] else { unreachable!() };
    row.statement = DefinitionPayload::Unavailable { reason: PayloadUnavailable::Restricted };
    let fixture = Fixture::new(source, records).await;
    let capture = fixture.read_families(&[EvidenceFamily::ActivityObservations]).await;
    let analysis = collected_resources::maintenance(&capture).unwrap();
    assert_eq!(analysis.report.unclassified_requests, 1);
    assert!(analysis.report.entries.is_empty());
    assert!(analysis.report.read_complete, "a natively unavailable payload differs from warehouse loss");

    for (source, rows) in activity_evidence_cases().into_iter().skip(2) {
        let native_state = source.observation.activity().unwrap().coverage.state;
        let fixture = Fixture::new(source, rows).await;
        let capture = fixture.read_families(&[EvidenceFamily::ActivityObservations]).await;
        let report = collected_resources::maintenance(&capture).unwrap().report;
        assert_eq!(report.source_state, native_state);
        assert!(report.entries.is_empty());
    }
}

#[tokio::test]
async fn a_missing_or_wrong_selected_family_cannot_become_an_empty_maintenance_report() {
    let (source, records) = recorded();
    let fixture = Fixture::new(source, records).await;
    let capture = fixture.read_families(&[EvidenceFamily::OperationalSamples]).await;
    assert!(collected_resources::maintenance(&capture).is_err());
}

#[tokio::test]
async fn an_omitted_waiter_preserves_positive_maintenance_without_inventing_an_outgoing_edge() {
    let (source, mut records) = recorded();
    records.swap(0, 1);
    let fixture = Fixture::new(source, records).await;
    fixture.remove_first_row(TelemetryStream::ActivityObservations);
    let capture = fixture.read_families(&[EvidenceFamily::ActivityObservations]).await;
    let report = collected_resources::maintenance(&capture).unwrap().report;
    assert_eq!(report.entries.len(), 1);
    assert_eq!(report.entries[0].direction, MaintenanceDirection::Unlinked);
    assert_eq!(report.entries[0].affected_count, 0);
    assert_eq!(report.entries[0].reference.evidence.as_ref().unwrap().row_ordinal, 1);
    assert_eq!(report.source_state, SourceState::Partial);
    assert!(!report.read_complete);
}

#[tokio::test]
async fn a_truncated_sql_projection_stays_a_gap_until_explicit_authenticated_payload_hydration() {
    let (source, mut records) = recorded();
    let TelemetryRecord::Activity(row) = &mut records[0] else { unreachable!() };
    let included = (0..200).map(|index| format!("included_{index}")).collect::<Vec<_>>().join(",");
    row.statement =
        DefinitionPayload::sql(&format!("CREATE INDEX ix ON dbo.orders (id) INCLUDE ({included})"), Engine::Mssql);
    assert!(matches!(&row.statement, DefinitionPayload::Available { text, .. } if text.len() > 1024));
    let fixture = Fixture::new(source, records).await;
    let before = fixture.recovery_object_reads();
    let capture = fixture.read_families(&[EvidenceFamily::ActivityObservations]).await;
    let report = collected_resources::maintenance(&capture).unwrap().report;
    assert!(!report.read_complete);
    assert!(report.entries.is_empty());
    assert_eq!(report.source_state, SourceState::Partial);
    assert!(capture.gaps().iter().any(|gap| gap.reason == EvidenceGapReason::DetailInRecovery));
    assert_eq!(fixture.recovery_object_reads(), before, "default analysis does not open private payloads");

    let hydrated = fixture
        .read_families_with_recovery(&[EvidenceFamily::ActivityObservations], RecoveryPayloadBudget::default())
        .await;
    let report = collected_resources::maintenance(&hydrated).unwrap().report;
    assert!(report.read_complete);
    assert_eq!(report.entries[0].direction, MaintenanceDirection::BlockingSession);
    assert_eq!(report.entries[0].reference.evidence.as_ref().unwrap().row_ordinal, 0);
    assert_eq!(hydrated.metrics().recovery_objects, 1);
}

#[tokio::test]
async fn original_maintenance_roles_keep_upstream_and_downstream_references_and_distinct_verification() {
    for direction in [
        MaintenanceDirection::BlockingSession,
        MaintenanceDirection::Waiting,
        MaintenanceDirection::Both,
        MaintenanceDirection::Unlinked,
    ] {
        let (source, rows) = maintenance_activity_evidence(direction);
        let fixture = Fixture::new(source.clone(), rows.clone()).await;
        let recovery_before = fixture.recovery_object_reads();
        let capture = fixture.read_families(&[EvidenceFamily::ActivityObservations]).await;
        let report = collected_resources::maintenance(&capture).unwrap().report;
        let entry = &report.entries[0];
        assert_eq!(entry.direction, direction);
        assert_eq!(entry.reference.evidence.as_ref().unwrap().row_ordinal, 0);
        assert_eq!(entry.reference.evidence.as_ref().unwrap().capture, *capture.capture());
        let (investigation, verify) = match direction {
            MaintenanceDirection::Both => (
                culprit_core::t!("view.maintenance.investigate.both"),
                culprit_core::t!("view.maintenance.verify.both"),
            ),
            MaintenanceDirection::BlockingSession => {
                (culprit_core::t!("view.maintenance.investigate.blocking"), culprit_core::t!("view.maintenance.verify"))
            }
            MaintenanceDirection::Waiting => {
                (culprit_core::t!("view.maintenance.investigate.waiting"), culprit_core::t!("view.maintenance.verify"))
            }
            MaintenanceDirection::Unlinked => {
                (culprit_core::t!("view.maintenance.investigate.unlinked"), culprit_core::t!("view.maintenance.verify"))
            }
        };
        assert_eq!(entry.investigation, investigation);
        assert_eq!(entry.verify, verify);
        if matches!(direction, MaintenanceDirection::BlockingSession | MaintenanceDirection::Both) {
            assert_eq!(entry.affected_count, 1);
            assert_eq!(entry.affected[0].activity.session.id, "82");
            assert_eq!(entry.affected[0].evidence.as_ref().unwrap().row_ordinal, 1);
        } else {
            assert!(entry.affected.is_empty());
        }
        if matches!(direction, MaintenanceDirection::Waiting | MaintenanceDirection::Both) {
            assert_eq!(
                entry.waiting_on,
                vec![ActivityWaitOwner::Session { id: "83".into(), generation: Some("native-login-83".into()) }]
            );
        } else {
            assert!(entry.waiting_on.is_empty());
        }
        for row in &rows {
            let original = ExportedRecord::new(&source, capture.capture(), row.clone()).unwrap();
            assert!(capture.records().iter().any(|row| row.row() == &original));
        }
        assert_eq!(fixture.recovery_object_reads(), recovery_before);
        assert!(!serde_json::to_string(&report).unwrap().contains("ALTER INDEX"));
        let replay = collected_resources::maintenance(&capture).unwrap().report;
        assert_eq!(serde_json::to_value(replay).unwrap(), serde_json::to_value(report).unwrap());
    }
}
