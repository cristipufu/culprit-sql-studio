use super::*;
use crate::analytical::test_support::{ReplicationCase, replication_evidence, replication_evidence_cases};
use serde_json::json;

#[tokio::test]
async fn all_replication_fact_families_reconstruct_exact_original_records_from_shared_inline_projection() {
    let catalog = LogicalCatalog::new().unwrap();
    for (index, (source, records)) in replication_evidence_cases().into_iter().enumerate() {
        let recovery = test_support::Recovery::capture(
            "replication:decode",
            catalog.digest(),
            source,
            records.clone(),
            index as u64 + 300_000,
        );
        let range = reopen(&recovery, &recovery.manifest.range_id).await.unwrap();
        let batch = &range.manifest().batches[0];
        let schema = catalog.get(batch.stream).unwrap();
        assert_eq!(batch.stream, EvidenceFamily::ReplicationSamples.stream());
        for (index, record) in records.into_iter().enumerate() {
            let expected = ExportedRecord::new(&range.manifest().source, &range.manifest().capture, record).unwrap();
            let input = Envelope::committed(&range, &batch.batch_id).unwrap().input(index + 1, &expected).unwrap();
            let values = schema
                .fields()
                .iter()
                .zip(schema.project(&input).unwrap())
                .map(|(field, value)| (field.name.into(), value))
                .collect();
            let decode::Decoded::Record(actual) =
                decode::record(&range, batch.stream, &values, &batch.rows[index]).unwrap()
            else {
                panic!("bounded native replication facts are inline")
            };
            assert_eq!(*actual, expected);
        }
    }
}

#[tokio::test]
async fn fact_keys_are_derived_from_native_replication_identity_and_unknown_native_kinds_remain_gaps() {
    let catalog = LogicalCatalog::new().unwrap();
    for case in [ReplicationCase::PostgresSlots, ReplicationCase::MssqlAvailabilityGroup, ReplicationCase::MssqlGeoLink]
    {
        let (source, records) = replication_evidence(case);
        let recovery =
            test_support::Recovery::capture("replication:decode", catalog.digest(), source, records.clone(), 310_000);
        let range = reopen(&recovery, &recovery.manifest.range_id).await.unwrap();
        let batch = &range.manifest().batches[0];
        let schema = catalog.get(batch.stream).unwrap();
        let expected =
            ExportedRecord::new(&range.manifest().source, &range.manifest().capture, records[0].clone()).unwrap();
        let input = Envelope::committed(&range, &batch.batch_id).unwrap().input(1, &expected).unwrap();
        let values: serde_json::Map<_, _> = schema
            .fields()
            .iter()
            .zip(schema.project(&input).unwrap())
            .map(|(field, value)| (field.name.into(), value))
            .collect();
        let mut changed = values.clone();
        changed.insert("FactKey".into(), json!("different-native-key"));
        assert!(decode::record(&range, batch.stream, &changed, &batch.rows[0]).is_err());
        let mut changed = values;
        changed.insert("FactKind".into(), json!("future-replication-kind"));
        assert!(matches!(
            decode::record(&range, batch.stream, &changed, &batch.rows[0]).unwrap(),
            decode::Decoded::Unavailable(EvidenceGapReason::NativeRepresentationRequired)
        ));
    }
}
