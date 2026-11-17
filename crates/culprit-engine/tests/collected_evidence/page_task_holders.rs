use super::fixture::Fixture;
use culprit_collector::analytical::{
    evidence::{EvidenceFamily, EvidenceGapReason, RecoveryPayloadBudget},
    test_support::{PageTaskHolderCase, page_task_holder_evidence},
};
use culprit_core::{SourceOutcome, SourceState, telemetry::*};
use culprit_engine::collected_resources;

const FAMILY: &[EvidenceFamily] = &[EvidenceFamily::OperationalSamples];

#[tokio::test]
async fn every_holder_case_replays_the_same_point_explanation_with_exact_original_context_and_native_fields() {
    for case in PageTaskHolderCase::ALL {
        let (source, records) = page_task_holder_evidence(case);
        let (focus, point) = native(&source, &records);
        let coverage = &source.observation.snapshot().unwrap().sources[0];
        let expected = culprit_checks::page_waits::explain_task_holders(
            focus,
            &NativeReadContext::from_snapshot(&source).unwrap(),
            Some(point),
            Some(&SourceOutcome {
                source: coverage.source.clone(),
                state: coverage.state,
                reason: None,
                reason_message: None,
            }),
        )
        .unwrap();
        let fixture = Fixture::new(source.clone(), records.clone()).await;
        let capture = fixture.read_families_with_recovery(FAMILY, RecoveryPayloadBudget::default()).await;
        let actual = collected_resources::page_task_holders(&capture).unwrap();
        assert!(capture.is_read_complete(), "{case:?}: {:?}", capture.gaps());
        assert_eq!(actual.context.source, source, "{case:?}");
        assert_eq!(actual.context.records.len(), 1);
        assert_eq!(
            actual.context.records[0].row,
            ExportedRecord::new(&source, capture.capture(), records[0].clone()).unwrap()
        );
        assert_eq!(serde_json::to_value(&actual.report).unwrap(), serde_json::to_value(expected).unwrap(), "{case:?}");
        assert_eq!(actual.report.original_page, focus.page);
        assert_eq!(actual.report.focus_key, focus.key().unwrap());
        assert_eq!(actual.report.target_state, Some(point.target_state));
        assert_eq!(actual.report.entries.len(), 2);
        for (entry, native) in actual.report.entries.iter().zip([&point.before, &point.after]) {
            assert_eq!(entry.observed_at, native.observed_at);
            assert_eq!(entry.relation, native.holder.relation);
            assert_eq!(entry.task, native.holder.task);
            assert_eq!(entry.request.as_ref(), native.holder.request.as_ref().map(|request| &request.key));
            assert_eq!(entry.statement.as_ref(), native.holder.request.as_ref().map(|request| &request.statement));
        }
        assert_ne!(point.before.observed_at, point.after.observed_at);
        assert_eq!(focus.task.wait_duration_ms.as_deref(), Some("9007199254740993"));
        assert_eq!(focus.participant.original.wait.as_ref().unwrap().native_type.as_deref(), Some("CXCONSUMER"));
        assert_eq!(focus.task.wait_type, "PAGELATCH_EX");
        if case == PageTaskHolderCase::DifferentHolders {
            assert_ne!(actual.report.entries[0].request, actual.report.entries[1].request);
            assert_ne!(actual.report.entries[0].statement_identity, actual.report.entries[1].statement_identity);
        }
        assert_eq!(actual.report.source_state, Some(coverage.state));
        assert!(actual.context.read_gaps.is_empty());
        assert!(actual.context.metrics.recovery_objects <= 1, "one original native batch, not per-point rereads");
    }
}

#[tokio::test]
async fn sql_preview_cannot_release_a_holder_verdict_and_exact_bounded_recovery_restores_both_native_points() {
    let (source, records) = page_task_holder_evidence(PageTaskHolderCase::LongSql);
    let focus = native(&source, &records).0.clone();
    let fixture = Fixture::new(source.clone(), records.clone()).await;
    let opens = fixture.recovery_object_reads();
    let preview = fixture.read_families(FAMILY).await;
    assert!(preview.records().is_empty());
    assert!(!preview.is_read_complete());
    assert!(matches!(
        preview.gaps()[0].reason,
        EvidenceGapReason::DetailInRecovery | EvidenceGapReason::NativeRepresentationRequired
    ));
    assert_eq!(fixture.recovery_object_reads(), opens);
    let report = collected_resources::page_task_holders(&preview).unwrap();
    assert!(report.report.entries.is_empty());
    assert_eq!(report.report.original_page, focus.page);
    assert!(report.report.target_state.is_none());
    assert_eq!(report.report.source_state, Some(SourceState::Partial));
    let bytes = fixture.range.manifest().batches[0].bytes;
    let capture =
        fixture.read_families_with_recovery(FAMILY, RecoveryPayloadBudget { max_objects: 1, max_bytes: bytes }).await;
    let restored = collected_resources::page_task_holders(&capture).unwrap();
    assert_eq!(restored.context.records[0].row.record, records[0]);
    assert_eq!(restored.context.source, source);
    assert_eq!(restored.context.metrics.recovery_objects, 1);
    assert_eq!(restored.context.metrics.recovery_bytes, bytes);
    assert_eq!(fixture.recovery_object_reads() - opens, 1);
    for entry in &restored.report.entries {
        let Some(DefinitionPayload::Available { text, .. }) = &entry.statement else { panic!("exact original SQL") };
        assert!(text.len() > 1024);
        assert!(!text.contains("9264"));
    }
}

#[tokio::test]
async fn a_missing_warehouse_body_is_distinct_from_an_original_partial_or_permission_unavailable_source() {
    for case in [
        PageTaskHolderCase::QualifiedBoth,
        PageTaskHolderCase::PermissionUnavailable,
        PageTaskHolderCase::PartialWarehouseBody,
    ] {
        let (source, records) = page_task_holder_evidence(case);
        let focus = native(&source, &records).0.clone();
        let fixture = Fixture::new(source.clone(), records).await;
        let intact = fixture.read_families_with_recovery(FAMILY, RecoveryPayloadBudget::default()).await;
        let report = collected_resources::page_task_holders(&intact).unwrap();
        assert_eq!(report.report.source_state, Some(SourceState::Partial));
        assert!(report.context.read_gaps.is_empty());
        assert_eq!(report.report.entries.len(), 2);
        if case == PageTaskHolderCase::PermissionUnavailable {
            assert!(report.report.entries.iter().all(|entry| entry.relation == PageTaskHolderRelation::Unavailable));
        }
        let opens = fixture.recovery_object_reads();
        fixture.remove_first_row(TelemetryStream::OperationalSamples);
        let missing = fixture.read_families_with_recovery(FAMILY, RecoveryPayloadBudget::default()).await;
        let analysis = collected_resources::page_task_holders(&missing).unwrap();
        assert_eq!(analysis.context.source, source);
        assert!(analysis.context.records.is_empty());
        assert_eq!(analysis.report.original_page, focus.page);
        assert!(analysis.report.entries.is_empty() && analysis.report.target_state.is_none());
        assert_eq!(analysis.report.source_state, Some(SourceState::Partial));
        assert_eq!(analysis.context.read_gaps[0].reason, EvidenceGapReason::MissingRows);
        assert_eq!(fixture.recovery_object_reads(), opens, "recovery cannot invent an absent row receipt");
        assert_eq!(source.observation.snapshot().unwrap().records, 1);
    }
}

#[test]
fn producer_boundary_rejects_foreign_original_receipts_native_scopes_pointers_and_invalid_payloads() {
    let (source, records) = page_task_holder_evidence(PageTaskHolderCase::QualifiedBoth);
    let capture = culprit_collector::analytical::test_support::fixture().0.capture;
    assert!(ExportedRecord::new(&source, &capture, records[0].clone()).is_ok());
    for field in ["page-receipt", "activity-receipt", "role", "member", "epoch", "database", "pointer", "payload"] {
        let mut changed_source = source.clone();
        let mut record = records[0].clone();
        match field {
            "role" => {
                let SourceObservation::PageTaskHolders { database_writable, .. } = &mut changed_source.observation
                else {
                    panic!()
                };
                *database_writable = Some(false);
            }
            "member" => {
                let SourcePartition::InstanceLocal { member: Some(member), .. } = &mut changed_source.partition else {
                    panic!()
                };
                member.native.id = "different-native-member".into();
            }
            "epoch" => {
                let SourcePartition::InstanceLocal { epoch, .. } = &mut changed_source.partition else { panic!() };
                *epoch = Some("different-native-process".into());
            }
            "database" => {
                changed_source.target = culprit_core::DatabaseIdentity::server(
                    culprit_core::Engine::Mssql,
                    "other",
                    "fixture.invalid",
                    1433,
                    None,
                )
                .into()
            }
            "page-receipt" | "activity-receipt" => {
                let SourceObservation::PageTaskHolders { focus, .. } = &mut changed_source.observation else {
                    panic!()
                };
                let reference = if field == "page-receipt" { &mut focus.page } else { &mut focus.participant.parent };
                reference.manifest_sha256 = digest(b"another original sealed capture");
            }
            _ => {
                let TelemetryRecord::Operational(OperationalObservation {
                    fact: OperationalFact::PageTaskHolders(row),
                    ..
                }) = &mut record
                else {
                    panic!()
                };
                if field == "pointer" {
                    row.after_task.as_mut().unwrap().blocking_task_address = Some("0000000000000099".into());
                } else {
                    row.before.holder.request.as_mut().unwrap().statement = DefinitionPayload::Available {
                        text: "SELECT 'private-invalid-payload'".into(),
                        sha256: digest(b"different original"),
                        redaction_version: REDACTION_VERSION,
                    };
                }
            }
        }
        assert!(ExportedRecord::new(&changed_source, &capture, record).is_err(), "{field}");
    }
}

fn native<'a>(
    source: &'a CollectionDescription,
    records: &'a [TelemetryRecord],
) -> (&'a PageTaskHolderFocus, &'a PageTaskHolderObservation) {
    let SourceObservation::PageTaskHolders { focus, .. } = &source.observation else { panic!() };
    let TelemetryRecord::Operational(OperationalObservation { fact: OperationalFact::PageTaskHolders(point), .. }) =
        &records[0]
    else {
        panic!()
    };
    (focus, point)
}
