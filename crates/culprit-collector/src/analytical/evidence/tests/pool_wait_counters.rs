use super::*;
#[tokio::test]
async fn curated_wait_context_cannot_replace_the_original_pool_authority_and_all_native_rows_reconstruct_exactly() {
    let catalog = LogicalCatalog::new().unwrap();
    for case in test_support::PoolWaitCounterCase::ALL {
        let (source, records) = test_support::pool_wait_counter_evidence(case);
        let recovery = test_support::Recovery::capture(
            "fixture:pool-waits",
            catalog.digest(),
            source.clone(),
            records.clone(),
            80_000,
        );
        let range = reopen(&recovery, &recovery.manifest.range_id).await.unwrap();
        assert_eq!(range.manifest().source, source);
        for batch in &range.manifest().batches {
            let schema = catalog.get(batch.stream).unwrap();
            for (index, record) in records.iter().enumerate() {
                let original = ExportedRecord::new(&source, &range.manifest().capture, record.clone()).unwrap();
                let input = Envelope::committed(&range, &batch.batch_id).unwrap().input(index + 1, &original).unwrap();
                let values: serde_json::Map<String, serde_json::Value> = schema
                    .fields()
                    .iter()
                    .zip(schema.project(&input).unwrap())
                    .map(|(field, value)| (field.name.into(), value))
                    .collect();
                let decode::Decoded::Record(decoded) =
                    decode::record(&range, batch.stream, &values, &batch.rows[index]).unwrap()
                else {
                    panic!()
                };
                assert_eq!(decoded.record, *record);
                assert_eq!(decoded.payload_sha256().unwrap(), original.payload_sha256().unwrap());
                let encoded = serde_json::to_string(&values).unwrap();
                assert!(!encoded.contains("logical_database_guid") && !encoded.contains("physical_database_guid"));
                let mut changed = values;
                changed.insert("WaitTimeMs".into(), serde_json::json!("9007199254740994"));
                assert!(matches!(
                    decode::record(&range, batch.stream, &changed, &batch.rows[index]).unwrap(),
                    decode::Decoded::Unavailable(EvidenceGapReason::NativeRepresentationRequired)
                ));
            }
        }
    }
}
