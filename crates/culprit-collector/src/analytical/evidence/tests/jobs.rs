use super::*;
use crate::analytical::test_support::jobs_evidence_cases;
#[tokio::test]
async fn jobs_reconstruct_from_the_existing_operational_projection_with_original_view_metadata_and_exact_facts() {
    let catalog = LogicalCatalog::new().unwrap();
    for (index, (source, records)) in jobs_evidence_cases()
        .into_iter()
        .chain(crate::analytical::test_support::mssql_agent_jobs_evidence_cases())
        .enumerate()
    {
        let recovery = test_support::Recovery::capture(
            "jobs:decode",
            catalog.digest(),
            source,
            records.clone(),
            320_000 + index as u64,
        );
        let range = reopen(&recovery, &recovery.manifest.range_id).await.unwrap();
        for batch in &range.manifest().batches {
            let schema = catalog.get(batch.stream).unwrap();
            assert_eq!(batch.stream, TelemetryStream::OperationalSamples);
            for (index, record) in records.iter().enumerate() {
                let expected =
                    ExportedRecord::new(&range.manifest().source, &range.manifest().capture, record.clone()).unwrap();
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
                    panic!("bounded native job facts stay inline")
                };
                assert_eq!(*actual, expected);
                if matches!(&expected.record, TelemetryRecord::Operational(OperationalObservation {
                    fact: OperationalFact::TimescaleJobState(point), ..
                }) if point.configuration_check.is_some())
                {
                    let mut altered = values.clone();
                    let details = altered.get_mut("Details").unwrap();
                    *details.pointer_mut("/configuration_check/name").unwrap() =
                        serde_json::json!("another_configuration_check");
                    let changed: TimescaleJobState = serde_json::from_value(details.clone()).unwrap();
                    assert!(changed.is_valid(), "the altered native label remains well formed");
                    assert_eq!(changed.key().unwrap(), altered["FactKey"].as_str().unwrap());
                    assert!(
                        matches!(
                            decode::record(&range, batch.stream, &altered, &batch.rows[index]).unwrap(),
                            decode::Decoded::Unavailable(EvidenceGapReason::NativeRepresentationRequired)
                        ),
                        "a valid changed label cannot borrow the original immutable native row receipt"
                    );
                }
            }
        }
    }
}
