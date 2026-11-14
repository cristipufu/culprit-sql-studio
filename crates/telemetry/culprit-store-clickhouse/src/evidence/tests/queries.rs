use super::*;

#[tokio::test]
async fn explicit_native_cohorts_preserve_every_definition_fragment_and_population_boundary() {
    for (case, source, records) in test_support::query_cohort_evidence_cases() {
        for family in [
            EvidenceFamily::QueryDefinitions,
            EvidenceFamily::PlanDefinitions,
            EvidenceFamily::QueryRuntime,
            EvidenceFamily::QueryWaits,
        ] {
            let fixture = Fixture::new_for_stream(source.clone(), records.clone(), family.stream()).await;
            fixture.replies(fixture.rows.iter().map(|row| vec![row.clone()]).collect());
            let capture = fixture
                .reader
                .read_capture(
                    &fixture.range,
                    &[family],
                    EvidenceReadBudget { page_rows: 1, ..Default::default() },
                    None,
                )
                .await
                .unwrap();
            test_support::assert_query_cohort_capture(&capture, &source, &records);
            assert_eq!(capture.metrics().pages as usize, fixture.rows.len(), "{case:?}/{family:?}");
            assert!(matches!(*fixture.recovery.fault.lock().unwrap(), Fault::Stall));
        }
    }
}

#[tokio::test]
async fn changing_or_removing_a_native_cohort_cannot_pass_the_original_row_receipt() {
    for family in [EvidenceFamily::QueryRuntime, EvidenceFamily::QueryWaits] {
        let (source, records) = test_support::query_cohort_evidence(test_support::QueryCohortCase::PlanVariation);
        let fixture = Fixture::new_for_stream(source, records, family.stream()).await;
        let schema = fixture.catalog.get(family.stream()).unwrap();
        let column = schema.columns().iter().position(|column| column.name == "NativeCohort").unwrap();
        let mut cohort: Value = serde_json::from_str(fixture.rows[0][column].as_str().unwrap()).unwrap();
        cohort["qualifiers"]["replica_group_id"] = json!("1");
        for changed in [json!(serde_json::to_string(&cohort).unwrap()), Value::Null] {
            let mut rows = fixture.rows.clone();
            rows[0][column] = changed;
            fixture.replies(vec![rows]);
            let error = fixture
                .reader
                .read_capture(&fixture.range, &[family], EvidenceReadBudget::default(), None)
                .await
                .unwrap_err();
            assert_eq!(error.detail, Some(culprit_core::ErrorDetail::Delivery(DeliveryProblem::Integrity)));
        }
    }
}
