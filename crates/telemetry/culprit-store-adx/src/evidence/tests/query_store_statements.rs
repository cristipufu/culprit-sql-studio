use super::*;
use std::sync::atomic::Ordering;

const RUNTIME: EvidenceFamily = EvidenceFamily::QueryRuntime;
const DEFINITIONS: EvidenceFamily = EvidenceFamily::QueryDefinitions;

async fn fixture(source: CollectionDescription, records: Vec<TelemetryRecord>, families: &[EvidenceFamily]) -> Fixture {
    let _ = families;
    Fixture::new(source, records).await
}
fn reply(fixture: &Fixture, families: &[EvidenceFamily]) {
    fixture.reply_families(families, 250);
}
async fn read(
    fixture: &Fixture,
    families: &[EvidenceFamily],
    recovery: Option<RecoveryPayloadBudget>,
) -> EvidenceCapture {
    try_read(fixture, families, recovery).await.unwrap()
}
async fn try_read(
    fixture: &Fixture,
    families: &[EvidenceFamily],
    recovery: Option<RecoveryPayloadBudget>,
) -> culprit_core::Result<EvidenceCapture> {
    reply(fixture, families);
    let options = EvidenceReadBudget { recovery_payloads: recovery, timeout_ms: 1000, ..Default::default() };
    fixture
        .reader()
        .read_capture(
            &fixture.range,
            families,
            options,
            recovery.map(|_| fixture.recovery.as_ref() as &dyn culprit_collector::delivery::recovery::RecoveryStore),
        )
        .await
}

#[tokio::test]
async fn runtime_and_companion_cases_preserve_separate_original_rows_scopes_and_clocks_without_recovery() {
    for case in test_support::query_store_statement_evidence_cases() {
        let unicode = case.name == "short_unicode_sql";
        for (is_definition, (source, records)) in
            std::iter::once((false, case.runtime)).chain(case.definitions.map(|pair| (true, pair)))
        {
            let family = if is_definition { DEFINITIONS } else { RUNTIME };
            let fixture = fixture(source.clone(), records.clone(), &[family]).await;
            let opens = fixture.recovery.object_reads.load(Ordering::SeqCst);
            assert!(matches!(*fixture.recovery.fault.lock().unwrap(), Fault::Stall));
            let capture = read(&fixture, &[family], None).await;
            if unicode && is_definition {
                assert!(capture.records().is_empty());
                assert!(!capture.is_read_complete());
                assert!(capture.gaps().iter().any(|gap| gap.reason == EvidenceGapReason::DetailInRecovery));
            } else {
                test_support::assert_query_cohort_capture(&capture, &source, &records);
            }
            assert_eq!(capture.source(), &source, "{}", case.name);
            assert_eq!(capture.metrics().selected_rows, 1);
            assert_eq!(capture.metrics().recovery_objects, 0);
            assert_eq!(fixture.recovery.object_reads.load(Ordering::SeqCst), opens);
            if !is_definition {
                let TelemetryRecord::QueryRuntime(native) = &capture.records()[0].row().record else { panic!() };
                assert_eq!(native.executions, "17");
                assert_eq!(native.page_server_reads.as_ref().unwrap().last_pages.as_deref(), Some("9007199254740993"));
                let original = records
                    .iter()
                    .find_map(|row| if let TelemetryRecord::QueryRuntime(row) = row { Some(row) } else { None })
                    .unwrap();
                assert_eq!(native.key, original.key);
            }
        }
    }
}

#[tokio::test]
async fn explicit_same_capture_family_selection_keeps_both_original_streams_without_hydration() {
    let case =
        test_support::query_store_statement_evidence_cases().into_iter().find(|case| case.name == "matched").unwrap();
    let (source, records) = case.runtime;
    let fixture = fixture(source.clone(), records.clone(), &[RUNTIME, DEFINITIONS]).await;
    let opens = fixture.recovery.object_reads.load(Ordering::SeqCst);
    let capture = read(&fixture, &[RUNTIME, DEFINITIONS], None).await;
    test_support::assert_query_cohort_capture(&capture, &source, &records);
    assert_eq!(capture.metrics().selected_rows, 2);
    assert_eq!(capture.captured_rows(TelemetryStream::QueryRuntime), Some(1));
    assert_eq!(capture.captured_rows(TelemetryStream::QueryDefinitions), Some(1));
    assert!(capture.records().iter().all(|record| &record.row().capture == capture.capture()));
    assert_eq!(fixture.recovery.object_reads.load(Ordering::SeqCst), opens);
}

#[tokio::test]
async fn changed_native_query_incarnation_cannot_borrow_the_original_runtime_receipt() {
    let case =
        test_support::query_store_statement_evidence_cases().into_iter().find(|case| case.name == "matched").unwrap();
    let fixture = fixture(case.runtime.0, case.runtime.1, &[RUNTIME]).await;
    let opens = fixture.recovery.object_reads.load(Ordering::SeqCst);
    let schema = fixture.catalog.get(TelemetryStream::QueryRuntime).unwrap();
    let column = schema.columns().iter().position(|column| column.name == "NativeReferences").unwrap();
    for (field, value) in [
        ("context_settings_id", "999"),
        ("initial_compile_at", "2020-01-01T07:00:00Z"),
        ("query_hash", "FEDCBA9876543210"),
    ] {
        let mut rows = fixture.rows_for(RUNTIME);
        let mut native: Value = serde_json::from_str(rows[0][column].as_str().unwrap()).unwrap();
        native["query"]["native"]["qualifiers"][field] = json!(value);
        rows[0][column] = json!(native.to_string());
        fixture.replies(vec![rows]);
        let error = fixture
            .reader()
            .read_capture(
                &fixture.range,
                &[RUNTIME],
                EvidenceReadBudget { timeout_ms: 1000, ..Default::default() },
                None,
            )
            .await
            .unwrap_err();
        assert_eq!(error.detail, Some(culprit_core::ErrorDetail::Delivery(DeliveryProblem::Integrity)), "{field}");
        assert_eq!(fixture.recovery.object_reads.load(Ordering::SeqCst), opens);
    }
}

#[tokio::test]
async fn short_unicode_sql_keeps_an_explicit_gap_until_the_exact_original_object_budget_is_admitted() {
    let case = test_support::query_store_statement_evidence_cases()
        .into_iter()
        .find(|case| case.name == "short_unicode_sql")
        .unwrap();
    let (source, records) = case.definitions.unwrap();
    let fixture = fixture(source.clone(), records.clone(), &[DEFINITIONS]).await;
    let opens = fixture.recovery.object_reads.load(Ordering::SeqCst);
    let preview = read(&fixture, &[DEFINITIONS], None).await;
    assert!(!preview.is_read_complete());
    assert!(preview.records().is_empty());
    assert_eq!(fixture.recovery.object_reads.load(Ordering::SeqCst), opens);
    *fixture.recovery.fault.lock().unwrap() = Fault::None;
    let bytes = fixture
        .range
        .manifest()
        .batches
        .iter()
        .find(|batch| batch.stream == TelemetryStream::QueryDefinitions)
        .unwrap()
        .bytes;
    let error =
        try_read(&fixture, &[DEFINITIONS], Some(RecoveryPayloadBudget { max_objects: 1, max_bytes: bytes - 1 }))
            .await
            .unwrap_err();
    assert_eq!(error.detail, Some(culprit_core::ErrorDetail::Delivery(DeliveryProblem::Capacity)));
    assert_eq!(
        fixture.recovery.object_reads.load(Ordering::SeqCst),
        opens,
        "immutable native object bytes are admitted before any body download"
    );
    let hydrated =
        read(&fixture, &[DEFINITIONS], Some(RecoveryPayloadBudget { max_objects: 1, max_bytes: bytes })).await;
    test_support::assert_query_cohort_capture(&hydrated, &source, &records);
    let TelemetryRecord::QueryDefinition(native) = &hydrated.records()[0].row().record else { panic!() };
    let DefinitionPayload::Available { text, .. } = &native.sql else { panic!() };
    assert!(text.chars().count() < 512 && text.len() > 1024);
    assert_eq!(hydrated.metrics().recovery_objects, 1);
    assert_eq!(fixture.recovery.object_reads.load(Ordering::SeqCst), opens + 1);
}
