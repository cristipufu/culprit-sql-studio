use super::*;
use culprit_core::plan_facts::{PlanParameterComparison as Comparison, PlanParameterIssue as Issue};

const SQL: &str = "SELECT customer FROM dbo.orders WHERE customer=@customer";
const XML: &str = r#"<ShowPlanXML><BatchSequence><Batch><Statements><StmtSimple StatementText="SELECT customer FROM dbo.orders WHERE customer=@customer" QueryHash="0x0123456789ABCDEF" StatementSubTreeCost="1"><QueryPlan>
<ParameterList><ColumnReference Column="@customer" ParameterDataType="nvarchar(4000)" ParameterCompiledValue="N'private-compiled-canary'" ParameterRuntimeValue="N'private-runtime-canary'"/></ParameterList>
<RelOp NodeId="7" PhysicalOp="Index Seek" LogicalOp="Index Seek" Parallel="false" EstimateRows="100" EstimatedRowsRead="100" EstimatedTotalSubtreeCost="1">
<RunTimeInformation><RunTimeCountersPerThread Thread="0" ActualRows="10000" ActualRowsRead="10000" ActualExecutions="1"/></RunTimeInformation>
<IndexScan><Object Database="[app]" Schema="[dbo]" Table="[orders]" Index="[ix_customer]"/>
<SeekPredicates><SeekPredicateNew><SeekKeys><Prefix ScanType="EQ"><RangeColumns><ColumnReference Table="[orders]" Column="customer"/></RangeColumns>
<RangeExpressions><ScalarOperator ScalarString="[@customer]"><Identifier><ColumnReference Column="@customer"/></Identifier></ScalarOperator></RangeExpressions>
</Prefix></SeekKeys></SeekPredicateNew></SeekPredicates></IndexScan></RelOp></QueryPlan></StmtSimple></Statements></Batch></BatchSequence></ShowPlanXML>"#;

fn row(sql: Option<&str>, xml: &str) -> Rec {
    replace(
        &crate::cached_definitions::tests::row(),
        &[
            ("query_hash", Val::Text("0x0123456789ABCDEF".into())),
            ("statement_start_offset", Val::Int(0)),
            ("statement_end_offset", Val::Int(-1)),
            ("source_text_bytes", sql.map_or(Val::Null, |sql| Val::Int((sql.encode_utf16().count() * 2) as i64))),
            ("text_prefix", sql.map_or(Val::Null, |sql| Val::Text(sql.into()))),
            ("text_complete", Val::Int(i64::from(sql.is_some()))),
            ("plan_bytes", Val::Int((xml.encode_utf16().count() * 2) as i64)),
            ("query_plan", Val::Text(xml.into())),
            ("execution_count", Val::Int(9_007_199_254_740_993)),
        ],
    )
}

fn limits() -> Layout {
    Layout {
        maximum_bytes: 128 * 1024,
        maximum_statement_bytes: retained::MAX_STATEMENT_BYTES,
        database_setting: Some(true),
    }
}

#[test]
fn original_scalar_facts_and_node_access_are_identical_interactive_and_offline_without_literal_or_counter_borrowing() {
    let source = crate::plan_xml::test_source(SnapshotDataset::RetainedPlans);
    let row = row(Some(SQL), XML);
    let definition = decode(&row, &limits(), Some(&source)).unwrap();
    let proof = definition.parameter_context_proof.as_ref().unwrap();
    assert!(proof.matches_retained(&source, &definition));
    assert_eq!(proof.context.parameters[0].comparison, Comparison::DifferentRepresentation);
    assert_eq!(proof.context.uses[0].node_id, 7);
    let original = crate::plan_xml::parse_cached_statement(
        XML,
        crate::plan_xml::CachedInputContext {
            statement: &definition.context,
            module_parameters: None,
            text: Some(SQL),
            origin: crate::plan_xml::PlanInputOrigin::OriginalCapture { source: &source },
        },
    )
    .unwrap();
    let offline = crate::plan_xml::parse_retained_definition(&definition, &source).unwrap();
    assert_eq!(offline.facts().parameter_context, original.facts().parameter_context);
    assert_eq!(offline.root.facts().access_rows, original.root.facts().access_rows);
    assert_eq!(offline.root.facts().serial_execution_count.as_deref(), Some("1"));
    assert!(offline.facts().last_run_at.is_none());
    assert_eq!(definition.statement_executions.as_deref(), Some("9007199254740993"));
    let insights = culprit_core::insights::insights(&offline);
    let context = insights.iter().find(|insight| insight.rule == "parameter-context").unwrap();
    assert_eq!(context.nodes, vec![7]);
    assert_eq!(context.impact, 0.0);
    assert!(context.fix_sql.is_none());
    assert!(insights.iter().filter(|insight| insight.rule == "misestimate").all(|insight| insight.fix_sql.is_none()));
    let encoded = serde_json::to_string(&definition).unwrap();
    assert!(!encoded.contains("private-compiled-canary") && !encoded.contains("private-runtime-canary"));
}

#[test]
fn missing_or_different_original_statement_never_gets_scalar_proof_from_a_matching_query_hash() {
    let source = crate::plan_xml::test_source(SnapshotDataset::RetainedPlans);
    for sql in [None, Some("SELECT customer FROM dbo.other_orders WHERE customer=@customer")] {
        let definition = decode(&row(sql, XML), &limits(), Some(&source)).unwrap();
        assert!(definition.parameter_context_proof.is_none());
        let original = crate::plan_xml::parse_cached_statement(
            XML,
            crate::plan_xml::CachedInputContext {
                statement: &definition.context,
                module_parameters: None,
                text: sql,
                origin: crate::plan_xml::PlanInputOrigin::OriginalCapture { source: &source },
            },
        )
        .unwrap();
        let context = original.facts().parameter_context.as_ref().unwrap();
        assert!(context.issues.contains(&Issue::UnboundStatement));
        assert!(context.uses.is_empty());
        assert!(context.parameters.iter().all(|parameter| parameter.comparison == Comparison::Unavailable));
        let offline = crate::plan_xml::parse_retained_definition(&definition, &source).unwrap();
        assert!(culprit_core::insights::insights(&offline).iter().all(|insight| insight.rule != "parameter-context"));
    }
}

#[test]
fn native_statement_ambiguity_and_changed_source_never_choose_a_representative_parameter_context() {
    let source = crate::plan_xml::test_source(SnapshotDataset::RetainedPlans);
    let statement = XML.split("<Statements>").nth(1).unwrap().split("</Statements>").next().unwrap();
    let duplicate = format!(
        "<ShowPlanXML><BatchSequence><Batch><Statements>{statement}{statement}</Statements></Batch></BatchSequence></ShowPlanXML>"
    );
    let definition = decode(&row(Some(SQL), &duplicate), &limits(), Some(&source)).unwrap();
    assert!(definition.parameter_context_proof.is_none());
    assert!(crate::plan_xml::parse_retained_definition(&definition, &source).is_err());
    let definition = decode(&row(Some(SQL), XML), &limits(), Some(&source)).unwrap();
    let mut changed = source;
    let SourcePartition::InstanceLocal { epoch, .. } = &mut changed.partition else { panic!() };
    *epoch = Some("different-native-process".into());
    let error = crate::plan_xml::parse_retained_definition(&definition, &changed).unwrap_err();
    assert_eq!(error.detail, Some(culprit_core::ErrorDetail::Delivery(DeliveryProblem::Integrity)));
}

#[test]
fn content_binding_cannot_replace_the_native_access_relationship_even_with_a_recomputed_digest() {
    let source = crate::plan_xml::test_source(SnapshotDataset::RetainedPlans);
    let mut definition = decode(&row(Some(SQL), XML), &limits(), Some(&source)).unwrap();
    let mut facts = definition.parameter_context_proof.take().unwrap().context;
    facts.uses[0].node_id = 8;
    definition.parameter_context_proof =
        Some(MssqlParameterContextProof::for_retained(&source, &definition, facts).unwrap());
    assert!(definition.is_valid(), "structural content binding is separate from native parser qualification");
    let error = crate::plan_xml::parse_retained_definition(&definition, &source).unwrap_err();
    assert_eq!(error.detail, Some(culprit_core::ErrorDetail::Delivery(DeliveryProblem::Integrity)));
}

mod full_batch;

#[test]
fn native_xml_attribute_whitespace_keeps_original_parameter_proof_through_canonical_export_and_restore() {
    let source = crate::plan_xml::test_source(SnapshotDataset::RetainedPlans);
    let sql = SQL.replacen("SELECT ", "SELECT\r\n\t", 1);
    let xml = XML.replacen("StatementText=\"SELECT ", "StatementText=\"SELECT&#xD;&#xA;&#x9;", 1);
    let definition = decode(&row(Some(&sql), &xml), &limits(), Some(&source)).unwrap();
    let original_proof =
        definition.parameter_context_proof.clone().expect("original native statement is exactly bound");
    assert!(original_proof.matches_retained(&source, &definition));
    let original_native_sha = definition.batch_plan.native_sha256.clone();
    let record = TelemetryRecord::ObjectDefinition(
        ObjectDefinitionRecord::new(
            source.observation.snapshot().unwrap().sources[0].source.clone(),
            ObjectDefinitionFact::MssqlRetainedPlan(Box::new(definition)),
        )
        .unwrap(),
    );
    // This is the canonical structural boundary, not a claim of durable capture authentication.
    let capture = CaptureIdentity { observation_id: "controlled-original-parameter-proof".into(), revision: None };
    let first = ExportedRecord::new(&source, &capture, record).unwrap();
    let second = ExportedRecord::new(&source, &capture, first.record.clone()).unwrap();
    assert_eq!(first, second, "canonical export may not change the content-bound original proof");
    let TelemetryRecord::ObjectDefinition(record) = &first.record else { panic!() };
    let ObjectDefinitionFact::MssqlRetainedPlan(retained) = &record.fact else { panic!() };
    assert_eq!(retained.parameter_context_proof.as_ref(), Some(&original_proof));
    assert_eq!(retained.batch_plan.native_sha256, original_native_sha);
    assert!(original_proof.matches_retained(&source, retained));
    let restored = crate::plan_xml::parse_retained_definition(retained, &source).unwrap();
    assert_eq!(restored.facts().parameter_context.as_ref(), Some(&original_proof.context));
    assert_eq!(restored.root.facts().access_rows.unwrap().actual_total, Some(10000.0));
    let encoded = serde_json::to_string(&first).unwrap();
    for private in ["private-compiled-canary", "private-runtime-canary"] {
        assert!(!encoded.contains(private));
    }

    let mut changed = (**retained).clone();
    changed.batch_plan.payload =
        DefinitionPayload::plan(&xml.replace("NodeId=\"7\"", "NodeId=\"8\""), culprit_core::Engine::Mssql);
    assert!(!changed.is_valid(), "a canonical body change must still invalidate the original proof");
}
