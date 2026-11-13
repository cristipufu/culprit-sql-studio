use super::*;
use crate::{
    analytical::{catalog::LogicalCatalog, envelope::Envelope, test_support},
    delivery::recovery::reopen,
};
use culprit_core::telemetry::*;

#[tokio::test]
async fn typed_operational_reconstruction_preserves_counter_space_scope_epochs_and_unknown_fields() {
    let catalog = LogicalCatalog::new().unwrap();
    let recovery = test_support::shared_operational_capture("fixture:test", catalog.digest());
    let range = reopen(&recovery, &recovery.manifest.range_id).await.unwrap();
    let (_, records) = test_support::shared_operational();
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
            panic!("complete inline operational evidence")
        };
        assert_eq!(serde_json::to_value(decoded).unwrap(), serde_json::to_value(row).unwrap());
        if index == 0 {
            let mut future_kind = values.clone();
            future_kind.insert("FactKind".into(), serde_json::json!("future-native-fact"));
            assert!(matches!(
                decode::record(&range, batch.stream, &future_kind, &batch.rows[index]).unwrap(),
                decode::Decoded::Unavailable(EvidenceGapReason::NativeRepresentationRequired)
            ));
        }
    }
}
