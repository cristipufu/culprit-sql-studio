use super::*;
use std::sync::atomic::Ordering;

const FAMILY: &[EvidenceFamily] = &[EvidenceFamily::ActivityObservations];

#[tokio::test]
async fn retained_lock_closures_replay_every_native_role_and_original_boundary_without_opening_private_payloads() {
    for (source, records) in test_support::blocking_evidence_cases() {
        let fixture = Fixture::new(source.clone(), records.clone()).await;
        let opened = fixture.recovery.object_reads.load(Ordering::SeqCst);
        fixture.replies(fixture.rows.iter().map(|row| vec![row.clone()]).collect());
        let capture = fixture
            .reader()
            .read_capture(&fixture.range, FAMILY, EvidenceReadBudget { page_rows: 1, ..Default::default() }, None)
            .await
            .unwrap();
        test_support::assert_blocking_capture(&capture, &source, &records);
        assert_eq!(capture.metrics().pages as usize, records.len());
        assert_eq!(fixture.recovery.object_reads.load(Ordering::SeqCst), opened);
        assert!(matches!(*fixture.recovery.fault.lock().unwrap(), Fault::Stall));
    }
}

#[tokio::test]
async fn missing_native_participants_preserve_the_exact_header_and_never_become_an_empty_complete_chain() {
    let (source, records) = test_support::blocking_evidence(test_support::BlockingCase::Matched);
    let fixture = Fixture::new(source.clone(), records.clone()).await;
    let opened = fixture.recovery.object_reads.load(Ordering::SeqCst);
    fixture.replies(vec![fixture.rows[1..].to_vec()]);
    let capture =
        fixture.reader().read_capture(&fixture.range, FAMILY, EvidenceReadBudget::default(), None).await.unwrap();
    assert_eq!(capture.source(), &source);
    assert_eq!(capture.records().len(), 1);
    assert!(!capture.is_read_complete());
    assert!(capture.gaps().iter().any(|gap| gap.reason == EvidenceGapReason::MissingRows));
    let native = capture.source().observation.activity().unwrap();
    assert_eq!(native.native_rows, 2);
    assert_eq!(native.observations, 2);
    assert!(!native.coverage.complete_scope);
    assert_eq!(fixture.recovery.object_reads.load(Ordering::SeqCst), opened);
}

#[tokio::test]
async fn changed_request_owner_generation_statement_or_exact_counter_fails_the_original_activity_receipt() {
    let (source, records) = test_support::blocking_evidence(test_support::BlockingCase::Matched);
    let fixture = Fixture::new(source.clone(), records.clone()).await;
    let opened = fixture.recovery.object_reads.load(Ordering::SeqCst);
    let schema = fixture.catalog.get(TelemetryStream::ActivityObservations).unwrap();
    let facts_column = schema.columns().iter().position(|column| column.name == "ActivityFacts").unwrap();
    for (pointer, value) in [
        ("/key/session/generation", json!("2020-01-01T11:01:42Z")),
        ("/key/request/generation", json!("2020-01-01T11:59:01Z")),
        ("/statement_identity/id", json!("another-native-statement")),
        ("/wait/owners/0/generation", json!("2020-01-01T11:01:43Z")),
        ("/elapsed_ms", json!("9007199254740992")),
        ("/database", json!("another_database")),
    ] {
        let mut changed = fixture.rows.clone();
        let mut facts: Value = serde_json::from_str(changed[0][facts_column].as_str().unwrap()).unwrap();
        *facts.pointer_mut(pointer).expect("canonical native fixture field") = value;
        changed[0][facts_column] = json!(serde_json::to_string(&facts).unwrap());
        fixture.replies(vec![changed]);
        let error = fixture
            .reader()
            .read_capture(&fixture.range, FAMILY, EvidenceReadBudget::default(), None)
            .await
            .unwrap_err();
        assert_eq!(error.detail, Some(culprit_core::ErrorDetail::Delivery(DeliveryProblem::Integrity)), "{pointer}");
        assert_eq!(fixture.recovery.object_reads.load(Ordering::SeqCst), opened);
    }
}

#[tokio::test]
async fn changed_public_focus_binding_closure_or_process_scope_is_not_accepted_as_the_original_capture() {
    for (pointer, value) in [
        ("/observation/data/focus_key", json!(digest(b"different-original-blocked-request"))),
        ("/observation/data/binding", json!("unverified-original-scope")),
        ("/observation/data/request_end", json!("missing")),
        ("/observation/data/closure/status", json!("depth-limit")),
        ("/observation/data/snapshot/epoch", json!("different-native-process")),
        ("/observation/data/snapshot/coverage/records", json!(0)),
    ] {
        let (source, records) = test_support::blocking_evidence(test_support::BlockingCase::Matched);
        let mut fixture = Fixture::new(source.clone(), records.clone()).await;
        let opened = fixture.recovery.object_reads.load(Ordering::SeqCst);
        let column = fixture
            .catalog
            .publication()
            .columns()
            .iter()
            .position(|column| column.name == "SourceDescription")
            .unwrap();
        let mut public: Value = serde_json::from_str(fixture.publication[column].as_str().unwrap()).unwrap();
        *public.pointer_mut(pointer).expect("public closure boundary field") = value;
        fixture.publication[column] = json!(serde_json::to_string(&public).unwrap());
        fixture.replies(vec![fixture.rows.clone()]);
        let error = fixture
            .reader()
            .read_capture(&fixture.range, FAMILY, EvidenceReadBudget::default(), None)
            .await
            .unwrap_err();
        assert_eq!(error.detail, Some(culprit_core::ErrorDetail::Delivery(DeliveryProblem::Integrity)), "{pointer}");
        assert_eq!(fixture.recovery.object_reads.load(Ordering::SeqCst), opened);
    }
}
