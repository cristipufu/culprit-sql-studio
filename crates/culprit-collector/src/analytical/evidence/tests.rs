use super::*;
use crate::{
    analytical::{catalog::LogicalCatalog, envelope::Envelope, test_support},
    delivery::recovery::reopen,
};
use culprit_core::telemetry::*;

mod activity;
mod diagnostics;
mod empty;

#[tokio::test]
async fn query_reconstruction_keeps_native_closure_outcomes_units_and_redacted_payloads_exact() {
    let catalog = LogicalCatalog::new().unwrap();
    let (source, records) = test_support::query_evidence();
    let recovery = test_support::Recovery::capture("fixture:test", catalog.digest(), source, records.clone(), 90_000);
    let range = reopen(&recovery, &recovery.manifest.range_id).await.unwrap();
    for batch in &range.manifest().batches {
        let schema = catalog.get(batch.stream).unwrap();
        for (index, record) in records.iter().filter(|record| record.stream() == batch.stream).enumerate() {
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
                panic!("the canonical query evidence is reconstructible")
            };
            assert_eq!(serde_json::to_value(decoded).unwrap(), serde_json::to_value(row).unwrap());
        }
    }
}

#[test]
fn supported_families_declare_distinct_native_streams() {
    for (index, family) in EvidenceFamily::ALL.iter().enumerate() {
        assert!(EvidenceFamily::ALL[..index].iter().all(|previous| previous.stream() != family.stream()));
    }
}

#[tokio::test]
async fn inline_grant_payloads_follow_the_typed_inventory_and_conflicting_copied_references_are_rejected() {
    let catalog = LogicalCatalog::new().unwrap();
    let (source, records) = test_support::request_memory_evidence(
        DefinitionPayload::sql("SELECT payload FROM dbo.orders WHERE customer_id=9264", culprit_core::Engine::Mssql),
        true,
    );
    let recovery = test_support::Recovery::capture("fixture:test", catalog.digest(), source, records.clone(), 120_000);
    let range = reopen(&recovery, &recovery.manifest.range_id).await.unwrap();
    let batch = &range.manifest().batches[0];
    let schema = catalog.get(batch.stream).unwrap();
    let row = ExportedRecord::new(&range.manifest().source, &range.manifest().capture, records[0].clone()).unwrap();
    let pointer = row.record.definition_payloads().next().unwrap().pointer;
    let input = Envelope::committed(&range, &batch.batch_id).unwrap().input(1, &row).unwrap();
    let values: serde_json::Map<_, _> = schema
        .fields()
        .iter()
        .zip(schema.project(&input).unwrap())
        .map(|(field, value)| (field.name.into(), value))
        .collect();
    let decode::Decoded::Record(decoded) = decode::record(&range, batch.stream, &values, &batch.rows[0]).unwrap()
    else {
        panic!("complete available nested payload")
    };
    assert_eq!(*decoded, row);
    for outside in [false, true] {
        let mut conflict = values.clone();
        if outside {
            conflict.get_mut("DefinitionPayloads").unwrap()[0]["pointer"] = serde_json::json!("/record/data/key");
        } else {
            let suffix = pointer.strip_prefix("/record/data/fact/data").unwrap();
            conflict.get_mut("Details").unwrap().pointer_mut(suffix).unwrap()["preview"] =
                serde_json::json!("another statement");
        }
        assert!(decode::record(&range, batch.stream, &conflict, &batch.rows[0]).is_err());
    }
}

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
