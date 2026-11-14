use super::*;
use crate::delivery::evidence::RowReceipt;
use serde_json::{Map, Value, json};

#[tokio::test]
async fn diagnostic_warehouse_details_restore_only_authenticated_target_and_match_exact_native_receipts() {
    let catalog = LogicalCatalog::new().unwrap();
    for (source, records) in test_support::diagnostic_evidence_cases() {
        let recovery =
            test_support::Recovery::capture("fixture:test", catalog.digest(), source.clone(), records.clone(), 230_000);
        let range = reopen(&recovery, &recovery.manifest.range_id).await.unwrap();
        let batch =
            range.manifest().batches.iter().find(|batch| batch.stream == TelemetryStream::OperationalSamples).unwrap();
        let original = ExportedRecord::new(&source, &range.manifest().capture, records[0].clone()).unwrap();
        let input = Envelope::committed(&range, &batch.batch_id).unwrap().input(1, &original).unwrap();
        let schema = catalog.get(batch.stream).unwrap();
        let values: Map<String, Value> = schema
            .fields()
            .iter()
            .zip(schema.project(&input).unwrap())
            .map(|(field, value)| (field.name.into(), value))
            .collect();
        assert!(values["Details"]["episode"]["scope"].get("target").is_none());
        let decode::Decoded::Record(decoded) = decode::record(&range, batch.stream, &values, &batch.rows[0]).unwrap()
        else {
            panic!("the exact target is reconstructed from authenticated private recovery authority")
        };
        assert_eq!(*decoded, original);
        assert_eq!(RowReceipt::from_export(&decoded).unwrap(), batch.rows[0]);
        for target in [serde_json::to_value(&source.target).unwrap(), json!({"kind":"database","database":"forged"})] {
            let mut injected = values.clone();
            injected.get_mut("Details").unwrap()["episode"]["scope"]["target"] = target;
            assert!(
                decode::record(&range, batch.stream, &injected, &batch.rows[0]).is_err(),
                "warehouse target injection is never trusted or silently overridden"
            );
        }
    }
}
