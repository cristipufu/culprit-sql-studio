use super::*;

#[tokio::test]
async fn object_definitions_reconstruct_exact_native_versions_contexts_and_payload_availability() {
    let catalog = LogicalCatalog::new().unwrap();
    for (source, records) in test_support::object_definition_evidence_cases() {
        let recovery =
            test_support::Recovery::capture("fixture:test", catalog.digest(), source, records.clone(), 150_000);
        let range = reopen(&recovery, &recovery.manifest.range_id).await.unwrap();
        let batch =
            range.manifest().batches.iter().find(|batch| batch.stream == TelemetryStream::ObjectDefinitions).unwrap();
        let schema = catalog.get(batch.stream).unwrap();
        for (index, record) in records.iter().enumerate() {
            let row = ExportedRecord::new(&range.manifest().source, &range.manifest().capture, record.clone()).unwrap();
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
                panic!("inline typed definitions preserve exact native metadata and explicit absent payloads")
            };
            assert_eq!(*decoded, row);
            for column in ["DefinitionVersion", "FactKey"] {
                let mut forged = values.clone();
                forged.insert(column.into(), serde_json::json!(digest(b"foreign-definition")));
                assert!(
                    !matches!(
                        decode::record(&range, batch.stream, &forged, &batch.rows[index]),
                        Ok(decode::Decoded::Record(_))
                    ),
                    "{column} must match native definition and receipt"
                );
            }
            let mut unavailable = values.clone();
            unavailable.insert("DetailState".into(), serde_json::json!("recovery"));
            assert!(matches!(
                decode::record(&range, batch.stream, &unavailable, &batch.rows[index]).unwrap(),
                decode::Decoded::Unavailable(EvidenceGapReason::DetailInRecovery)
            ));
        }
    }
}
