//! Controlled original native scalar evidence shared by SDK and both warehouse adapters. No SQL is executed.
use super::*;
use culprit_core::plan_facts::{
    PlanParameter, PlanParameterComparison as Comparison, PlanParameterContext, PlanParameterIssue as Issue,
    PlanParameterUse, PlanParameterUseKind, PlanParameterValueState as State,
};

const HASH: &str = "0x0123456789ABCDEF";
const COMPILED: &str = "N'private-original-compiled-canary'";
const RUNTIME: &str = "N'private-original-runtime-canary'";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ParameterEvidenceCase {
    AccessMismatch,
    AccurateAccess,
    HealthyNativeVariant,
    SameRepresentation,
    UnusedDifferentParameter,
    UnsupportedConversion,
    MissingRuntimeParameter,
    Estimated,
    SiblingOnlyRuntime,
    UseLimit,
    PartialSource,
    SourceUnavailable,
    StatementUnavailable,
    PayloadUnavailable,
    RecordedStatisticsContext,
    RecordedGrantPressure,
}

impl ParameterEvidenceCase {
    pub const ALL: [Self; 16] = [
        Self::AccessMismatch,
        Self::AccurateAccess,
        Self::HealthyNativeVariant,
        Self::SameRepresentation,
        Self::UnusedDifferentParameter,
        Self::UnsupportedConversion,
        Self::MissingRuntimeParameter,
        Self::Estimated,
        Self::SiblingOnlyRuntime,
        Self::UseLimit,
        Self::PartialSource,
        Self::SourceUnavailable,
        Self::StatementUnavailable,
        Self::PayloadUnavailable,
        Self::RecordedStatisticsContext,
        Self::RecordedGrantPressure,
    ];
}

pub fn parameter_evidence_cases() -> Vec<(CollectionDescription, Vec<TelemetryRecord>)> {
    ParameterEvidenceCase::ALL.into_iter().map(parameter_evidence).collect()
}

pub fn parameter_evidence(case: ParameterEvidenceCase) -> (CollectionDescription, Vec<TelemetryRecord>) {
    let state = match case {
        ParameterEvidenceCase::PartialSource => SourceState::Partial,
        ParameterEvidenceCase::SourceUnavailable => SourceState::Unavailable,
        _ => SourceState::Collected,
    };
    let source =
        qualified_source(SnapshotDataset::RetainedPlans, usize::from(state != SourceState::Unavailable), state);
    if state == SourceState::Unavailable {
        return (source, vec![]);
    }
    let mut sql = format!(
        "SELECT order_id FROM dbo.orders WHERE customer = @customer /* private-statement-canary {} */",
        "bounded_native_text ".repeat(70)
    );
    if case == ParameterEvidenceCase::HealthyNativeVariant {
        sql.push_str(" option (PLAN PER VALUE(ObjectID = 0, QueryVariantID = 2, predicate_range([orders].[customer] = @customer, 100.0, 1000000.0)))");
    }
    let xml = document(case, &sql);
    let mut context = cached_definition().context;
    context.identity.sql_handle = "0x46504650".into();
    context.identity.plan_handle = "0x50465046".into();
    context.source_text_bytes = Some((sql.encode_utf16().count() * 2).to_string());
    let mut native = MssqlRetainedPlanDefinition {
        context,
        statement: if case == ParameterEvidenceCase::StatementUnavailable {
            missing_payload(PayloadUnavailable::Restricted)
        } else {
            cached_payload(&sql, DefinitionPayload::sql)
        },
        module_parameters: None,
        current_type_schema: None,
        relational_input_proof: None,
        parameter_context_proof: None,
        statement_executions: Some("9007199254740993".into()),
        last_start_without_offset: Some("2020-01-01T11:59:59.003".into()),
        database_last_query_plan_stats: Some(true),
        batch_plan: if case == ParameterEvidenceCase::PayloadUnavailable {
            missing_payload(PayloadUnavailable::Restricted)
        } else {
            cached_payload(&xml, DefinitionPayload::plan)
        },
    };
    if !matches!(case, ParameterEvidenceCase::PayloadUnavailable | ParameterEvidenceCase::StatementUnavailable) {
        native.parameter_context_proof =
            Some(MssqlParameterContextProof::for_retained(&source, &native, native_context(case)).unwrap());
    }
    let record = ObjectDefinitionRecord::new(
        "native-object-definitions".into(),
        ObjectDefinitionFact::MssqlRetainedPlan(Box::new(native)),
    )
    .unwrap();
    (source, vec![TelemetryRecord::ObjectDefinition(record)])
}

fn native_context(case: ParameterEvidenceCase) -> PlanParameterContext {
    let mut parameter = PlanParameter {
        integer_values_equal: None,
        name: Some("@customer".into()),
        data_type: Some("nvarchar(4000)".into()),
        compiled: State::Recorded,
        runtime: State::Recorded,
        comparison: Comparison::DifferentRepresentation,
    };
    if case == ParameterEvidenceCase::HealthyNativeVariant {
        parameter.data_type = Some("int".into());
    }
    if matches!(case, ParameterEvidenceCase::SameRepresentation | ParameterEvidenceCase::UnusedDifferentParameter) {
        parameter.comparison = Comparison::SameRepresentation;
    }
    if case == ParameterEvidenceCase::MissingRuntimeParameter {
        parameter.runtime = State::NotRecorded;
        parameter.comparison = Comparison::Unavailable;
    }
    if matches!(case, ParameterEvidenceCase::Estimated | ParameterEvidenceCase::SiblingOnlyRuntime) {
        parameter.runtime = State::Unqualified;
        parameter.comparison = Comparison::Unavailable;
    }
    let mut context = PlanParameterContext {
        retrieved_from_cache: None,
        parameters: vec![parameter],
        uses: vec![PlanParameterUse { parameter_index: 0, node_id: 7, kind: PlanParameterUseKind::EqualitySeek }],
        issues: vec![],
    };
    if case == ParameterEvidenceCase::UnusedDifferentParameter {
        context.parameters.push(PlanParameter {
            integer_values_equal: None,
            name: Some("@unused".into()),
            comparison: Comparison::DifferentRepresentation,
            ..context.parameters[0].clone()
        });
    }
    if case == ParameterEvidenceCase::UnsupportedConversion {
        context.uses.clear();
        context.issues.push(Issue::UnsupportedUse);
    }
    if case == ParameterEvidenceCase::UseLimit {
        context.uses.clear();
        context.issues.push(Issue::UseLimit);
    }
    context
}

fn document(case: ParameterEvidenceCase, sql: &str) -> String {
    let selected = statement(case, sql, HASH);
    let sibling = if case == ParameterEvidenceCase::SiblingOnlyRuntime {
        statement(
            ParameterEvidenceCase::AccessMismatch,
            "SELECT other_id FROM dbo.other_rows WHERE other_id=@customer",
            "0xFEDCBA9876543210",
        )
    } else {
        String::new()
    };
    format!(
        "<ShowPlanXML><BatchSequence><Batch><Statements>{selected}{sibling}</Statements></Batch></BatchSequence></ShowPlanXML>"
    )
}

fn statement(case: ParameterEvidenceCase, sql: &str, hash: &str) -> String {
    let compiled = if case == ParameterEvidenceCase::HealthyNativeVariant { "(100)" } else { COMPILED };
    let runtime_value = if case == ParameterEvidenceCase::HealthyNativeVariant {
        "(1000)"
    } else if matches!(
        case,
        ParameterEvidenceCase::SameRepresentation | ParameterEvidenceCase::UnusedDifferentParameter
    ) {
        COMPILED
    } else {
        RUNTIME
    };
    let data_type = if case == ParameterEvidenceCase::HealthyNativeVariant { "int" } else { "nvarchar(4000)" };
    let runtime_attribute = if case == ParameterEvidenceCase::MissingRuntimeParameter {
        String::new()
    } else {
        format!("ParameterRuntimeValue=\"{runtime_value}\"")
    };
    let unused = if case == ParameterEvidenceCase::UnusedDifferentParameter {
        format!(
            r#"<ColumnReference Column="@unused" ParameterDataType="nvarchar(4000)" ParameterCompiledValue="{COMPILED}" ParameterRuntimeValue="{RUNTIME}"/>"#
        )
    } else {
        String::new()
    };
    let operators = if case == ParameterEvidenceCase::UseLimit {
        let children =
            (0..=culprit_core::plan_facts::MAX_PLAN_PARAMETER_USES).map(|id| seek(case, id as u32)).collect::<String>();
        format!(
            r#"<RelOp NodeId="4294967295" PhysicalOp="Concatenation" LogicalOp="Concatenation" EstimateRows="650000"><Concat>{children}</Concat></RelOp>"#
        )
    } else {
        seek(case, 7)
    };
    let statistics = if case == ParameterEvidenceCase::RecordedStatisticsContext {
        r#"<OptimizerStatsUsage><StatisticsInfo Database="[app]" Schema="[dbo]" Table="[orders]" Statistics="[ix_customer]" LastUpdate="2020-01-01T11:45:00.003" ModificationCount="0" SamplingPercent="100"/></OptimizerStatsUsage>"#
    } else {
        ""
    };
    let grant = if case == ParameterEvidenceCase::RecordedGrantPressure {
        r#"<MemoryGrantInfo SerialRequiredMemory="256" SerialDesiredMemory="1024" RequiredMemory="256" RequestedMemory="1024" GrantedMemory="1024" MaxUsedMemory="1024" GrantWaitTime="2"/>"#
    } else {
        ""
    };
    format!(
        r#"<StmtSimple QueryHash="{hash}" StatementText="{sql}" StatementType="SELECT" StatementSubTreeCost="1"><QueryPlan>{statistics}{grant}
<ParameterList><ColumnReference Column="@customer" ParameterDataType="{data_type}" ParameterCompiledValue="{compiled}" {runtime_attribute}/>{unused}</ParameterList>
{operators}</QueryPlan></StmtSimple>"#
    )
}

fn seek(case: ParameterEvidenceCase, id: u32) -> String {
    let estimated =
        if matches!(case, ParameterEvidenceCase::AccurateAccess | ParameterEvidenceCase::HealthyNativeVariant) {
            "10000"
        } else {
            "100"
        };
    let runtime = if matches!(case, ParameterEvidenceCase::Estimated | ParameterEvidenceCase::SiblingOnlyRuntime) {
        ""
    } else {
        r#"<RunTimeInformation><RunTimeCountersPerThread Thread="0" ActualRows="10000" ActualRowsRead="10000" ActualExecutions="1"/></RunTimeInformation>"#
    };
    let direct = r#"<Identifier><ColumnReference Column="@customer"/></Identifier>"#;
    let expression = if case == ParameterEvidenceCase::UnsupportedConversion {
        format!("<Convert>{direct}</Convert>")
    } else {
        direct.into()
    };
    format!(
        r#"<RelOp NodeId="{id}" PhysicalOp="Index Seek" LogicalOp="Index Seek" Parallel="false" EstimateRows="{estimated}" EstimatedRowsRead="{estimated}" EstimatedTotalSubtreeCost="1">
{runtime}<IndexScan><Object Database="[app]" Schema="[dbo]" Table="[orders]" Index="[ix_customer]"/>
<SeekPredicates><SeekPredicateNew><SeekKeys><Prefix ScanType="EQ"><RangeColumns><ColumnReference Table="[orders]" Column="customer"/></RangeColumns>
<RangeExpressions><ScalarOperator ScalarString="[@customer]">{expression}</ScalarOperator></RangeExpressions>
</Prefix></SeekKeys></SeekPredicateNew></SeekPredicates></IndexScan></RelOp>"#
    )
}
