//! Controlled native retained evidence. These fixtures declare original facts and implement no plan classifier.
use super::*;
use culprit_core::plan_facts::{RelationalInput, RelationalInputKind, RelationalInputProvenance};

const QUERY_HASH: &str = "0x0123456789ABCDEF";
const SELECT: &str = "SELECT r.value, i.id FROM @Rows AS r JOIN dbo.Items AS i ON r.value = i.id WHERE r.value > 9264 AND i.is_active = 1";
const AGGREGATE: &str = "SELECT DISTINCT i.is_active FROM @Rows AS r JOIN dbo.Items AS i ON r.value = i.id WHERE r.value > 9264 AND i.is_active = 1";
const COUNTED_JOIN: &str = "SELECT COUNT_BIG(*) FROM @Rows AS r JOIN dbo.Items AS i ON r.value = i.id WHERE r.value > 9264 AND i.is_active = 1";
const COUNTED_INPUT: &str = "SELECT COUNT_BIG(*) FROM @Rows AS r WHERE (r.value > 9264)";
const SCALAR_INPUT: &str = "SELECT ABS(r.value) FROM @Rows AS r WHERE (r.value > 9264)";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TvpLoopCase {
    RepeatedAccess,
    HealthySeek,
    AccurateEstimate,
    MissingInnerRead,
    ParallelInner,
    FilteredAndAggregated,
    MissingAggregateBinding,
    CountedJoin,
    ConsumedInput,
    ScalarInput,
    ParallelAggregate,
    UnknownAggregateParallelism,
    MissingInputRead,
    HealthyCountedInput,
    MissingBinding,
    WrongBinding,
    Estimated,
    PartialSource,
}

impl TvpLoopCase {
    pub const ALL: [Self; 18] = [
        Self::RepeatedAccess,
        Self::HealthySeek,
        Self::AccurateEstimate,
        Self::MissingInnerRead,
        Self::ParallelInner,
        Self::FilteredAndAggregated,
        Self::MissingAggregateBinding,
        Self::CountedJoin,
        Self::ConsumedInput,
        Self::ScalarInput,
        Self::ParallelAggregate,
        Self::UnknownAggregateParallelism,
        Self::MissingInputRead,
        Self::HealthyCountedInput,
        Self::MissingBinding,
        Self::WrongBinding,
        Self::Estimated,
        Self::PartialSource,
    ];
}

pub fn tvp_loop_evidence_cases() -> Vec<(CollectionDescription, Vec<TelemetryRecord>)> {
    TvpLoopCase::ALL.into_iter().map(tvp_loop_evidence).collect()
}

/// Original module/statement/plan facts share one qualified captured source. Its present native reader owner is
/// independent from the retained run's execution population. No lifetime count substitutes for that run's counters.
pub fn tvp_loop_evidence(case: TvpLoopCase) -> (CollectionDescription, Vec<TelemetryRecord>) {
    let source = qualified_source(
        SnapshotDataset::RetainedPlans,
        1,
        if case == TvpLoopCase::PartialSource { SourceState::Partial } else { SourceState::Collected },
    );
    let sql = match case {
        TvpLoopCase::FilteredAndAggregated => AGGREGATE,
        TvpLoopCase::MissingAggregateBinding | TvpLoopCase::CountedJoin => COUNTED_JOIN,
        TvpLoopCase::ScalarInput => SCALAR_INPUT,
        _ if direct_input(case) => COUNTED_INPUT,
        _ => SELECT,
    };
    let mut cached = cached_definition();
    cached.statement = cached_payload(sql, DefinitionPayload::sql);
    cached.context.source_text_bytes = cached.statement.source_utf16_bytes.clone();
    let raw = document(case, sql);
    let mut native = MssqlRetainedPlanDefinition {
        context: cached.context,
        statement: cached.statement,
        module_parameters: cached.module_parameters,
        current_type_schema: None,
        relational_input_proof: None,
        parameter_context_proof: None,
        statement_executions: Some("9007199254740993".into()),
        last_start_without_offset: Some("2020-01-01T11:59:58.003".into()),
        database_last_query_plan_stats: Some(true),
        batch_plan: cached_payload(&raw, DefinitionPayload::plan),
    };
    let DefinitionPayload::Available { text, .. } = &native.batch_plan.payload else {
        panic!("bounded controlled XML")
    };
    assert!(text.len() > 1024 && text.len() < 32 * 1024, "native body must exceed preview without oversized padding");
    // Named JOIN replay needs original source-qualified binding evidence even when COUNT_BIG is recognized.
    // Available module metadata and runtime counters cannot reconstruct a redacted native namespace.
    if !matches!(case, TvpLoopCase::MissingBinding | TvpLoopCase::MissingAggregateBinding) {
        let bindings = vec![MssqlRelationalInputBinding {
            node_id: if case == TvpLoopCase::WrongBinding { 999 } else { 1 },
            input: RelationalInput {
                kind: RelationalInputKind::TableValuedParameter,
                provenance: RelationalInputProvenance::NativeParameterMetadata,
            },
        }];
        native.relational_input_proof =
            Some(MssqlRelationalInputProof::for_retained(&source, &native, bindings).unwrap());
    }
    let record = ObjectDefinitionRecord::new(
        "native-object-definitions".into(),
        ObjectDefinitionFact::MssqlRetainedPlan(Box::new(native)),
    )
    .unwrap();
    (source, vec![TelemetryRecord::ObjectDefinition(record)])
}

fn document(case: TvpLoopCase, sql: &str) -> String {
    let accurate = matches!(case, TvpLoopCase::AccurateEstimate | TvpLoopCase::HealthyCountedInput);
    let expected_output = if accurate { "10000" } else { "100" };
    let expected_access = if accurate { "20000" } else { "100" };
    let outer_reads = if case == TvpLoopCase::MissingInputRead { None } else { Some("20000") };
    let outer_runtime = runtime(case, "10000", outer_reads, "1", false);
    let inner_reads = match case {
        TvpLoopCase::HealthySeek => Some("10000"),
        TvpLoopCase::MissingInnerRead => None,
        _ => Some("2000000"),
    };
    let inner_runtime = runtime(case, "10000", inner_reads, "10000", true);
    let join_runtime = runtime(case, "10000", None, "1", false);
    let inner_parallel = if case == TvpLoopCase::ParallelInner { "true" } else { "false" };
    let output = if case == TvpLoopCase::FilteredAndAggregated {
        r#"<ColumnReference Database="[db]" Schema="[dbo]" Table="[Items]" Alias="[i]" Column="is_active" />"#
    } else {
        r#"<ColumnReference Table="[@Rows]" Alias="[r]" Column="value" /><ColumnReference Database="[db]" Schema="[dbo]" Table="[Items]" Alias="[i]" Column="id" />"#
    };
    let inner_output = if case == TvpLoopCase::FilteredAndAggregated {
        r#"<ColumnReference Database="[db]" Schema="[dbo]" Table="[Items]" Alias="[i]" Column="is_active" />"#
    } else {
        r#"<ColumnReference Database="[db]" Schema="[dbo]" Table="[Items]" Alias="[i]" Column="id" />"#
    };
    let outer = format!(
        r#"<RelOp NodeId="1" PhysicalOp="Table Scan" LogicalOp="Table Scan" Parallel="false" EstimateRows="{expected_output}" EstimatedRowsRead="{expected_access}" EstimatedTotalSubtreeCost="1">
        <OutputList><ColumnReference Table="[@Rows]" Alias="[r]" Column="value" /></OutputList>
        {outer_runtime}<TableScan Ordered="false" Storage="RowStore"><DefinedValues><DefinedValue><ColumnReference Table="[@Rows]" Alias="[r]" Column="value" /></DefinedValue></DefinedValues>
        <Object Table="[@Rows]" Alias="[r]" /><Predicate><ScalarOperator ScalarString="[r].[value]&gt;(9264)"><Compare CompareOp="GT"><ScalarOperator><Identifier><ColumnReference Table="[@Rows]" Alias="[r]" Column="value" /></Identifier></ScalarOperator><ScalarOperator><Const ConstValue="(9264)" /></ScalarOperator></Compare></ScalarOperator></Predicate>
        </TableScan></RelOp>"#
    );
    let body = if direct_input(case) {
        outer
    } else {
        format!(
            r#"<RelOp NodeId="0" PhysicalOp="Nested Loops" LogicalOp="Inner Join" Parallel="false" EstimateRows="100" EstimatedTotalSubtreeCost="30">
        <OutputList>{output}</OutputList>
        {join_runtime}<NestedLoops Optimized="false"><OuterReferences><ColumnReference Table="[@Rows]" Alias="[r]" Column="value" /></OuterReferences>{outer}
        <RelOp NodeId="2" PhysicalOp="Index Seek" LogicalOp="Index Seek" Parallel="{inner_parallel}" EstimateRows="1" EstimatedRowsRead="200" EstimatedTotalSubtreeCost="28">
        <OutputList>{inner_output}</OutputList>
        {inner_runtime}<IndexScan Ordered="true" Storage="RowStore"><Object Database="[db]" Schema="[dbo]" Table="[Items]" Index="[IX_Items_id]" Alias="[i]" IndexKind="NonClustered" />
        <Predicate><ScalarOperator ScalarString="[i].[is_active]=(1)"><Compare CompareOp="EQ"><ScalarOperator><Identifier><ColumnReference Database="[db]" Schema="[dbo]" Table="[Items]" Alias="[i]" Column="is_active" /></Identifier></ScalarOperator><ScalarOperator><Const ConstValue="(1)" /></ScalarOperator></Compare></ScalarOperator></Predicate>
        <SeekPredicates><SeekPredicateNew><SeekKeys><Prefix ScanType="EQ"><RangeColumns><ColumnReference Database="[db]" Schema="[dbo]" Table="[Items]" Alias="[i]" Column="id" /></RangeColumns><RangeExpressions><ScalarOperator ScalarString="[r].[value]"><Identifier><ColumnReference Table="[@Rows]" Alias="[r]" Column="value" /></Identifier></ScalarOperator></RangeExpressions></Prefix></SeekKeys></SeekPredicateNew></SeekPredicates>
        </IndexScan></RelOp></NestedLoops></RelOp>"#
        )
    };
    let body = if case == TvpLoopCase::ScalarInput {
        let runtime = runtime(case, "10000", None, "1", false);
        format!(
            r#"<RelOp NodeId="3" PhysicalOp="Compute Scalar" LogicalOp="Compute Scalar" Parallel="false" EstimateRows="100" EstimatedTotalSubtreeCost="2"><OutputList><ColumnReference Column="Expr1000" /></OutputList>{runtime}<ComputeScalar><DefinedValues><DefinedValue><ColumnReference Column="Expr1000" /><ScalarOperator ScalarString="abs([r].[value])"><Intrinsic FunctionName="abs"><ScalarOperator><Identifier><ColumnReference Table="[@Rows]" Alias="[r]" Column="value" /></Identifier></ScalarOperator></Intrinsic></ScalarOperator></DefinedValue></DefinedValues>{body}</ComputeScalar></RelOp>"#
        )
    } else if direct_input(case)
        || matches!(
            case,
            TvpLoopCase::FilteredAndAggregated | TvpLoopCase::MissingAggregateBinding | TvpLoopCase::CountedJoin
        )
    {
        let runtime = runtime(case, "1", None, "1", false);
        let output = if case == TvpLoopCase::FilteredAndAggregated {
            r#"<ColumnReference Database="[db]" Schema="[dbo]" Table="[Items]" Alias="[i]" Column="is_active" />"#
        } else {
            r#"<ColumnReference Column="Expr1000" />"#
        };
        let grouping = if case == TvpLoopCase::FilteredAndAggregated {
            format!("<DefinedValues /><GroupBy>{output}</GroupBy>")
        } else {
            r#"<DefinedValues><DefinedValue><ColumnReference Column="Expr1000" /><ScalarOperator ScalarString="COUNT_BIG(*)"><Aggregate AggType="countstar" Distinct="false" /></ScalarOperator></DefinedValue></DefinedValues>"#.into()
        };
        let parallel = match case {
            TvpLoopCase::ParallelAggregate => "Parallel=\"true\"",
            TvpLoopCase::UnknownAggregateParallelism => "",
            _ => "Parallel=\"false\"",
        };
        format!(
            r#"<RelOp NodeId="3" PhysicalOp="Stream Aggregate" LogicalOp="Aggregate" {parallel} EstimateRows="1" EstimatedTotalSubtreeCost="31"><OutputList>{output}</OutputList>{runtime}<StreamAggregate>{grouping}{body}</StreamAggregate></RelOp>"#
        )
    } else {
        body
    };
    format!(
        r#"<ShowPlanXML Version="1.539" Build="16.0.1000.6" xmlns="http://schemas.microsoft.com/sqlserver/2004/07/showplan"><BatchSequence><Batch><Statements><StmtSimple StatementId="1" StatementText="{sql}" QueryHash="{QUERY_HASH}" StatementType="SELECT" StatementSubTreeCost="31"><QueryPlan>{body}</QueryPlan></StmtSimple></Statements></Batch></BatchSequence></ShowPlanXML>"#
    )
}

fn direct_input(case: TvpLoopCase) -> bool {
    matches!(
        case,
        TvpLoopCase::ConsumedInput
            | TvpLoopCase::ScalarInput
            | TvpLoopCase::ParallelAggregate
            | TvpLoopCase::UnknownAggregateParallelism
            | TvpLoopCase::MissingInputRead
            | TvpLoopCase::HealthyCountedInput
    )
}

fn runtime(case: TvpLoopCase, rows: &str, read: Option<&str>, executions: &str, inner: bool) -> String {
    if case == TvpLoopCase::Estimated {
        return String::new();
    }
    let read = read.map(|value| format!(r#" ActualRowsRead="{value}""#)).unwrap_or_default();
    let thread = if inner && case == TvpLoopCase::ParallelInner { "1" } else { "0" };
    format!(
        r#"<RunTimeInformation><RunTimeCountersPerThread Thread="{thread}" ActualRows="{rows}" ActualExecutions="{executions}" ActualEndOfScans="{executions}"{read} /></RunTimeInformation>"#
    )
}
