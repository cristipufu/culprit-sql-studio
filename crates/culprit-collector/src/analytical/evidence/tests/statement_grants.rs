use super::*;

#[tokio::test]
async fn cached_grant_facts_use_the_shared_payload_inventory_decoder_and_original_native_receipts() {
    let catalog = LogicalCatalog::new().unwrap();
    for (_, source, records) in test_support::statement_grant_evidence_cases() {
        let recovery =
            test_support::Recovery::capture("fixture:test", catalog.digest(), source, records.clone(), 190_000);
        let range = reopen(&recovery, &recovery.manifest.range_id).await.unwrap();
        for batch in &range.manifest().batches {
            let schema = catalog.get(batch.stream).unwrap();
            for (index, record) in records.iter().enumerate() {
                let row =
                    ExportedRecord::new(&range.manifest().source, &range.manifest().capture, record.clone()).unwrap();
                let fields = row.record.definition_payloads().collect::<Vec<_>>();
                assert_eq!(fields.len(), 1);
                assert_eq!(fields[0].pointer, "/record/data/fact/data/statement");
                let input = Envelope::committed(&range, &batch.batch_id).unwrap().input(index + 1, &row).unwrap();
                assert!(input.pointer("/record/data/fact/data/statement/text").is_none());
                let values = schema
                    .fields()
                    .iter()
                    .zip(schema.project(&input).unwrap())
                    .map(|(field, value)| (field.name.into(), value))
                    .collect();
                let decode::Decoded::Record(decoded) =
                    decode::record(&range, batch.stream, &values, &batch.rows[index]).unwrap()
                else {
                    panic!("complete native grant payload")
                };
                assert_eq!(*decoded, row);
            }
        }
    }
}

#[tokio::test]
async fn cached_grant_text_is_redacted_at_export_and_utf8_previews_cannot_masquerade_as_complete_native_sql() {
    let catalog = LogicalCatalog::new().unwrap();
    let (source, mut records) = test_support::statement_grant_evidence(test_support::StatementGrantCase::Complete);
    let TelemetryRecord::Operational(observation) = &mut records[0] else { panic!() };
    let OperationalFact::MssqlStatementGrants(grant) = &mut observation.fact else { panic!() };
    let text = "SELECT value FROM dbo.grant_input WHERE id=9264".to_string();
    grant.statement =
        DefinitionPayload::Available { sha256: digest(text.as_bytes()), text, redaction_version: REDACTION_VERSION };
    let recovery = test_support::Recovery::capture("fixture:test", catalog.digest(), source, records.clone(), 190_001);
    let range = reopen(&recovery, &recovery.manifest.range_id).await.unwrap();
    let row = ExportedRecord::new(&range.manifest().source, &range.manifest().capture, records[0].clone()).unwrap();
    assert!(!serde_json::to_string(&row.record).unwrap().contains("9264"));
    let (source, records) = test_support::statement_grant_evidence(test_support::StatementGrantCase::LargeStatement);
    let recovery = test_support::Recovery::capture("fixture:test", catalog.digest(), source, records.clone(), 190_002);
    let range = reopen(&recovery, &recovery.manifest.range_id).await.unwrap();
    let batch = &range.manifest().batches[0];
    let row = ExportedRecord::new(&range.manifest().source, &range.manifest().capture, records[0].clone()).unwrap();
    let schema = catalog.get(batch.stream).unwrap();
    let input = Envelope::committed(&range, &batch.batch_id).unwrap().input(1, &row).unwrap();
    let mut values: serde_json::Map<_, _> = schema
        .fields()
        .iter()
        .zip(schema.project(&input).unwrap())
        .map(|(field, value)| (field.name.into(), value))
        .collect();
    assert!(matches!(
        decode::record(&range, batch.stream, &values, &batch.rows[0]).unwrap(),
        decode::Decoded::Unavailable(EvidenceGapReason::DetailInRecovery)
    ));
    values.get_mut("DefinitionPayloads").unwrap()[0]["pointer"] =
        serde_json::json!("/record/data/fact/data/not-the-statement");
    assert!(decode::record(&range, batch.stream, &values, &batch.rows[0]).is_err());
}
