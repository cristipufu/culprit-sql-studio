use super::*;
use std::sync::atomic::Ordering;

const FAMILY: &[EvidenceFamily] = &[EvidenceFamily::OperationalSamples];

#[tokio::test]
async fn focused_grant_round_trip_keeps_exact_original_context_and_publication_omits_private_owners() {
    for peers in [0, 3] {
        let (source, records) = test_support::focused_request_memory_evidence_with_peers(peers);
        let fixture = Fixture::new(source.clone(), records.clone()).await;
        let before = fixture.recovery.object_reads.load(Ordering::SeqCst);
        fixture.replies(vec![fixture.rows.clone()]);
        let capture =
            fixture.reader().read_capture(&fixture.range, FAMILY, EvidenceReadBudget::default(), None).await.unwrap();
        test_support::assert_operational_capture(&capture, &source, &records);
        assert_eq!(fixture.recovery.object_reads.load(Ordering::SeqCst), before);
        let SourceObservation::FocusedRequestMemory { result, .. } = &source.observation else { panic!() };
        let grants = decoded_grants(&capture);
        assert_eq!(grants.len(), peers + 1);
        assert!(result.matches_observations(grants.iter().copied()));
        assert_eq!(result.peers.len(), peers);
        assert_eq!(grants.iter().filter(|grant| result.is_subject(grant)).count(), 1);
        for grant in &grants {
            assert_eq!(grant.resource, result.resource);
            let expected = records
                .iter()
                .find_map(|record| match record {
                    TelemetryRecord::Operational(OperationalObservation {
                        fact: OperationalFact::ExecutionGrant(expected),
                        ..
                    }) if expected.identity == grant.identity => Some(expected.as_ref()),
                    _ => None,
                })
                .unwrap();
            assert_eq!(grant.queue, expected.queue);
            if result.is_peer(grant) {
                assert!(matches!(
                    grant.request.as_ref().unwrap().statement,
                    DefinitionPayload::Unavailable { reason: PayloadUnavailable::NotCollected }
                ));
                assert_eq!(grant.statement_relationship, GrantStatementRelationship::Unresolved);
            }
        }
        let schema = fixture.catalog.get(TelemetryStream::OperationalSamples).unwrap();
        let column = schema.columns().iter().position(|column| column.name == "FocusKey").unwrap();
        assert_eq!(fixture.rows[0][column], result.focus.key().unwrap());
        let schema = fixture.catalog.publication();
        let column = schema.columns().iter().position(|column| column.name == "SourceDescription").unwrap();
        let public: Value = serde_json::from_str(fixture.publication[column].as_str().unwrap()).unwrap();
        assert_eq!(public["observation"]["kind"], "focused-request-memory-context");
        assert_eq!(public["observation"]["data"]["focus_key"], result.focus.key().unwrap());
        let encoded = serde_json::to_string(&public).unwrap();
        assert!(!encoded.contains("private-memory") && !encoded.contains("private_memory"));
        assert!(public["observation"]["data"].get("focus").is_none());
        assert!(public["observation"]["data"].get("original").is_none());
        for private in ["subject", "resource", "peers"] {
            assert!(public["observation"]["data"].get(private).is_none());
        }
    }
}

#[tokio::test]
async fn another_valid_focus_key_cannot_replace_the_original_grant_receipt_even_with_recovery_enabled() {
    let (source, records) = test_support::focused_request_memory_evidence();
    let fixture = Fixture::new(source, records).await;
    let before = fixture.recovery.object_reads.load(Ordering::SeqCst);
    *fixture.recovery.fault.lock().unwrap() = Fault::None;
    let schema = fixture.catalog.get(TelemetryStream::OperationalSamples).unwrap();
    let column = schema.columns().iter().position(|column| column.name == "FocusKey").unwrap();
    let mut rows = fixture.rows.clone();
    rows[0][column] = json!(digest(b"another structurally valid focused request"));
    fixture.replies(vec![rows]);
    let error = fixture
        .reader()
        .read_capture(
            &fixture.range,
            FAMILY,
            EvidenceReadBudget { recovery_payloads: Some(RecoveryPayloadBudget::default()), ..Default::default() },
            Some(fixture.recovery.as_ref()),
        )
        .await
        .unwrap_err();
    assert_eq!(error.detail, Some(culprit_core::ErrorDetail::Delivery(DeliveryProblem::Integrity)));
    assert_eq!(fixture.recovery.object_reads.load(Ordering::SeqCst), before);
}

fn decoded_grants(capture: &EvidenceCapture) -> Vec<&ExecutionGrantObservation> {
    capture
        .records()
        .iter()
        .map(|row| match &row.row().record {
            TelemetryRecord::Operational(OperationalObservation {
                fact: OperationalFact::ExecutionGrant(grant),
                ..
            }) => grant.as_ref(),
            _ => panic!("focused grant fact"),
        })
        .collect()
}

#[tokio::test]
async fn an_omitted_original_or_peer_preserves_the_frozen_roster_and_explicit_read_gap() {
    for missing in [0, 2] {
        let (source, records) = test_support::focused_request_memory_evidence_with_peers(3);
        let fixture = Fixture::new(source.clone(), records).await;
        let before = fixture.recovery.object_reads.load(Ordering::SeqCst);
        let mut rows = fixture.rows.clone();
        rows.remove(missing);
        fixture.replies(vec![rows]);
        let capture =
            fixture.reader().read_capture(&fixture.range, FAMILY, EvidenceReadBudget::default(), None).await.unwrap();
        assert_eq!(capture.source(), &source);
        assert!(!capture.is_read_complete());
        assert_eq!(capture.records().len(), 3);
        assert_eq!(capture.gaps().len(), 1);
        assert_eq!(capture.gaps()[0].reason, EvidenceGapReason::MissingRows);
        assert_eq!(capture.gaps()[0].row_ordinal, Some((missing + 1) as u32));
        let SourceObservation::FocusedRequestMemory { result, snapshot } = &capture.source().observation else {
            panic!()
        };
        assert_eq!(snapshot.records, 4);
        assert_eq!(snapshot.sources[0].state, culprit_core::SourceState::Collected);
        assert_eq!(result.peers.len(), 3);
        let grants = decoded_grants(&capture);
        assert!(grants.iter().all(|row| result.accepts_observation(row)));
        assert!(!result.matches_observations(grants));
        assert_eq!(capture.metrics().recovery_objects, 0);
        assert_eq!(fixture.recovery.object_reads.load(Ordering::SeqCst), before);
        assert!(matches!(*fixture.recovery.fault.lock().unwrap(), Fault::Stall));
    }
}

#[tokio::test]
async fn peer_identity_resource_queue_and_request_proof_cannot_be_substituted_before_hydration() {
    let (source, records) = test_support::focused_request_memory_evidence_with_peers(3);
    let fixture = Fixture::new(source, records).await;
    let before = fixture.recovery.object_reads.load(Ordering::SeqCst);
    *fixture.recovery.fault.lock().unwrap() = Fault::None;
    let schema = fixture.catalog.get(TelemetryStream::OperationalSamples).unwrap();
    let column = schema.columns().iter().position(|column| column.name == "Details").unwrap();
    for (pointer, value) in [
        ("/identity/id", json!("different-native-grant")),
        ("/resource/id", json!("different-resource")),
        ("/queue/identity/id", json!("different-queue")),
        ("/queue/next_candidate", json!(true)),
        ("/request/key/request/generation", json!("recycled-request")),
        ("/request/statement_identity/id", json!("changed-native-statement")),
    ] {
        let mut rows = fixture.rows.clone();
        let mut details: Value = serde_json::from_str(rows[1][column].as_str().unwrap()).unwrap();
        *details.pointer_mut(pointer).expect("original peer field") = value;
        rows[1][column] = json!(serde_json::to_string(&details).unwrap());
        fixture.replies(vec![rows]);
        let error = fixture
            .reader()
            .read_capture(
                &fixture.range,
                FAMILY,
                EvidenceReadBudget { recovery_payloads: Some(RecoveryPayloadBudget::default()), ..Default::default() },
                Some(fixture.recovery.as_ref()),
            )
            .await
            .unwrap_err();
        assert_eq!(error.detail, Some(culprit_core::ErrorDetail::Delivery(DeliveryProblem::Integrity)), "{pointer}");
        assert_eq!(fixture.recovery.object_reads.load(Ordering::SeqCst), before, "{pointer}");
    }
}
