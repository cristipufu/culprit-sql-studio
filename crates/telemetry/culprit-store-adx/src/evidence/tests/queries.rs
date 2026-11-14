use super::*;

const FAMILIES: &[EvidenceFamily] = &[
    EvidenceFamily::QueryDefinitions,
    EvidenceFamily::PlanDefinitions,
    EvidenceFamily::QueryRuntime,
    EvidenceFamily::QueryWaits,
];

#[tokio::test]
async fn native_variant_metadata_round_trips_without_requiring_or_synthesizing_parent_runtime() {
    let (source, records) = test_support::query_variant_evidence();
    let fixture = Fixture::new(source.clone(), records.clone()).await;
    fixture.reply_families(FAMILIES, 250);
    let captured = fixture.reader().read_capture(&fixture.range, FAMILIES, budget()).await.unwrap();
    assert!(captured.is_read_complete(), "{:?}", captured.gaps());
    assert_eq!(captured.records().len(), records.len());
    for expected in &records {
        let expected = ExportedRecord::new(&source, captured.capture(), expected.clone()).unwrap();
        assert!(captured.records().iter().any(|row| row.row() == &expected));
    }
    let definitions = captured
        .records()
        .iter()
        .filter_map(|row| match &row.row().record {
            TelemetryRecord::QueryDefinition(query) => Some(query),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(definitions.len(), 1);
    let family = definitions[0].variant_family.as_ref().unwrap();
    assert_eq!(family.variant, definitions[0].identity);
    assert_ne!(family.parent, family.variant);
    assert_eq!(family.dispatcher.query, family.parent);
}

#[tokio::test]
async fn changed_variant_parent_dispatcher_or_incarnation_fails_the_original_projection_proof() {
    let (source, records) = test_support::query_variant_evidence();
    let fixture = Fixture::new(source, records).await;
    let schema = fixture.catalog.get(TelemetryStream::QueryDefinitions).unwrap();
    let column = schema.columns().iter().position(|column| column.name == "QueryVariantFamily").unwrap();
    for pointer in
        ["/variant/qualifiers/context", "/parent/query_id", "/dispatcher/plan_id", "/dispatcher/qualifiers/generation"]
    {
        let mut rows = fixture.rows_for(EvidenceFamily::QueryDefinitions);
        let mut family: Value = serde_json::from_str(rows[0][column].as_str().unwrap()).unwrap();
        *family.pointer_mut(pointer).unwrap() = json!("another-native-incarnation");
        rows[0][column] = json!(serde_json::to_string(&family).unwrap());
        fixture.replies(vec![rows]);
        let error = fixture
            .reader()
            .read_capture(&fixture.range, &[EvidenceFamily::QueryDefinitions], budget())
            .await
            .unwrap_err();
        assert_eq!(error.detail, Some(culprit_core::ErrorDetail::Delivery(DeliveryProblem::Integrity)));
    }
}

#[tokio::test]
async fn temporary_space_average_or_page_unit_tampering_fails_the_original_committed_proof() {
    let (source, records) = test_support::query_evidence();
    let fixture = Fixture::new(source, records).await;
    let schema = fixture.catalog.get(TelemetryStream::QueryRuntime).unwrap();
    let column = schema.columns().iter().position(|column| column.name == "Metrics").unwrap();
    for (pointer, replacement) in [("/2/average", json!(99.0)), ("/2/unit/bytes_per_page", json!(8192))] {
        let mut rows = fixture.rows_for(EvidenceFamily::QueryRuntime);
        let mut metrics: Value = serde_json::from_str(rows[0][column].as_str().unwrap()).unwrap();
        *metrics.pointer_mut(pointer).unwrap() = replacement;
        rows[0][column] = json!(serde_json::to_string(&metrics).unwrap());
        fixture.replies(vec![rows]);
        let error =
            fixture.reader().read_capture(&fixture.range, &[EvidenceFamily::QueryRuntime], budget()).await.unwrap_err();
        assert_eq!(error.detail, Some(culprit_core::ErrorDetail::Delivery(DeliveryProblem::Integrity)));
    }
}

#[tokio::test]
async fn query_reads_preserve_exact_references_outcomes_metrics_scope_and_redacted_text() {
    let (source, records) = test_support::query_evidence();
    let fixture = Fixture::new(source.clone(), records.clone()).await;
    fixture.reply_families(FAMILIES, 1);
    let result = fixture
        .reader()
        .read_capture(&fixture.range, FAMILIES, EvidenceReadBudget { page_rows: 1, ..budget() })
        .await
        .unwrap();
    assert!(result.is_read_complete());
    assert_eq!(result.source(), &source);
    assert_eq!(result.metrics().pages, records.len() as u32);
    assert_eq!(result.records().len(), records.len());
    for expected in records {
        let expected = ExportedRecord::new(&source, result.capture(), expected).unwrap();
        let actual = result.records().iter().find(|record| record.row().record_id == expected.record_id).unwrap();
        assert_eq!(actual.row(), &expected);
    }
    for record in result.records() {
        if let TelemetryRecord::QueryRuntime(runtime) = &record.row().record {
            let space = runtime.metric(&RuntimeMetricKey::TemporaryStorage).unwrap();
            assert_eq!(space.unit, RuntimeMetricUnit::Pages { bytes_per_page: 16384 });
            assert_eq!(space.average, 1.25);
            assert_eq!(runtime.executions, "9007199254740993");
        }
    }
    let query = result
        .records()
        .iter()
        .find_map(|record| match &record.row().record {
            TelemetryRecord::QueryDefinition(query) => Some(query),
            _ => None,
        })
        .unwrap();
    let DefinitionPayload::Available { text, .. } = &query.sql else { panic!("redacted SQL") };
    assert!(!text.contains("fixture-private-literal"));
    assert!(!text.contains("9264"));
    let requests = fixture.query.requests.lock().unwrap();
    assert!(requests.iter().all(|request| request.byte_limit <= budget().page_bytes as usize));
    assert!(requests.iter().all(|request| !request.text.contains(&fixture.range.manifest().range_id)));
    assert!(matches!(*fixture.recovery.fault.lock().unwrap(), Fault::Stall));
}

#[tokio::test]
async fn missing_query_definition_is_not_replaced_by_identical_sql_from_another_native_context() {
    let (source, mut records) = test_support::query_evidence();
    let TelemetryRecord::QueryDefinition(original) = &records[0] else { panic!() };
    let mut decoy = original.clone();
    decoy.identity.qualifiers.insert("userid".into(), "18".into());
    decoy.id = decoy.identity.definition_id(&source).unwrap();
    let original_id = original.id.clone();
    records.push(TelemetryRecord::QueryDefinition(decoy));
    let fixture = Fixture::new(source, records).await;
    let mut queries = fixture.rows_for(EvidenceFamily::QueryDefinitions);
    queries.remove(0);
    fixture.replies(vec![
        queries,
        fixture.rows_for(EvidenceFamily::PlanDefinitions),
        fixture.rows_for(EvidenceFamily::QueryRuntime),
        fixture.rows_for(EvidenceFamily::QueryWaits),
    ]);
    let result = fixture.reader().read_capture(&fixture.range, FAMILIES, budget()).await.unwrap();
    assert!(!result.is_read_complete());
    assert_eq!(result.gaps().len(), 1);
    assert_eq!(result.gaps()[0].stream, Some(TelemetryStream::QueryDefinitions));
    assert_eq!(result.gaps()[0].row_ordinal, Some(1));
    assert_eq!(result.gaps()[0].reason, EvidenceGapReason::MissingRows);
    for record in result.records() {
        match &record.row().record {
            TelemetryRecord::PlanDefinition(plan) => assert_eq!(plan.query_definition_id, Some(original_id.clone())),
            TelemetryRecord::QueryRuntime(runtime) => {
                assert_eq!(runtime.key.references.query.as_ref().unwrap().definition_id, Some(original_id.clone()));
            }
            _ => (),
        }
    }
}

#[tokio::test]
async fn changed_plan_parent_identity_or_source_scope_fails_against_the_committed_native_closure() {
    let (source, records) = test_support::query_evidence();
    let fixture = Fixture::new(source, records).await;
    let schema = fixture.catalog.get(TelemetryStream::PlanDefinitions).unwrap();
    for column_name in ["QueryDefinitionId", "NativeIdentity", "ResourceId"] {
        let column = schema.columns().iter().position(|column| column.name == column_name).unwrap();
        let mut plans = fixture.rows_for(EvidenceFamily::PlanDefinitions);
        plans[0][column] = match column_name {
            "NativeIdentity" => {
                let mut identity: Value = serde_json::from_str(plans[0][column].as_str().unwrap()).unwrap();
                identity["query"]["qualifiers"]["userid"] = json!("18");
                json!(serde_json::to_string(&identity).unwrap())
            }
            _ => json!(digest(b"another-native-scope-or-parent")),
        };
        fixture.replies(vec![fixture.rows_for(EvidenceFamily::QueryDefinitions), plans]);
        let error = fixture.reader().read_capture(&fixture.range, FAMILIES, budget()).await.unwrap_err();
        assert_eq!(error.detail, Some(culprit_core::ErrorDetail::Delivery(DeliveryProblem::Integrity)));
    }
}

#[tokio::test]
async fn conflicting_sql_with_the_same_definition_id_and_native_hash_is_rejected() {
    let (source, records) = test_support::query_evidence();
    let fixture = Fixture::new(source, records).await;
    let schema = fixture.catalog.get(TelemetryStream::QueryDefinitions).unwrap();
    let column = schema.columns().iter().position(|column| column.name == "DefinitionPayloads").unwrap();
    let mut queries = fixture.rows_for(EvidenceFamily::QueryDefinitions);
    let mut conflict = queries[0].clone();
    let mut payloads: Value = serde_json::from_str(conflict[column].as_str().unwrap()).unwrap();
    payloads[0]["preview"] = json!("SELECT another_column FROM another_table");
    payloads[0]["bytes"] = json!(payloads[0]["preview"].as_str().unwrap().len());
    payloads[0]["sha256"] = json!(digest(payloads[0]["preview"].as_str().unwrap().as_bytes()));
    conflict[column] = json!(serde_json::to_string(&payloads).unwrap());
    queries.push(conflict);
    fixture.replies(vec![queries]);
    let error = fixture.reader().read_capture(&fixture.range, FAMILIES, budget()).await.unwrap_err();
    assert_eq!(error.detail, Some(culprit_core::ErrorDetail::Delivery(DeliveryProblem::Integrity)));
}

#[tokio::test]
async fn truncated_sql_and_plan_payloads_remain_explicit_recovery_gaps_with_references_intact() {
    for large_sql in [true, false] {
        let (source, mut records) = test_support::query_evidence();
        let (record, stream) = if large_sql {
            let TelemetryRecord::QueryDefinition(query) = &mut records[0] else { panic!() };
            query.sql = DefinitionPayload::sql(
                &format!("SELECT {}last_column FROM public.orders", "column_name,".repeat(200)),
                source.target.engine(),
            );
            (0, TelemetryStream::QueryDefinitions)
        } else {
            let TelemetryRecord::PlanDefinition(plan) = &mut records[1] else { panic!() };
            plan.plan = DefinitionPayload::plan(
                &json!({"Plan":{"Node Type":"Append","Plans":vec![json!({"Node Type":"Seq Scan"});100]}}).to_string(),
                source.target.engine(),
            );
            (1, TelemetryStream::PlanDefinitions)
        };
        let expected_id = ExportedRecord::new(&source, &test_support::fixture().0.capture, records[record].clone())
            .unwrap()
            .record_id;
        let fixture = Fixture::new(source, records).await;
        fixture.reply_families(FAMILIES, 250);
        let result = fixture.reader().read_capture(&fixture.range, FAMILIES, budget()).await.unwrap();
        assert!(!result.is_read_complete());
        assert_eq!(result.gaps().len(), 1);
        assert_eq!(result.gaps()[0].stream, Some(stream));
        assert_eq!(result.gaps()[0].reason, EvidenceGapReason::DetailInRecovery);
        assert!(result.records().iter().all(|record| record.row().record_id != expected_id));
        assert!(result.records().iter().any(|record| matches!(record.row().record, TelemetryRecord::QueryRuntime(_))));
    }
}

#[tokio::test]
async fn native_unavailable_payloads_are_preserved_as_source_facts_without_inventing_read_gaps() {
    let (source, mut records) = test_support::query_evidence();
    let TelemetryRecord::QueryDefinition(query) = &mut records[0] else { panic!() };
    query.sql = DefinitionPayload::Unavailable { reason: PayloadUnavailable::Restricted };
    query.origin = StatementOrigin::CulpritMonitoring;
    query.monitoring_template_id = Some(digest(b"native-monitoring-template"));
    let TelemetryRecord::PlanDefinition(plan) = &mut records[1] else { panic!() };
    plan.plan = DefinitionPayload::Unavailable { reason: PayloadUnavailable::Encrypted };
    let fixture = Fixture::new(source, records).await;
    fixture.reply_families(FAMILIES, 250);
    let result = fixture.reader().read_capture(&fixture.range, FAMILIES, budget()).await.unwrap();
    assert!(result.is_read_complete());
    for record in result.records() {
        match &record.row().record {
            TelemetryRecord::QueryDefinition(query) => {
                assert_eq!(query.origin, StatementOrigin::CulpritMonitoring);
                assert_eq!(query.monitoring_template_id, Some(digest(b"native-monitoring-template")));
                assert_eq!(query.sql, DefinitionPayload::Unavailable { reason: PayloadUnavailable::Restricted })
            }
            TelemetryRecord::PlanDefinition(plan) => {
                assert_eq!(plan.plan, DefinitionPayload::Unavailable { reason: PayloadUnavailable::Encrypted })
            }
            _ => (),
        }
    }
}

#[tokio::test]
async fn verified_timestamp_rounding_never_becomes_an_exact_runtime_measurement() {
    let (source, mut records) = test_support::query_evidence();
    let TelemetryRecord::QueryRuntime(runtime) = &mut records[2] else { panic!() };
    runtime.first_execution_at = Some("2020-01-01T06:00:00.123456001Z".into());
    let fixture = Fixture::new(source, records).await;
    fixture.reply_families(FAMILIES, 250);
    let result = fixture.reader().read_capture(&fixture.range, FAMILIES, budget()).await.unwrap();
    assert!(!result.is_read_complete());
    assert_eq!(result.gaps().len(), 1);
    assert_eq!(result.gaps()[0].stream, Some(TelemetryStream::QueryRuntime));
    assert_eq!(result.gaps()[0].reason, EvidenceGapReason::NativeRepresentationRequired);
}

#[tokio::test]
async fn all_declared_families_are_admitted_and_absent_families_remain_explicit() {
    let (source, records) = test_support::query_evidence();
    let fixture = Fixture::new(source, records).await;
    fixture.reply_families(EvidenceFamily::ALL, 250);
    let result = fixture.reader().read_capture(&fixture.range, EvidenceFamily::ALL, budget()).await.unwrap();
    assert_eq!(result.families(), EvidenceFamily::ALL);
    assert_eq!(result.records().len(), 6);
    assert_eq!(result.gaps().len(), 2);
    assert!(result.gaps().iter().all(|gap| gap.reason == EvidenceGapReason::FamilyNotCaptured));
}
