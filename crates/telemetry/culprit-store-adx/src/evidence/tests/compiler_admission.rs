use super::*;
use std::sync::atomic::Ordering;

const FAMILY: &[EvidenceFamily] = &[EvidenceFamily::OperationalSamples];

#[tokio::test]
async fn compiler_admission_round_trip_preserves_the_exact_waiter_and_independent_gateway_roster() {
    let (source, records) = test_support::compiler_admission_evidence();
    let fixture = Fixture::new(source.clone(), records.clone()).await;
    let before = fixture.recovery.object_reads.load(Ordering::SeqCst);
    fixture.replies(vec![fixture.rows.clone()]);
    let capture = fixture.reader().read_capture(&fixture.range, FAMILY, budget(), None).await.unwrap();
    test_support::assert_operational_capture(&capture, &source, &records);
    assert_eq!(fixture.recovery.object_reads.load(Ordering::SeqCst), before);
    let SourceObservation::FocusedCompilationAdmission { result, snapshot } = &source.observation else { panic!() };
    let observations = decoded_observations(&capture);
    assert!(result.matches_observations(observations.iter().copied()));
    assert_eq!(snapshot.dataset, SnapshotDataset::CompilerAdmission);
    assert_eq!(observations.len(), 3);
    let OperationalFact::CompilerWaiter(waiter) = &observations[0].fact else { panic!() };
    assert_eq!(waiter.request.key, result.focus.original.key);
    assert_eq!(waiter.request.statement_identity, Some(result.focus.statement_identity.clone()));
    assert_eq!(waiter.observed_at, result.observed_until);
    let gateways =
        observations.iter().skip(1).map(|row| CompilerGatewayFacts::from_observation(row).unwrap()).collect::<Vec<_>>();
    assert_eq!(
        gateways.iter().map(|gateway| gateway.identity).collect::<Vec<_>>(),
        result.gateways.iter().collect::<Vec<_>>()
    );
    assert_eq!(gateways[0].resource_pool_id, "2");
    assert_eq!(gateways[0].threshold_bytes, None);
    assert!(!gateways[0].required);
    assert_eq!(gateways[0].waiting_compilers, 3);
    assert_eq!(gateways[1].resource_pool_id, "3");
    assert_eq!(gateways[1].threshold_bytes, Some(9_007_199_254_740_993));
    assert!(gateways[1].required);
    assert_eq!(gateways[1].waiting_compilers, 0);
    let schema = fixture.catalog.get(TelemetryStream::OperationalSamples).unwrap();
    let focus = schema.columns().iter().position(|column| column.name == "FocusKey").unwrap();
    assert!(fixture.rows.iter().all(|row| row[focus] == result.focus.key().unwrap()));
    assert_eq!(capture.metrics().recovery_objects, 0);
}

#[tokio::test]
async fn a_missing_waiter_or_gateway_keeps_the_original_roster_and_an_explicit_read_gap() {
    for missing in 0..3 {
        let (source, records) = test_support::compiler_admission_evidence();
        let fixture = Fixture::new(source.clone(), records).await;
        let before = fixture.recovery.object_reads.load(Ordering::SeqCst);
        let mut rows = fixture.rows.clone();
        rows.remove(missing);
        fixture.replies(vec![rows]);
        let capture = fixture.reader().read_capture(&fixture.range, FAMILY, budget(), None).await.unwrap();
        assert_eq!(capture.source(), &source);
        assert!(!capture.is_read_complete());
        assert_eq!(capture.records().len(), 2);
        assert_eq!(capture.gaps().len(), 1);
        assert_eq!(capture.gaps()[0].reason, EvidenceGapReason::MissingRows);
        assert_eq!(capture.gaps()[0].row_ordinal, Some((missing + 1) as u32));
        let SourceObservation::FocusedCompilationAdmission { result, snapshot } = &capture.source().observation else {
            panic!()
        };
        assert_eq!(snapshot.records, 3);
        assert_eq!(snapshot.sources[0].state, culprit_core::SourceState::Collected);
        assert_eq!(result.gateways.len(), 2);
        assert_eq!(result.membership, CompilationAdmissionMembership::Observed);
        let observations = decoded_observations(&capture);
        assert!(observations.iter().all(|row| result.accepts_observation(row)));
        assert!(!result.matches_observations(observations));
        assert_eq!(capture.metrics().recovery_objects, 0);
        assert_eq!(fixture.recovery.object_reads.load(Ordering::SeqCst), before);
        assert!(matches!(*fixture.recovery.fault.lock().unwrap(), Fault::Stall));
    }
}

#[tokio::test]
async fn changed_focus_request_generation_gateway_identity_or_counter_fails_before_recovery_opens() {
    let (source, records) = test_support::compiler_admission_evidence();
    let fixture = Fixture::new(source, records).await;
    let before = fixture.recovery.object_reads.load(Ordering::SeqCst);
    *fixture.recovery.fault.lock().unwrap() = Fault::None;
    let schema = fixture.catalog.get(TelemetryStream::OperationalSamples).unwrap();
    let details = schema.columns().iter().position(|column| column.name == "Details").unwrap();
    let focus = schema.columns().iter().position(|column| column.name == "FocusKey").unwrap();
    for (row, pointer, replacement) in [
        (0, None, json!(digest(b"another valid compiler question"))),
        (0, Some("/request/key/request/generation"), json!("recycled-compiler-request")),
        (0, Some("/request/statement_identity/id"), json!("another-compiler-statement")),
        (1, Some("/identity/qualifiers/resource_pool_id"), json!("7")),
        (1, Some("/counters/1/value/value"), json!("0")),
    ] {
        let mut rows = fixture.rows.clone();
        if let Some(pointer) = pointer {
            let mut value: Value = serde_json::from_str(rows[row][details].as_str().unwrap()).unwrap();
            *value.pointer_mut(pointer).expect("existing native compiler field") = replacement;
            rows[row][details] = json!(value.to_string());
        } else {
            rows[row][focus] = replacement;
        }
        fixture.replies(vec![rows]);
        let error = fixture
            .reader()
            .read_capture(
                &fixture.range,
                FAMILY,
                EvidenceReadBudget { recovery_payloads: Some(RecoveryPayloadBudget::default()), ..budget() },
                Some(fixture.recovery.as_ref()),
            )
            .await
            .unwrap_err();
        assert_eq!(error.detail, Some(culprit_core::ErrorDetail::Delivery(DeliveryProblem::Integrity)), "{pointer:?}");
        assert_eq!(fixture.recovery.object_reads.load(Ordering::SeqCst), before, "{pointer:?}");
    }
}

fn decoded_observations(capture: &EvidenceCapture) -> Vec<&OperationalObservation> {
    capture
        .records()
        .iter()
        .map(|row| match &row.row().record {
            TelemetryRecord::Operational(observation) => observation,
            _ => panic!("compiler admission fact"),
        })
        .collect()
}
