use super::*;

#[tokio::test]
async fn managed_instance_interval_projection_keeps_exact_totals_and_authenticated_original_clock() {
    let catalog = LogicalCatalog::new().unwrap();
    let (source, records) = test_support::managed_instance_resource_evidence();
    let recovery =
        test_support::Recovery::capture("fixture:mi", catalog.digest(), source.clone(), records.clone(), 90_123);
    let range = reopen(&recovery, &recovery.manifest.range_id).await.unwrap();
    let batch = &range.manifest().batches[0];
    let schema = catalog.get(batch.stream).unwrap();
    let row = ExportedRecord::new(&source, &range.manifest().capture, records[0].clone()).unwrap();
    let input = Envelope::committed(&range, &batch.batch_id).unwrap().input(1, &row).unwrap();
    let values: serde_json::Map<String, serde_json::Value> = schema
        .fields()
        .iter()
        .zip(schema.project(&input).unwrap())
        .map(|(field, value)| (field.name.into(), value))
        .collect();
    assert_eq!(values["ManagedInstanceInterval"]["io_request"], "9007199254740993");
    assert_eq!(values["ManagedInstanceInterval"]["storage_space_used_mb"], "9007199254740992.25");
    let decode::Decoded::Record(decoded) = decode::record(&range, batch.stream, &values, &batch.rows[0]).unwrap()
    else {
        panic!()
    };
    assert_eq!(serde_json::to_value(decoded).unwrap(), serde_json::to_value(row).unwrap());
    let mut altered = values;
    altered["ManagedInstanceInterval"]["io_bytes_written"] = serde_json::json!("1");
    assert!(matches!(
        decode::record(&range, batch.stream, &altered, &batch.rows[0]).unwrap(),
        decode::Decoded::Unavailable(EvidenceGapReason::NativeRepresentationRequired)
    ));
}
