use super::*;
use test_support::{
    TableFullTextCase, TableStorageCase, assert_table_metadata_capture, table_full_text_evidence,
    table_storage_evidence,
};
const FAMILY: &[EvidenceFamily] = &[EvidenceFamily::OperationalSamples];
#[tokio::test]
async fn original_full_text_catalog_states_round_trip_without_recovery_or_current_metadata() {
    for (source, records) in TableFullTextCase::ALL
        .into_iter()
        .map(table_full_text_evidence)
        .chain(TableStorageCase::ALL.into_iter().map(table_storage_evidence))
    {
        let fixture =
            Fixture::new_for_stream(source.clone(), records.clone(), TelemetryStream::OperationalSamples).await;
        fixture.replies(fixture.rows.iter().map(|row| vec![row.clone()]).collect());
        let capture = fixture
            .reader
            .read_capture(&fixture.range, FAMILY, EvidenceReadBudget { page_rows: 1, ..Default::default() }, None)
            .await
            .unwrap();
        assert_table_metadata_capture(&capture, &source, &records);
    }
}
#[tokio::test]
async fn changed_enabled_flag_cannot_borrow_an_original_table_row_receipt() {
    let (source, records) = table_full_text_evidence(TableFullTextCase::Enabled);
    let fixture = Fixture::new_for_stream(source.clone(), records.clone(), TelemetryStream::OperationalSamples).await;
    let schema = fixture.catalog.get(TelemetryStream::OperationalSamples).unwrap();
    let column = schema.columns().iter().position(|column| column.name == "Details").unwrap();
    let mut rows = fixture.rows.clone();
    let mut details: Value = serde_json::from_str(rows[0][column].as_str().unwrap()).unwrap();
    details["full_text"]["index"]["enabled"] = json!(false);
    rows[0][column] = json!(serde_json::to_string(&details).unwrap());
    fixture.replies(vec![rows]);
    let error =
        fixture.reader.read_capture(&fixture.range, FAMILY, EvidenceReadBudget::default(), None).await.unwrap_err();
    assert_eq!(error.detail, Some(culprit_core::ErrorDetail::Delivery(DeliveryProblem::Integrity)));
}

#[tokio::test]
async fn a_changed_durability_pair_cannot_borrow_the_original_native_table_receipt() {
    let (source, records) = table_storage_evidence(TableStorageCase::SchemaOnlyMemoryOptimized);
    let fixture = Fixture::new_for_stream(source.clone(), records.clone(), TelemetryStream::OperationalSamples).await;
    let schema = fixture.catalog.get(TelemetryStream::OperationalSamples).unwrap();
    let column = schema.columns().iter().position(|column| column.name == "Details").unwrap();
    let mut rows = fixture.rows.clone();
    let mut details: Value = serde_json::from_str(rows[0][column].as_str().unwrap()).unwrap();
    details["storage_configuration"]["durability"] = json!(0);
    details["storage_configuration"]["durability_desc"] = json!("SCHEMA_AND_DATA");
    rows[0][column] = json!(serde_json::to_string(&details).unwrap());
    fixture.replies(vec![rows]);
    let error = fixture
        .reader
        .read_capture(&fixture.range, FAMILY, EvidenceReadBudget { page_rows: 1, ..Default::default() }, None)
        .await
        .unwrap_err();
    assert_eq!(error.detail, Some(culprit_core::ErrorDetail::Delivery(DeliveryProblem::Integrity)));
}
