use super::*;
use std::sync::atomic::Ordering;

async fn collected(source: CollectionDescription, records: Vec<TelemetryRecord>, family: EvidenceFamily) -> Fixture {
    let catalog = Arc::new(SchemaCatalog::new().unwrap());
    let recovery =
        test_support::collected_resource_recovery("clickhouse:test", catalog.digest(), source, records).await;
    Fixture::from_recovery(catalog, Arc::new(recovery), &[family.stream()]).await
}

fn cases() -> [(CollectionDescription, Vec<TelemetryRecord>, EvidenceFamily); 2] {
    let (counter_source, counters) = test_support::output_wait_counter_evidence();
    let (grant_source, grants) = test_support::output_wait_grant_evidence();
    [
        (counter_source, counters, EvidenceFamily::WaitCounterSamples),
        (grant_source, grants, EvidenceFamily::OperationalSamples),
    ]
}

#[tokio::test]
async fn sealed_restarted_committed_replay_preserves_separate_cumulative_output_waits_and_current_grant_inputs() {
    for (source, records, family) in cases() {
        let fixture = collected(source.clone(), records.clone(), family).await;
        fixture.replies(vec![fixture.rows.clone()]);
        let opened = fixture.recovery.object_reads.load(Ordering::SeqCst);
        let capture =
            fixture.reader.read_capture(&fixture.range, &[family], EvidenceReadBudget::default(), None).await.unwrap();
        test_support::assert_grant_output_capture(&capture, &source, &records);
        assert_eq!(capture.captured_rows(family.stream()), Some(records.len() as u32));
        assert_eq!(capture.metrics().queries, 3);
        assert_eq!(capture.metrics().pages, 1);
        assert_eq!(capture.metrics().recovery_objects, 0);
        assert_eq!(fixture.query.requests.lock().unwrap().len(), 3);
        assert!(fixture.query.replies.lock().unwrap().is_empty());
        assert_eq!(fixture.recovery.object_reads.load(Ordering::SeqCst), opened);
    }
}

#[tokio::test]
async fn changed_output_mechanism_raw_wait_or_current_grant_body_cannot_borrow_original_projection_receipts() {
    let mechanism =
        serde_json::to_string(&Some(WaitMechanism::LogRateGovernance(LogRateGovernanceReason::ReplicaLogApply)))
            .unwrap();
    for (column, value) in [
        ("WaitMechanism", json!(mechanism)),
        ("WaitMechanism", Value::Null),
        ("WaitType", json!("RESOURCE_SEMAPHORE")),
        ("WaitTimeMs", json!("9007199254740992")),
        ("MeasurementScope", json!("server")),
    ] {
        let (source, records) = test_support::output_wait_counter_evidence();
        let family = EvidenceFamily::WaitCounterSamples;
        let fixture = collected(source, records, family).await;
        let schema = fixture.catalog.get(family.stream()).unwrap();
        let column = schema.columns().iter().position(|field| field.name == column).unwrap();
        let mut rows = fixture.rows.clone();
        rows[0][column] = value;
        fixture.replies(vec![rows]);
        assert_projection_rejected(&fixture, family).await;
    }
    for (pointer, value) in [
        ("/request/wait/native_type", json!("RESOURCE_SEMAPHORE")),
        ("/request/wait", Value::Null),
        ("/request/key/request/generation", json!("another-native-request-generation")),
        ("/resource/qualifiers/resource_semaphore_id", json!("1")),
        ("/observed_at", json!("2020-01-01T12:00:00.000000006Z")),
        ("/granted_kibibytes", json!("9007199254740992")),
    ] {
        let (source, records) = test_support::output_wait_grant_evidence();
        let family = EvidenceFamily::OperationalSamples;
        let fixture = collected(source, records, family).await;
        let schema = fixture.catalog.get(family.stream()).unwrap();
        let column = schema.columns().iter().position(|field| field.name == "Details").unwrap();
        let mut rows = fixture.rows.clone();
        let mut details: Value = serde_json::from_str(rows[0][column].as_str().unwrap()).unwrap();
        *details.pointer_mut(pointer).unwrap() = value;
        rows[0][column] = json!(serde_json::to_string(&details).unwrap());
        fixture.replies(vec![rows]);
        assert_projection_rejected(&fixture, family).await;
    }
}

#[tokio::test]
async fn missing_output_counter_or_grant_holder_remains_an_original_counted_gap_without_hydration_or_replacement() {
    for (source, records, family) in cases() {
        let fixture = collected(source.clone(), records.clone(), family).await;
        let mut rows = fixture.rows.clone();
        rows.remove(0);
        fixture.replies(vec![rows]);
        *fixture.recovery.fault.lock().unwrap() = Fault::None;
        let opened = fixture.recovery.object_reads.load(Ordering::SeqCst);
        let capture = fixture
            .reader
            .read_capture(
                &fixture.range,
                &[family],
                EvidenceReadBudget { recovery_payloads: Some(RecoveryPayloadBudget::default()), ..Default::default() },
                Some(fixture.recovery.as_ref()),
            )
            .await
            .unwrap();
        assert!(!capture.is_read_complete());
        assert_eq!(capture.source(), &source);
        assert_eq!(capture.captured_rows(family.stream()), Some(records.len() as u32));
        assert_eq!(capture.records().len(), records.len() - 1);
        assert!(capture.gaps().iter().any(|gap| gap.reason == EvidenceGapReason::MissingRows));
        assert_eq!(
            capture.source().observation.snapshot().unwrap().sources[0].state,
            culprit_core::SourceState::Collected
        );
        if family == EvidenceFamily::OperationalSamples {
            let surviving = &capture.records()[0];
            assert_eq!(surviving.row_ordinal(), 2);
            assert_eq!(surviving.row(), &ExportedRecord::new(&source, capture.capture(), records[1].clone()).unwrap());
            let TelemetryRecord::Operational(OperationalObservation {
                fact: OperationalFact::ExecutionGrant(waiter),
                ..
            }) = &surviving.row().record
            else {
                panic!("retained native waiter")
            };
            assert_eq!(waiter.state, ExecutionGrantState::Waiting);
            assert_eq!(waiter.granted_kibibytes.as_deref(), Some("0"));
            let wait = waiter.request.as_ref().unwrap().wait.as_ref().unwrap();
            assert_eq!(wait.native_type.as_deref(), Some("RESOURCE_SEMAPHORE"));
            assert!(wait.owners.is_empty());
        } else {
            assert!(capture.records().is_empty());
        }
        assert_eq!(capture.metrics().queries, 3, "publication, audit and one bounded page; no retry or hydration");
        assert_eq!(capture.metrics().pages, 1);
        assert_eq!(capture.metrics().recovery_objects, 0);
        assert_eq!(fixture.query.requests.lock().unwrap().len(), 3);
        assert!(fixture.query.replies.lock().unwrap().is_empty());
        assert_eq!(fixture.recovery.object_reads.load(Ordering::SeqCst), opened);
    }
}

async fn assert_projection_rejected(fixture: &Fixture, family: EvidenceFamily) {
    *fixture.recovery.fault.lock().unwrap() = Fault::None;
    let opened = fixture.recovery.object_reads.load(Ordering::SeqCst);
    let error = fixture
        .reader
        .read_capture(
            &fixture.range,
            &[family],
            EvidenceReadBudget { recovery_payloads: Some(RecoveryPayloadBudget::default()), ..Default::default() },
            Some(fixture.recovery.as_ref()),
        )
        .await
        .unwrap_err();
    assert_eq!(error.detail, Some(culprit_core::ErrorDetail::Delivery(DeliveryProblem::Integrity)));
    assert_eq!(fixture.recovery.object_reads.load(Ordering::SeqCst), opened);
    assert_eq!(fixture.query.requests.lock().unwrap().len(), 3);
    assert!(fixture.query.replies.lock().unwrap().is_empty());
}
