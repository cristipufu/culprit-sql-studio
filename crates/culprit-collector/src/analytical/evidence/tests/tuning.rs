use super::*;

#[tokio::test]
async fn tuning_fact_decoder_restores_exact_scopes_definitions_counts_and_unavailable_bodies() {
    let catalog = LogicalCatalog::new().unwrap();
    for (_, source, records) in test_support::tuning_evidence_cases() {
        let recovery =
            test_support::Recovery::capture("fixture:test", catalog.digest(), source, records.clone(), 180_000);
        let range = reopen(&recovery, &recovery.manifest.range_id).await.unwrap();
        let batch = &range.manifest().batches[0];
        let schema = catalog.get(batch.stream).unwrap();
        for (index, record) in records.into_iter().enumerate() {
            let row = ExportedRecord::new(&range.manifest().source, &range.manifest().capture, record).unwrap();
            let input = Envelope::committed(&range, &batch.batch_id).unwrap().input(index + 1, &row).unwrap();
            let values = schema
                .fields()
                .iter()
                .zip(schema.project(&input).unwrap())
                .map(|(field, value)| (field.name.into(), value))
                .collect();
            let decode::Decoded::Record(decoded) =
                decode::record(&range, batch.stream, &values, &batch.rows[index]).unwrap()
            else {
                panic!("inline native tuning facts are reconstructible")
            };
            assert_eq!(*decoded, row);
            let mut wrong_key = values;
            wrong_key.insert("FactKey".into(), serde_json::json!("forced:3:9:replica:99"));
            assert!(
                decode::record(&range, batch.stream, &wrong_key, &batch.rows[index]).is_err(),
                "native scoped keys must be enforced before accepting a row"
            );
        }
    }
}

#[tokio::test]
async fn tuning_payload_previews_require_recovery_and_conflicting_payload_pointers_are_rejected() {
    let catalog = LogicalCatalog::new().unwrap();
    let (source, records) = test_support::tuning_evidence(test_support::TuningCase::LargeDefinition);
    let recovery = test_support::Recovery::capture("fixture:test", catalog.digest(), source, records.clone(), 180_001);
    let range = reopen(&recovery, &recovery.manifest.range_id).await.unwrap();
    let batch = &range.manifest().batches[0];
    let row = ExportedRecord::new(&range.manifest().source, &range.manifest().capture, records[1].clone()).unwrap();
    let schema = catalog.get(batch.stream).unwrap();
    let input = Envelope::committed(&range, &batch.batch_id).unwrap().input(2, &row).unwrap();
    let mut values: serde_json::Map<_, _> = schema
        .fields()
        .iter()
        .zip(schema.project(&input).unwrap())
        .map(|(field, value)| (field.name.into(), value))
        .collect();
    assert!(matches!(
        decode::record(&range, batch.stream, &values, &batch.rows[1]).unwrap(),
        decode::Decoded::Unavailable(EvidenceGapReason::DetailInRecovery)
    ));
    values.get_mut("DefinitionPayloads").unwrap()[0]["pointer"] =
        serde_json::json!("/record/data/fact/data/not-the-script");
    assert!(decode::record(&range, batch.stream, &values, &batch.rows[1]).is_err());
}
