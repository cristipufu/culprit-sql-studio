use super::*;
use test_support::{TvpLoopCase, tvp_loop_evidence};

#[tokio::test]
async fn changed_tvp_proof_or_module_metadata_cannot_be_repaired_by_a_native_payload() {
    let (source, records) = tvp_loop_evidence(TvpLoopCase::CountedJoin);
    let fixture = Fixture::new(source, records).await;
    let schema = fixture.catalog.get(TelemetryStream::ObjectDefinitions).unwrap();
    let details = schema.columns().iter().position(|column| column.name == "Details").unwrap();
    let details_sha = schema.columns().iter().position(|column| column.name == "DetailSha256").unwrap();
    let opens = fixture.recovery.object_reads.load(Ordering::SeqCst);
    *fixture.recovery.fault.lock().unwrap() = Fault::None;
    for (pointer, value) in [
        ("/relational_input_proof/source_scope", json!(digest(b"another native namespace"))),
        ("/relational_input_proof/input_sha256", json!(digest(b"another native module binding"))),
        ("/relational_input_proof/bindings/0/node_id", json!(999)),
        ("/module_parameters/object_id", json!(2002)),
        ("/context/identity/plan_generation", json!("55")),
    ] {
        let mut rows = fixture.rows_for(EvidenceFamily::ObjectDefinitions);
        let mut body: Value = serde_json::from_str(rows[0][details].as_str().unwrap()).unwrap();
        *body.pointer_mut(pointer).unwrap() = value;
        rows[0][details] = json!(serde_json::to_string(&body).unwrap());
        rows[0][details_sha] = json!(digest(&serde_json::to_vec(&body).unwrap()));
        fixture.replies(vec![rows]);
        let error = fixture
            .reader()
            .read_capture(
                &fixture.range,
                FAMILY,
                EvidenceReadBudget {
                    recovery_payloads: Some(RecoveryPayloadBudget {
                        max_objects: 1,
                        max_bytes: fixture.range.manifest().batches[0].bytes,
                    }),
                    ..budget()
                },
                Some(fixture.recovery.as_ref()),
            )
            .await
            .unwrap_err();
        assert_eq!(error.detail, Some(culprit_core::ErrorDetail::Delivery(DeliveryProblem::Integrity)), "{pointer}");
        assert_eq!(fixture.recovery.object_reads.load(Ordering::SeqCst), opens);
    }
}

#[tokio::test]
async fn a_missing_tvp_definition_stays_an_explicit_gap_without_native_fallback_or_coverage_rewrite() {
    let (source, records) = tvp_loop_evidence(TvpLoopCase::ConsumedInput);
    let fixture = Fixture::new(source.clone(), records).await;
    let opens = fixture.recovery.object_reads.load(Ordering::SeqCst);
    *fixture.recovery.fault.lock().unwrap() = Fault::None;
    fixture.replies(vec![vec![]]);
    let capture = fixture
        .reader()
        .read_capture(
            &fixture.range,
            FAMILY,
            EvidenceReadBudget { recovery_payloads: Some(RecoveryPayloadBudget::default()), ..budget() },
            Some(fixture.recovery.as_ref()),
        )
        .await
        .unwrap();
    assert!(!capture.is_read_complete());
    assert!(capture.records().is_empty());
    assert_eq!(capture.gaps()[0].reason, EvidenceGapReason::MissingRows);
    assert_eq!(capture.source(), &source);
    assert!(source.observation.snapshot().unwrap().sources[0].complete_scope);
    assert_eq!(fixture.recovery.object_reads.load(Ordering::SeqCst), opens);
}
