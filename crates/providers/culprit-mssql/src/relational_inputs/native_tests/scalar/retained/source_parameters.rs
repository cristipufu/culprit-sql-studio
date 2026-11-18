//! A source-owned proof may honestly lack runtime values. Its canonical XML preserves attribute presence
//! after value redaction; STATISTICS XML from another acquisition never fills those absent attributes.
use culprit_core::{
    Plan,
    plan_facts::{PlanParameterComparison, PlanParameterValueState},
    telemetry::*,
};

pub(super) fn assert_own_metadata(definition: &MssqlRetainedPlanDefinition, plan: &Plan) {
    let context = plan.facts().parameter_context.as_ref().expect("retained source parameter context");
    assert!(context.is_valid());
    assert_eq!(context, &definition.parameter_context_proof.as_ref().unwrap().context);
    let DefinitionPayload::Available { text: xml, .. } = &definition.batch_plan.payload else {
        panic!("canonical retained XML")
    };
    culprit_core::xml::read(xml, |document| {
        let document = document.unwrap();
        let statement =
            crate::plan_xml::select_native_statement(&document, definition.context.query_hash.as_deref()).unwrap();
        let query_plan = crate::plan_xml::query_plan_of(statement).unwrap();
        let lists = query_plan
            .children()
            .filter(|node| node.is_element() && node.tag_name().name() == "ParameterList")
            .collect::<Vec<_>>();
        let [list] = lists.as_slice() else { panic!("one selected retained parameter list") };
        let native = list
            .children()
            .filter(|node| node.is_element() && node.tag_name().name() == "ColumnReference")
            .collect::<Vec<_>>();
        assert_eq!(native.len(), context.parameters.len());
        for (native, parameter) in native.iter().zip(&context.parameters) {
            assert_eq!(native.attribute("Column"), parameter.name.as_deref());
            assert_eq!(native.attribute("ParameterDataType"), parameter.data_type.as_deref());
            assert_eq!(
                native.attribute("ParameterCompiledValue").is_some(),
                parameter.compiled != PlanParameterValueState::NotRecorded,
                "compiled attribute presence belongs to this retained source"
            );
            assert_eq!(
                native.attribute("ParameterRuntimeValue").is_some(),
                parameter.runtime != PlanParameterValueState::NotRecorded,
                "runtime attribute presence belongs to this retained source"
            );
            if parameter.compiled != PlanParameterValueState::Recorded
                || parameter.runtime != PlanParameterValueState::Recorded
            {
                assert_eq!(parameter.comparison, PlanParameterComparison::Unavailable);
                assert_eq!(parameter.integer_values_equal, None);
            }
        }
    });
    if context.parameters.iter().all(|parameter| parameter.comparison == PlanParameterComparison::Unavailable) {
        assert!(
            !culprit_core::insights::insights(plan).iter().any(|insight| insight.rule == "parameter-context"),
            "missing source runtime values never become a difference finding"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{read::text, resources::test_support::replace, value::Val};

    #[derive(Clone, Copy)]
    enum NativeValues {
        CompiledOnly,
        Both,
        RuntimeOnly,
    }

    fn original(values: NativeValues) -> (CollectionDescription, MssqlRetainedPlanDefinition) {
        let sql = "SELECT id FROM dbo.rows WHERE id=@p";
        let compiled = if matches!(values, NativeValues::RuntimeOnly) { "" } else { r#"ParameterCompiledValue="(1)""# };
        let runtime = if matches!(values, NativeValues::CompiledOnly) { "" } else { r#"ParameterRuntimeValue="(2)""# };
        let xml = format!(
            r#"<ShowPlanXML><BatchSequence><Batch><Statements><StmtSimple StatementText="{sql}" QueryHash="0x0123456789ABCDEF" RetrievedFromCache="true" StatementSubTreeCost="1"><QueryPlan>
<ParameterList><ColumnReference Column="@p" ParameterDataType="int" {compiled} {runtime}/></ParameterList>
<RelOp NodeId="7" PhysicalOp="Index Seek" LogicalOp="Index Seek" Parallel="false" EstimateRows="100" EstimatedRowsRead="100" EstimatedTotalSubtreeCost="1"><RunTimeInformation><RunTimeCountersPerThread Thread="0" ActualRows="1000" ActualRowsRead="1000" ActualExecutions="1"/></RunTimeInformation><IndexScan><Object Database="[app]" Schema="[dbo]" Table="[rows]" Index="[ix_id]"/><SeekPredicates><SeekPredicateNew><SeekKeys><Prefix ScanType="EQ"><RangeColumns><ColumnReference Table="[rows]" Column="id"/></RangeColumns><RangeExpressions><ScalarOperator><Identifier><ColumnReference Column="@p"/></Identifier></ScalarOperator></RangeExpressions></Prefix></SeekKeys></SeekPredicateNew></SeekPredicates></IndexScan></RelOp>
</QueryPlan></StmtSimple></Statements></Batch></BatchSequence></ShowPlanXML>"#
        );
        let row = replace(
            &crate::cached_definitions::tests::row(),
            &[
                ("query_hash", Val::Text("0x0123456789ABCDEF".into())),
                ("statement_start_offset", Val::Int(0)),
                ("statement_end_offset", Val::Int(-1)),
                ("source_text_bytes", Val::Int((sql.encode_utf16().count() * 2) as i64)),
                ("text_prefix", Val::Text(sql.into())),
                ("text_complete", Val::Int(1)),
                ("module_database_id", Val::Null),
                ("module_object_id", Val::Null),
                ("module_number", Val::Null),
                ("plan_bytes", Val::Int((xml.encode_utf16().count() * 2) as i64)),
                ("query_plan", Val::Text(xml)),
            ],
        );
        assert_eq!(text(&row, "text_prefix").unwrap(), sql);
        let source = crate::plan_xml::test_source(SnapshotDataset::RetainedPlans);
        let limits = crate::retained_definitions::Layout {
            maximum_bytes: 128 * 1024,
            maximum_statement_bytes: crate::actual::retained::MAX_STATEMENT_BYTES,
            database_setting: Some(true),
        };
        let definition = crate::retained_definitions::decode(&row, &limits, Some(&source)).unwrap();
        (source, definition)
    }

    #[test]
    fn absent_retained_runtime_stays_unavailable_through_canonical_payload_and_safe_proof_restore() {
        let (source, definition) = original(NativeValues::CompiledOnly);
        let plan = crate::plan_xml::parse_retained_definition(&definition, &source).unwrap();
        assert_own_metadata(&definition, &plan);
        let parameter = &plan.facts().parameter_context.as_ref().unwrap().parameters[0];
        assert_eq!(parameter.compiled, PlanParameterValueState::Recorded);
        assert_eq!(parameter.runtime, PlanParameterValueState::NotRecorded);
        assert_eq!(parameter.comparison, PlanParameterComparison::Unavailable);
        assert_eq!(parameter.integer_values_equal, None);
        let again = crate::plan_xml::parse_retained_definition(&definition, &source).unwrap();
        assert_eq!(plan.facts().parameter_context, again.facts().parameter_context);
    }
    #[test]
    fn retained_runtime_present_preserves_its_own_comparison_without_an_executed_companion() {
        let (source, definition) = original(NativeValues::Both);
        let plan = crate::plan_xml::parse_retained_definition(&definition, &source).unwrap();
        assert_own_metadata(&definition, &plan);
        let parameter = &plan.facts().parameter_context.as_ref().unwrap().parameters[0];
        assert_eq!(parameter.runtime, PlanParameterValueState::Recorded);
        assert_eq!(parameter.comparison, PlanParameterComparison::DifferentRepresentation);
        assert_eq!(parameter.integer_values_equal, Some(false));
    }
    #[test]
    fn borrowed_runtime_claim_is_refused_even_if_a_structural_proof_digest_is_recomputed() {
        let (source, mut definition) = original(NativeValues::CompiledOnly);
        let mut context = definition.parameter_context_proof.take().unwrap().context;
        context.parameters[0].runtime = PlanParameterValueState::Recorded;
        context.parameters[0].comparison = PlanParameterComparison::DifferentRepresentation;
        context.parameters[0].integer_values_equal = Some(false);
        definition.parameter_context_proof =
            Some(MssqlParameterContextProof::for_retained(&source, &definition, context).unwrap());
        assert!(definition.is_valid(), "a content digest alone is not native attribute evidence");
        let error = crate::plan_xml::parse_retained_definition(&definition, &source).unwrap_err();
        assert_eq!(error.detail, Some(culprit_core::ErrorDetail::Delivery(DeliveryProblem::Integrity)));
    }

    #[test]
    fn borrowed_compiled_claim_is_rejected_by_the_production_parser_even_with_a_recomputed_digest() {
        let (source, mut definition) = original(NativeValues::RuntimeOnly);
        let mut context = definition.parameter_context_proof.take().unwrap().context;
        assert_eq!(context.parameters[0].compiled, PlanParameterValueState::NotRecorded);
        context.parameters[0].compiled = PlanParameterValueState::Recorded;
        context.parameters[0].comparison = PlanParameterComparison::DifferentRepresentation;
        context.parameters[0].integer_values_equal = Some(false);
        definition.parameter_context_proof =
            Some(MssqlParameterContextProof::for_retained(&source, &definition, context).unwrap());
        assert!(definition.is_valid());
        let error = crate::plan_xml::parse_retained_definition(&definition, &source).unwrap_err();
        assert_eq!(error.detail, Some(culprit_core::ErrorDetail::Delivery(DeliveryProblem::Integrity)));
    }

    #[test]
    fn a_false_absence_claim_cannot_hide_either_present_native_value_attribute() {
        for compiled in [true, false] {
            let (source, mut definition) = original(NativeValues::Both);
            let mut context = definition.parameter_context_proof.take().unwrap().context;
            if compiled {
                context.parameters[0].compiled = PlanParameterValueState::NotRecorded;
            } else {
                context.parameters[0].runtime = PlanParameterValueState::NotRecorded;
            }
            context.parameters[0].comparison = PlanParameterComparison::Unavailable;
            context.parameters[0].integer_values_equal = None;
            definition.parameter_context_proof =
                Some(MssqlParameterContextProof::for_retained(&source, &definition, context).unwrap());
            assert!(definition.is_valid());
            let error = crate::plan_xml::parse_retained_definition(&definition, &source).unwrap_err();
            assert_eq!(error.detail, Some(culprit_core::ErrorDetail::Delivery(DeliveryProblem::Integrity)));
        }
    }
}
