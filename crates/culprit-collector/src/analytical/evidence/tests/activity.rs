use super::*;
use serde_json::{Map, Value, json};

#[tokio::test]
async fn original_activity_metadata_reconstructs_exactly_with_independent_coverage_and_payload_references() {
    let catalog = LogicalCatalog::new().unwrap();
    for (source, records) in test_support::activity_evidence_cases() {
        let recovery =
            test_support::Recovery::capture("fixture:test", catalog.digest(), source.clone(), records.clone(), 220_000);
        let range = reopen(&recovery, &recovery.manifest.range_id).await.unwrap();
        let batch = &range.manifest().batches[0];
        assert_eq!(batch.stream, TelemetryStream::ActivityObservations);
        assert_eq!(batch.rows.len(), records.len());
        let schema = catalog.get(batch.stream).unwrap();
        for (index, record) in records.into_iter().enumerate() {
            let exported = ExportedRecord::new(&source, &range.manifest().capture, record).unwrap();
            let input = Envelope::committed(&range, &batch.batch_id).unwrap().input(index + 1, &exported).unwrap();
            assert!(input["_projection"]["activity"]["statement"].get("text").is_none());
            let values = schema
                .fields()
                .iter()
                .zip(schema.project(&input).unwrap())
                .map(|(field, value)| (field.name.into(), value))
                .collect();
            let decode::Decoded::Record(actual) =
                decode::record(&range, batch.stream, &values, &batch.rows[index]).unwrap()
            else {
                panic!("exact bounded activity representation")
            };
            assert_eq!(*actual, exported);
            assert_eq!(actual.record.definition_payloads().next().unwrap().pointer, "/record/data/statement");
            assert!(!serde_json::to_string(&actual).unwrap().contains("private-activity-canary"));
        }
    }
}

#[tokio::test]
async fn activity_payload_inventory_rejects_duplicate_mislocated_and_conflicting_references() {
    let catalog = LogicalCatalog::new().unwrap();
    let (source, records) = test_support::activity_evidence_cases().remove(0);
    let recovery =
        test_support::Recovery::capture("fixture:test", catalog.digest(), source.clone(), records.clone(), 220_001);
    let range = reopen(&recovery, &recovery.manifest.range_id).await.unwrap();
    let batch = &range.manifest().batches[0];
    let schema = catalog.get(batch.stream).unwrap();
    let row = ExportedRecord::new(&source, &range.manifest().capture, records[0].clone()).unwrap();
    let input = Envelope::committed(&range, &batch.batch_id).unwrap().input(1, &row).unwrap();
    let values: Map<String, Value> = schema
        .fields()
        .iter()
        .zip(schema.project(&input).unwrap())
        .map(|(field, value)| (field.name.into(), value))
        .collect();
    for change in ["duplicate", "pointer", "copied-preview", "invalid-token"] {
        let mut changed = values.clone();
        match change {
            "duplicate" => {
                let payload = changed["DefinitionPayloads"][0].clone();
                changed.get_mut("DefinitionPayloads").unwrap().as_array_mut().unwrap().push(payload);
            }
            "pointer" => changed.get_mut("DefinitionPayloads").unwrap()[0]["pointer"] = json!("/record/data/key"),
            "copied-preview" => {
                changed.get_mut("ActivityFacts").unwrap()["statement"]["preview"] = json!("different native payload")
            }
            _ => {
                changed.get_mut("ActivityFacts").unwrap()["statement_identity"] = json!("invalid native identity shape")
            }
        }
        assert!(decode::record(&range, batch.stream, &changed, &batch.rows[0]).is_err(), "{change}");
    }
}

#[tokio::test]
async fn a_truncated_activity_statement_stays_recovery_required_instead_of_becoming_a_complete_original() {
    let catalog = LogicalCatalog::new().unwrap();
    let (mut source, mut records) = test_support::activity_evidence_cases().remove(0);
    let TelemetryRecord::Activity(row) = &mut records[0] else { panic!() };
    row.statement =
        DefinitionPayload::sql(&format!("SELECT {} FROM dbo.orders", "字段".repeat(1000)), culprit_core::Engine::Mssql);
    records.truncate(1);
    let SourceObservation::Activity(snapshot) = &mut source.observation else { panic!() };
    snapshot.native_rows = 1;
    snapshot.observations = 1;
    snapshot.coverage.records = 1;
    let recovery =
        test_support::Recovery::capture("fixture:test", catalog.digest(), source.clone(), records.clone(), 220_002);
    let range = reopen(&recovery, &recovery.manifest.range_id).await.unwrap();
    let batch = &range.manifest().batches[0];
    let exported = ExportedRecord::new(&source, &range.manifest().capture, records.remove(0)).unwrap();
    let input = Envelope::committed(&range, &batch.batch_id).unwrap().input(1, &exported).unwrap();
    let schema = catalog.get(batch.stream).unwrap();
    let values = schema
        .fields()
        .iter()
        .zip(schema.project(&input).unwrap())
        .map(|(field, value)| (field.name.into(), value))
        .collect();
    assert!(matches!(
        decode::record(&range, batch.stream, &values, &batch.rows[0]).unwrap(),
        decode::Decoded::Unavailable(EvidenceGapReason::DetailInRecovery)
    ));
}
