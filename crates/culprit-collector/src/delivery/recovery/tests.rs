use super::*;
use crate::analytical::test_support::{self, Fault, Recovery};
use culprit_core::{ErrorDetail, telemetry::*};
use std::sync::atomic::Ordering;

fn foreign_population_binding(empty: bool) -> Recovery {
    let (source, records) = test_support::query_population_evidence();
    let (fixture, _) = test_support::fixture();
    let mut recovery =
        Recovery::capture("header-admission-fixture", fixture.contract.schema_digest(), source, records, 42);
    if empty {
        recovery.make_empty();
    }
    recovery.manifest.validate().unwrap();
    assert_eq!(recovery.manifest.batches.iter().all(|batch| batch.rows.is_empty()), empty);
    let original_batches = recovery.manifest.batches.clone();
    let original_receipts = recovery.manifest.receipts_digest().unwrap();
    let SourceObservation::QueryHistory(history) = &mut recovery.manifest.source.observation else {
        panic!("native query history")
    };
    let QueryHistoryPopulationBinding::Dimensions { bindings } = &mut history.population_binding else {
        panic!("source-owned population binding")
    };
    bindings[0].population = NativeIdentity::new("valid-native-identity-outside-the-catalog");
    assert!(!history.is_valid());
    assert_eq!(recovery.manifest.batches, original_batches, "native artifacts and every receipt remain unchanged");
    assert_eq!(recovery.manifest.receipts_digest().unwrap(), original_receipts);
    *recovery.fault.lock().unwrap() = Fault::Stall;
    recovery
}

#[test]
fn invalid_population_binding_fails_manifest_header_admission_with_or_without_native_rows() {
    for empty in [false, true] {
        let recovery = foreign_population_binding(empty);
        let error = recovery.manifest.validate().unwrap_err();
        assert_eq!(error.detail, Some(ErrorDetail::Delivery(DeliveryProblem::Integrity)));
        assert_eq!(recovery.object_reads.load(Ordering::SeqCst), 0);
    }
}

#[tokio::test]
async fn reopening_rejects_invalid_population_metadata_before_payload_or_warehouse_work() {
    for empty in [false, true] {
        let recovery = foreign_population_binding(empty);
        let error = reopen(&recovery, &recovery.manifest.range_id).await.err().expect("invalid source header rejected");
        assert_eq!(error.detail, Some(ErrorDetail::Delivery(DeliveryProblem::Integrity)));
        assert_eq!(recovery.object_reads.load(Ordering::SeqCst), 0);
        assert!(matches!(*recovery.fault.lock().unwrap(), Fault::Stall));
    }
}
