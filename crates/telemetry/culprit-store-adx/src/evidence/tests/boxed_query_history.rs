//! Both destinations replay the original logical namespace and the independently acquired native owner.
use super::*;
use std::sync::atomic::Ordering;
const FAMILIES: [EvidenceFamily; 3] =
    [EvidenceFamily::QueryRuntime, EvidenceFamily::QueryDefinitions, EvidenceFamily::PlanDefinitions];
#[tokio::test]
async fn boxed_history_retains_completed_owner_original_ids_and_observed_membership_without_recovery() {
    let (source, records) = test_support::boxed_query_history_evidence();
    let fixture = Fixture::new(source.clone(), records.clone()).await;
    let opens = fixture.recovery.object_reads.load(Ordering::SeqCst);
    assert!(matches!(*fixture.recovery.fault.lock().unwrap(), Fault::Stall));
    fixture.reply_families(&FAMILIES, 250);
    let capture =
        fixture.reader().read_capture(&fixture.range, &FAMILIES, EvidenceReadBudget::default(), None).await.unwrap();
    test_support::assert_query_cohort_capture(&capture, &source, &records);
    assert_eq!(capture.source(), &source);
    assert_eq!(capture.metrics().selected_rows, 3);
    assert_eq!(capture.metrics().recovery_objects, 0);
    assert_eq!(fixture.recovery.object_reads.load(Ordering::SeqCst), opens);
    let history = capture.source().query_history().unwrap();
    assert_eq!(capture.source().partition, SourcePartition::Target);
    assert!(capture.source().target.database().unwrap().incarnation.is_none());
    assert!(history.acquisition_owner.as_ref().unwrap().has_qualified_local_owner());
    assert!(!history.population_catalog.as_ref().unwrap().has_complete_membership());
    assert!(matches!(
        history.population_catalog.as_ref().unwrap().membership,
        culprit_core::RuntimePopulationMembership::ObservedWindow { .. }
    ));
    let native = capture
        .records()
        .iter()
        .find_map(|row| match &row.row().record {
            TelemetryRecord::QueryRuntime(row) => Some(row),
            _ => None,
        })
        .unwrap();
    assert_eq!(native.executions, "9007199254740993");
    assert_eq!(native.key.native.qualifiers["replica_group_id"], "1");
    assert!(native.key.references.is_resolved());
}
