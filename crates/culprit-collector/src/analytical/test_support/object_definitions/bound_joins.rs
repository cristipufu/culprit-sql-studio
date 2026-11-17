//! Controlled original native facts for replay conformance. This declares evidence; it implements no classifier.
use super::*;
use culprit_core::plan_facts::{RelationalInput, RelationalInputKind, RelationalInputProvenance};

pub fn object_definition_bound_join_evidence() -> Vec<(CollectionDescription, Vec<TelemetryRecord>)> {
    let [_, (sql, raw_xml)] = hydration_native_inputs();
    [false, true].into_iter().map(|retained| bound_join(&sql, &raw_xml, retained)).collect()
}

fn bound_join(sql: &str, raw_xml: &str, retained: bool) -> (CollectionDescription, Vec<TelemetryRecord>) {
    let dataset = if retained { SnapshotDataset::RetainedPlans } else { SnapshotDataset::CachedDefinitions };
    let source = qualified_source(dataset, 1, SourceState::Collected);
    let mut definition = cached_definition();
    definition.context.query_hash = Some("0x1023456789ABCDEF".into());
    definition.statement = cached_payload(sql, DefinitionPayload::sql);
    definition.context.source_text_bytes = definition.statement.source_utf16_bytes.clone();
    let xml = if retained { retained_xml(raw_xml) } else { raw_xml.into() };
    definition.estimated_plan = cached_payload(&xml, DefinitionPayload::plan);
    let bindings = vec![MssqlRelationalInputBinding {
        node_id: 1,
        input: RelationalInput {
            kind: RelationalInputKind::TableValuedParameter,
            provenance: RelationalInputProvenance::NativeParameterMetadata,
        },
    }];
    let fact = if retained {
        let mut retained = MssqlRetainedPlanDefinition {
            context: definition.context,
            statement: definition.statement,
            module_parameters: definition.module_parameters,
            relational_input_proof: None,
            parameter_context_proof: None,
            statement_executions: Some("9007199254740993".into()),
            last_start_without_offset: Some("2020-01-01T11:59:00.003".into()),
            database_last_query_plan_stats: Some(true),
            batch_plan: definition.estimated_plan,
        };
        retained.relational_input_proof =
            Some(MssqlRelationalInputProof::for_retained(&source, &retained, bindings).unwrap());
        ObjectDefinitionFact::MssqlRetainedPlan(Box::new(retained))
    } else {
        definition.relational_input_proof =
            Some(MssqlRelationalInputProof::for_cached(&source, &definition, bindings).unwrap());
        ObjectDefinitionFact::MssqlCachedDefinition(Box::new(definition))
    };
    let record = TelemetryRecord::ObjectDefinition(
        ObjectDefinitionRecord::new("native-object-definitions".into(), fact).unwrap(),
    );
    (source, vec![record])
}

fn retained_xml(raw: &str) -> String {
    let access = r#"<RelOp NodeId="1" PhysicalOp="Table Scan" LogicalOp="Table Scan" EstimateRows="100" EstimatedTotalSubtreeCost="1">"#;
    // The retained native fragment records this operator only. Other operators have no fabricated runtime rows.
    let measured = r#"<RelOp NodeId="1" PhysicalOp="Table Scan" LogicalOp="Table Scan" EstimateRows="100" EstimatedRowsRead="100" Parallel="false" EstimatedTotalSubtreeCost="1"><RunTimeInformation><RunTimeCountersPerThread Thread="0" ActualRows="10000" ActualRowsRead="20000" ActualExecutions="1" ActualEndOfScans="1" /></RunTimeInformation>"#;
    assert!(raw.contains(access));
    raw.replace(access, measured)
}
