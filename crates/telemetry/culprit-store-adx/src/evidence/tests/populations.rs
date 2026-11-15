use super::*;

const FAMILIES: &[EvidenceFamily] = &[
    EvidenceFamily::QueryDefinitions,
    EvidenceFamily::PlanDefinitions,
    EvidenceFamily::QueryRuntime,
    EvidenceFamily::QueryWaits,
];

#[tokio::test]
async fn source_population_bindings_and_every_original_group_are_read_once_without_native_recovery() {
    for (source, records) in test_support::query_population_evidence_cases() {
        let fixture = Fixture::new(source.clone(), records.clone()).await;
        fixture.reply_families(FAMILIES, 1);
        let before = fixture.recovery.object_reads.load(std::sync::atomic::Ordering::SeqCst);
        let capture = fixture
            .reader()
            .read_capture(&fixture.range, FAMILIES, EvidenceReadBudget { page_rows: 1, ..budget() }, None)
            .await
            .unwrap();
        test_support::assert_query_population_capture(&capture, &source, &records);
        assert_eq!(capture.metrics().pages as usize, records.len());
        assert_eq!(fixture.recovery.object_reads.load(std::sync::atomic::Ordering::SeqCst), before);
        assert!(matches!(*fixture.recovery.fault.lock().unwrap(), Fault::Stall));
        let requests = fixture.query.requests.lock().unwrap();
        assert_eq!(requests.len(), records.len() + 2);
        assert!(requests.iter().all(|request| !request.text.contains("native-routing-class")));
    }
}

#[tokio::test]
async fn published_population_catalog_or_tuple_changes_fail_before_any_measurement_query() {
    for pointer in [
        "/observation/data/population_binding/bindings/0/population/qualifiers/native-generation",
        "/observation/data/population_binding/bindings/0/qualifiers/native-routing-class",
        "/observation/data/population_catalog/populations/0/identity/id",
    ] {
        let (source, records) = test_support::query_population_evidence();
        let mut fixture = Fixture::new(source, records).await;
        let column = fixture
            .catalog
            .publication()
            .columns()
            .iter()
            .position(|column| column.name == "SourceDescription")
            .unwrap();
        let mut description: Value = serde_json::from_str(fixture.publication[column].as_str().unwrap()).unwrap();
        *description.pointer_mut(pointer).unwrap() = json!("counterfeit-native-binding");
        fixture.publication[column] = json!(serde_json::to_string(&description).unwrap());
        fixture.replies(Vec::new());
        let error = fixture.reader().read_capture(&fixture.range, FAMILIES, budget(), None).await.unwrap_err();
        assert_eq!(error.detail, Some(culprit_core::ErrorDetail::Delivery(DeliveryProblem::Integrity)), "{pointer}");
        assert_eq!(fixture.query.requests.lock().unwrap().len(), 1);
        assert!(matches!(*fixture.recovery.fault.lock().unwrap(), Fault::Stall));
    }
}

#[tokio::test]
async fn a_measurement_cannot_be_moved_to_another_declared_group_under_the_original_row_receipt() {
    for family in [EvidenceFamily::QueryRuntime, EvidenceFamily::QueryWaits] {
        let (source, records) = test_support::query_population_evidence();
        let fixture = Fixture::new(source, records).await;
        let schema = fixture.catalog.get(family.stream()).unwrap();
        let column = schema.columns().iter().position(|column| column.name == "NativeDimensions").unwrap();
        let mut rows = fixture.rows_for(family);
        let mut dimensions: Value = serde_json::from_str(rows[0][column].as_str().unwrap()).unwrap();
        dimensions["native-routing-class"] = json!("native-value-east");
        rows[0][column] = json!(serde_json::to_string(&dimensions).unwrap());
        fixture.replies(vec![rows]);
        let error = fixture.reader().read_capture(&fixture.range, &[family], budget(), None).await.unwrap_err();
        assert_eq!(error.detail, Some(culprit_core::ErrorDetail::Delivery(DeliveryProblem::Integrity)));
        assert!(matches!(*fixture.recovery.fault.lock().unwrap(), Fault::Stall));
    }
}

#[tokio::test]
async fn missing_warehouse_measurements_leave_native_group_discovery_intact_and_read_coverage_unknown() {
    let (source, records) = test_support::query_population_evidence();
    let fixture = Fixture::new(source.clone(), records).await;
    let mut rows = fixture.rows_for(EvidenceFamily::QueryRuntime);
    rows.remove(1);
    fixture.replies(vec![rows]);
    let capture =
        fixture.reader().read_capture(&fixture.range, &[EvidenceFamily::QueryRuntime], budget(), None).await.unwrap();
    assert_eq!(capture.source(), &source);
    assert!(!capture.is_read_complete());
    assert_eq!(capture.records().len(), 1);
    assert_eq!(capture.gaps().len(), 1);
    assert_eq!(capture.gaps()[0].reason, EvidenceGapReason::MissingRows);
    assert_eq!(capture.gaps()[0].row_ordinal, Some(2));
    assert_eq!(capture.source().query_history().unwrap().population_catalog.as_ref().unwrap().populations.len(), 2);
    assert_eq!(capture.metrics().recovery_objects, 0);
    assert!(matches!(*fixture.recovery.fault.lock().unwrap(), Fault::Stall));
}
