use super::*;
use std::sync::atomic::Ordering;

#[tokio::test]
async fn compiler_and_original_definition_corpora_preserve_each_source_native_row_and_exact_receipt() {
    for case in test_support::compilation_definition_evidence_cases() {
        let unicode = case.name == "short_unicode_sql";
        for (is_definition, (source, records)) in
            std::iter::once((false, case.current)).chain(case.definitions.map(|definition| (true, definition)))
        {
            let family =
                if is_definition { EvidenceFamily::QueryDefinitions } else { EvidenceFamily::OperationalSamples };
            let fixture = Fixture::new_for_stream(source.clone(), records.clone(), family.stream()).await;
            let opens = fixture.recovery.object_reads.load(Ordering::SeqCst);
            assert!(matches!(*fixture.recovery.fault.lock().unwrap(), Fault::Stall));
            fixture.replies(vec![fixture.rows.clone()]);
            let capture = fixture
                .reader
                .read_capture(&fixture.range, &[family], EvidenceReadBudget::default(), None)
                .await
                .unwrap();
            if unicode && is_definition {
                assert!(!capture.is_read_complete());
                assert!(capture.records().is_empty());
                assert!(capture.gaps().iter().any(|gap| gap.reason == EvidenceGapReason::DetailInRecovery));
            } else {
                test_support::assert_operational_capture(&capture, &source, &records);
            }
            assert_eq!(capture.source(), &source);
            assert_eq!(capture.metrics().recovery_objects, 0);
            assert_eq!(fixture.recovery.object_reads.load(Ordering::SeqCst), opens);
            if unicode && is_definition {
                *fixture.recovery.fault.lock().unwrap() = Fault::None;
                fixture.replies(vec![fixture.rows.clone()]);
                let batch =
                    fixture.range.manifest().batches.iter().find(|batch| batch.stream == family.stream()).unwrap();
                let options = EvidenceReadBudget {
                    recovery_payloads: Some(RecoveryPayloadBudget { max_objects: 1, max_bytes: batch.bytes }),
                    ..EvidenceReadBudget::default()
                };
                let hydrated = fixture
                    .reader
                    .read_capture(&fixture.range, &[family], options, Some(fixture.recovery.as_ref()))
                    .await
                    .unwrap();
                test_support::assert_operational_capture(&hydrated, &source, &records);
                assert_eq!(hydrated.metrics().recovery_objects, 1);
                assert_eq!(fixture.recovery.object_reads.load(Ordering::SeqCst), opens + 1);
            }
        }
    }
}

#[tokio::test]
async fn changed_definition_incarnation_origin_or_sql_cannot_borrow_the_original_projection_receipt() {
    for field in ["context_settings_id", "initial_compile_at", "origin", "sql"] {
        let case = test_support::compilation_definition_evidence_cases()
            .into_iter()
            .find(|case| case.name == "matched")
            .unwrap();
        let (source, records) = case.definitions.unwrap();
        let family = EvidenceFamily::QueryDefinitions;
        let fixture = Fixture::new_for_stream(source.clone(), records.clone(), family.stream()).await;
        let opens = fixture.recovery.object_reads.load(Ordering::SeqCst);
        *fixture.recovery.fault.lock().unwrap() = Fault::None;
        let schema = fixture.catalog.get(TelemetryStream::QueryDefinitions).unwrap();
        let mut rows = fixture.rows_for(TelemetryStream::QueryDefinitions);
        let column_name = match field {
            "origin" => "StatementOrigin",
            "sql" => "DefinitionPayloads",
            _ => "NativeIdentity",
        };
        let column = schema.columns().iter().position(|column| column.name == column_name).unwrap();
        if field == "origin" {
            rows[0][column] = json!("engine-internal");
        } else {
            let mut value: serde_json::Value = serde_json::from_str(rows[0][column].as_str().unwrap()).unwrap();
            match field {
                "context_settings_id" => value["qualifiers"][field] = json!("6"),
                "initial_compile_at" => value["qualifiers"][field] = json!("2020-01-01T07:00:00Z"),
                "sql" => {
                    let sql = "SELECT different_column FROM dbo.another_statement OPTION(RECOMPILE)";
                    value[0]["preview"] = json!(sql);
                    value[0]["bytes"] = json!(sql.len());
                    value[0]["sha256"] = json!(digest(sql.as_bytes()));
                }
                _ => unreachable!(),
            }
            rows[0][column] = json!(value.to_string());
        }
        fixture.replies(vec![rows]);
        let options = EvidenceReadBudget {
            recovery_payloads: Some(RecoveryPayloadBudget::default()),
            ..EvidenceReadBudget::default()
        };
        let error = fixture
            .reader
            .read_capture(&fixture.range, &[family], options, Some(fixture.recovery.as_ref()))
            .await
            .unwrap_err();
        assert_eq!(error.detail, Some(culprit_core::ErrorDetail::Delivery(DeliveryProblem::Integrity)), "{field}");
        assert_eq!(fixture.recovery.object_reads.load(Ordering::SeqCst), opens, "{field}");
    }
}

#[tokio::test]
async fn missing_original_definition_preserves_the_selected_manifest_count_and_explicit_read_gap() {
    let case =
        test_support::compilation_definition_evidence_cases().into_iter().find(|case| case.name == "matched").unwrap();
    let (source, records) = case.definitions.unwrap();
    let family = EvidenceFamily::QueryDefinitions;
    let fixture = Fixture::new_for_stream(source.clone(), records.clone(), family.stream()).await;
    let opens = fixture.recovery.object_reads.load(Ordering::SeqCst);
    assert!(matches!(*fixture.recovery.fault.lock().unwrap(), Fault::Stall));
    fixture.replies(vec![vec![]]);
    let capture =
        fixture.reader.read_capture(&fixture.range, &[family], EvidenceReadBudget::default(), None).await.unwrap();
    assert!(!capture.is_read_complete());
    assert!(capture.records().is_empty());
    assert_eq!(capture.source(), &source);
    assert_eq!(capture.metrics().selected_rows, 1);
    assert!(capture.gaps().iter().any(|gap| gap.reason == EvidenceGapReason::MissingRows));
    assert_eq!(capture.metrics().recovery_objects, 0);
    assert_eq!(fixture.recovery.object_reads.load(Ordering::SeqCst), opens);
}
