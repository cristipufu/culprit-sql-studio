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

#[tokio::test]
async fn changed_wait_last_deviation_or_native_descriptions_cannot_pass_the_original_projection_receipt() {
    let (source, records) = test_support::query_wait_fragment_evidence();
    let fixture = Fixture::new_for_stream(source, records, TelemetryStream::QueryWaits).await;
    let schema = fixture.catalog.get(TelemetryStream::QueryWaits).unwrap();
    let original = fixture.rows.clone();
    for (name, changed) in [
        ("LastWait", json!(serde_json::to_string(&json!({"kind":"integer","value":"7"})).unwrap())),
        ("WaitStandardDeviation", json!(99.25)),
        ("WaitCategoryDescription", json!("Different native category")),
        ("ExecutionOutcomeDescription", Value::Null),
    ] {
        let column = schema.columns().iter().position(|column| column.name == name).unwrap();
        let mut rows = original.clone();
        rows[0][column] = changed;
        fixture.replies(vec![rows]);
        let error = fixture
            .reader
            .read_capture(&fixture.range, &[EvidenceFamily::QueryWaits], EvidenceReadBudget::default(), None)
            .await
            .unwrap_err();
        assert_eq!(error.detail, Some(culprit_core::ErrorDetail::Delivery(DeliveryProblem::Integrity)), "{name}");
    }
}

#[tokio::test]
async fn wait_fragment_originals_roundtrip_exact_statistics_native_labels_empty_and_unknown_populations() {
    for (source, records) in test_support::query_wait_evidence_cases() {
        let expected =
            records.iter().filter(|row| matches!(row, TelemetryRecord::QueryWaits(_))).cloned().collect::<Vec<_>>();
        let fixture = Fixture::new_for_stream(source.clone(), records, TelemetryStream::QueryWaits).await;
        fixture.replies(fixture.rows.iter().map(|row| vec![row.clone()]).collect());
        let capture = fixture
            .reader
            .read_capture(
                &fixture.range,
                &[EvidenceFamily::QueryWaits],
                EvidenceReadBudget { page_rows: 1, ..Default::default() },
                None,
            )
            .await
            .unwrap();
        assert!(capture.is_read_complete(), "{:?}", capture.gaps());
        assert_eq!(capture.source(), &source);
        assert_eq!(capture.records().len(), expected.len());
        for original in expected {
            let exported = ExportedRecord::new(&source, capture.capture(), original).unwrap();
            assert!(capture.records().iter().any(|actual| actual.row() == &exported));
        }
        assert!(matches!(*fixture.recovery.fault.lock().unwrap(), Fault::Stall));
    }
}

#[tokio::test]
async fn nullable_runtime_means_keep_exact_bigint_endpoints_counts_and_native_read_operation_units() {
    for missing in [false, true] {
        let (source, records) = test_support::query_runtime_fragment_evidence(missing);
        let expected =
            records.iter().filter(|row| matches!(row, TelemetryRecord::QueryRuntime(_))).cloned().collect::<Vec<_>>();
        let fixture = Fixture::new_for_stream(source.clone(), records, TelemetryStream::QueryRuntime).await;
        fixture.replies(fixture.rows.iter().map(|row| vec![row.clone()]).collect());
        let capture = fixture
            .reader
            .read_capture(
                &fixture.range,
                &[EvidenceFamily::QueryRuntime],
                EvidenceReadBudget { page_rows: 1, ..Default::default() },
                None,
            )
            .await
            .unwrap();
        assert!(capture.is_read_complete(), "{:?}", capture.gaps());
        assert_eq!(capture.records().len(), 2);
        for row in &expected {
            let exported = ExportedRecord::new(&source, capture.capture(), row.clone()).unwrap();
            assert!(capture.records().iter().any(|actual| actual.row() == &exported));
        }
        let TelemetryRecord::QueryRuntime(first) = &capture.records()[0].row().record else { panic!() };
        let duration = first.metric(&RuntimeMetricKey::Duration).unwrap();
        assert_eq!(duration.average.is_none(), missing);
        assert!(matches!(&duration.last,Some(MeasurementValue::Integer(value)) if value=="9007199254740993"));
        let schema = fixture.catalog.get(TelemetryStream::QueryRuntime).unwrap();
        let column = schema.columns().iter().position(|column| column.name == "Metrics").unwrap();
        let mut rows = fixture.rows.clone();
        let mut metrics: Value = serde_json::from_str(rows[0][column].as_str().unwrap()).unwrap();
        metrics[0]["last"]["value"] = json!("9007199254740992");
        rows[0][column] = json!(serde_json::to_string(&metrics).unwrap());
        fixture.replies(vec![rows]);
        let error = fixture
            .reader
            .read_capture(&fixture.range, &[EvidenceFamily::QueryRuntime], EvidenceReadBudget::default(), None)
            .await
            .unwrap_err();
        assert_eq!(error.detail, Some(culprit_core::ErrorDetail::Delivery(DeliveryProblem::Integrity)));
    }
}
