use super::*;
#[tokio::test]
async fn shared_settings_projection_restores_all_nullable_native_fields_and_original_pool_header() {
    let catalog = LogicalCatalog::new().unwrap();
    for case in test_support::PoolSettingsCase::ALL {
        let (source, records) = test_support::pool_settings_evidence(case);
        let recovery = test_support::Recovery::capture(
            "fixture:pool-settings",
            catalog.digest(),
            source.clone(),
            records.clone(),
            80_000,
        );
        let range = reopen(&recovery, &recovery.manifest.range_id).await.unwrap();
        assert_eq!(range.manifest().source, source);
        for batch in &range.manifest().batches {
            let schema = catalog.get(batch.stream).unwrap();
            assert_eq!(batch.stream, TelemetryStream::SettingObservations);
            for (index, record) in records.iter().enumerate() {
                let original = ExportedRecord::new(&source, &range.manifest().capture, record.clone()).unwrap();
                let input = Envelope::committed(&range, &batch.batch_id).unwrap().input(index + 1, &original).unwrap();
                let values = schema
                    .fields()
                    .iter()
                    .zip(schema.project(&input).unwrap())
                    .map(|(field, value)| (field.name.into(), value))
                    .collect();
                let decode::Decoded::Record(decoded) =
                    decode::record(&range, batch.stream, &values, &batch.rows[index]).unwrap()
                else {
                    panic!("exact native Settings representation")
                };
                assert_eq!(decoded.record, *record);
                assert_eq!(decoded.payload_sha256().unwrap(), original.payload_sha256().unwrap());
            }
        }
    }
}
#[tokio::test]
async fn changed_setting_keeps_native_shape_valid_but_cannot_reuse_the_original_row_receipt() {
    let catalog = LogicalCatalog::new().unwrap();
    let (source, records) = test_support::pool_settings_evidence(test_support::PoolSettingsCase::Exact);
    let recovery = test_support::Recovery::capture(
        "fixture:pool-settings",
        catalog.digest(),
        source.clone(),
        records.clone(),
        80_000,
    );
    let range = reopen(&recovery, &recovery.manifest.range_id).await.unwrap();
    let batch = &range.manifest().batches[0];
    let schema = catalog.get(batch.stream).unwrap();
    let row = ExportedRecord::new(&source, &range.manifest().capture, records[0].clone()).unwrap();
    let input = Envelope::committed(&range, &batch.batch_id).unwrap().input(1, &row).unwrap();
    let mut values: serde_json::Map<String, serde_json::Value> = schema
        .fields()
        .iter()
        .zip(schema.project(&input).unwrap())
        .map(|(field, value)| (field.name.into(), value))
        .collect();
    values.insert("ConfiguredValue".into(), serde_json::json!("1"));
    assert!(matches!(
        decode::record(&range, batch.stream, &values, &batch.rows[0]).unwrap(),
        decode::Decoded::Unavailable(EvidenceGapReason::NativeRepresentationRequired)
    ));
}
