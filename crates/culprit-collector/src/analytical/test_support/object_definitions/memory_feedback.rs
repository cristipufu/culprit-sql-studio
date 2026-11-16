//! Controlled native-style retained plans. This fixture declares original evidence and never interprets feedback.
use super::*;

const STATEMENT: &str = "SELECT order_id FROM dbo.orders WHERE customer_id = 9264 ORDER BY order_id";
const QUERY_HASH: &str = "0x0123456789ABCDEF";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MemoryFeedbackCase {
    FirstExecution,
    AccurateGrant,
    DisabledUnstable,
    Adjusting,
    PercentileAdjusting,
    Stable,
    Unrecognized,
    Absent,
    InvalidState,
    InvalidPreviousGrant,
    LargePreviousGrant,
    SiblingOnly,
    Estimated,
    PartialSource,
    SourceUnavailable,
    PayloadUnavailable,
}

impl MemoryFeedbackCase {
    pub const ALL: [Self; 16] = [
        Self::FirstExecution,
        Self::AccurateGrant,
        Self::DisabledUnstable,
        Self::Adjusting,
        Self::PercentileAdjusting,
        Self::Stable,
        Self::Unrecognized,
        Self::Absent,
        Self::InvalidState,
        Self::InvalidPreviousGrant,
        Self::LargePreviousGrant,
        Self::SiblingOnly,
        Self::Estimated,
        Self::PartialSource,
        Self::SourceUnavailable,
        Self::PayloadUnavailable,
    ];
}

pub fn memory_feedback_evidence_cases() -> Vec<(CollectionDescription, Vec<TelemetryRecord>)> {
    MemoryFeedbackCase::ALL.into_iter().map(memory_feedback_evidence).collect()
}

/// Native cache identities bind the selected statement. Feedback and runtime counters belong only to its QueryPlan;
/// the qualified current capture member does not identify the historical execution population or a memory owner.
pub fn memory_feedback_evidence(case: MemoryFeedbackCase) -> (CollectionDescription, Vec<TelemetryRecord>) {
    let state = match case {
        MemoryFeedbackCase::PartialSource => SourceState::Partial,
        MemoryFeedbackCase::SourceUnavailable => SourceState::Unavailable,
        _ => SourceState::Collected,
    };
    let source =
        qualified_source(SnapshotDataset::RetainedPlans, usize::from(state != SourceState::Unavailable), state);
    if state == SourceState::Unavailable {
        return (source, vec![]);
    }
    let mut cached = cached_definition();
    cached.context.identity.plan_handle = "0x31415926".into();
    cached.context.identity.sql_handle = "0x53589793".into();
    cached.context.identity.created_without_offset = "2020-01-01T11:45:00.003".into();
    cached.context.query_hash = Some(QUERY_HASH.into());
    let full_batch = if case == MemoryFeedbackCase::SiblingOnly {
        cached.context.identity.statement_end_bytes = ((STATEMENT.encode_utf16().count() - 1) * 2) as i32;
        format!("{STATEMENT}; SELECT unrelated_value FROM dbo.unrelated")
    } else {
        STATEMENT.into()
    };
    cached.context.source_text_bytes = Some((full_batch.encode_utf16().count() * 2).to_string());
    let xml = document(case);
    assert!(xml.len() > 1024 && xml.len() < 32 * 1024, "a bounded native body exceeding warehouse previews");
    let native = MssqlRetainedPlanDefinition {
        context: cached.context,
        statement: cached_payload(STATEMENT, DefinitionPayload::sql),
        module_parameters: None,
        relational_input_proof: None,
        statement_executions: Some("7".into()),
        last_start_without_offset: Some("2020-01-01T11:59:58.003".into()),
        database_last_query_plan_stats: Some(true),
        batch_plan: if case == MemoryFeedbackCase::PayloadUnavailable {
            missing_payload(PayloadUnavailable::Restricted)
        } else {
            cached_payload(&xml, DefinitionPayload::plan)
        },
    };
    let record = ObjectDefinitionRecord::new(
        "native-object-definitions".into(),
        ObjectDefinitionFact::MssqlRetainedPlan(Box::new(native)),
    )
    .unwrap();
    (source, vec![TelemetryRecord::ObjectDefinition(record)])
}

fn document(case: MemoryFeedbackCase) -> String {
    let selected = statement(STATEMENT, QUERY_HASH, feedback_attributes(case), case != MemoryFeedbackCase::Estimated);
    let sibling = if case == MemoryFeedbackCase::SiblingOnly {
        statement(
            "SELECT unrelated_value FROM dbo.unrelated",
            "0xFEDCBA9876543210",
            feedback_attributes(MemoryFeedbackCase::DisabledUnstable),
            true,
        )
    } else {
        String::new()
    };
    format!(
        r#"<ShowPlanXML Version="1.539" Build="16.0.1000.6" xmlns="http://schemas.microsoft.com/sqlserver/2004/07/showplan"><BatchSequence><Batch><Statements>{selected}{sibling}</Statements></Batch></BatchSequence></ShowPlanXML>"#
    )
}

fn feedback_attributes(case: MemoryFeedbackCase) -> &'static str {
    use MemoryFeedbackCase::*;
    match case {
        FirstExecution => r#"IsMemoryGrantFeedbackAdjusted="No: First Execution" LastRequestedMemory="0""#,
        AccurateGrant => r#"IsMemoryGrantFeedbackAdjusted="No: Accurate Grant" LastRequestedMemory="65536""#,
        DisabledUnstable | Estimated => {
            r#"IsMemoryGrantFeedbackAdjusted="No: Feedback disabled" LastRequestedMemory="131072""#
        }
        Adjusting | PartialSource | PayloadUnavailable | SourceUnavailable => {
            r#"IsMemoryGrantFeedbackAdjusted="Yes: Adjusting" LastRequestedMemory="131072""#
        }
        PercentileAdjusting => {
            r#"IsMemoryGrantFeedbackAdjusted="Yes: Percentile Adjusting" LastRequestedMemory="131072""#
        }
        Stable => r#"IsMemoryGrantFeedbackAdjusted="Yes: Stable" LastRequestedMemory="65536""#,
        Unrecognized => r#"IsMemoryGrantFeedbackAdjusted="private-feedback-state-canary" LastRequestedMemory="65536""#,
        Absent | SiblingOnly => "",
        InvalidState => r#"IsMemoryGrantFeedbackAdjusted="" LastRequestedMemory="65536""#,
        InvalidPreviousGrant => r#"IsMemoryGrantFeedbackAdjusted="Yes: Adjusting" LastRequestedMemory="12.5""#,
        LargePreviousGrant => {
            r#"IsMemoryGrantFeedbackAdjusted="No: Feedback disabled" LastRequestedMemory="9007199254740993""#
        }
    }
}

fn statement(sql: &str, hash: &str, feedback: &str, actual: bool) -> String {
    let runtime = if actual {
        r#"<RunTimeInformation><RunTimeCountersPerThread Thread="0" ActualRows="512" ActualRowsRead="512" ActualExecutions="1" ActualEndOfScans="1" ActualCPUms="2" ActualElapsedms="3" /></RunTimeInformation>"#
    } else {
        ""
    };
    format!(
        r#"<StmtSimple StatementId="1" StatementText="{sql}" QueryHash="{hash}" StatementType="SELECT" StatementSubTreeCost="1.25" StatementEstRows="512">
        <QueryPlan CachedPlanSize="32" CompileTime="2" CompileCPU="2" CompileMemory="256">
        <MemoryGrantInfo SerialRequiredMemory="512" SerialDesiredMemory="65536" RequiredMemory="512" DesiredMemory="65536" RequestedMemory="65536" GrantWaitTime="0" GrantedMemory="65536" MaxUsedMemory="49152" MaxQueryMemory="1048576" {feedback} />
        <RelOp NodeId="0" PhysicalOp="Sort" LogicalOp="Sort" EstimateRows="512" EstimateIO="0.01" EstimateCPU="0.02" AvgRowSize="11" EstimatedTotalSubtreeCost="1.25" Parallel="false">{runtime}
        <OutputList><ColumnReference Database="[db]" Schema="[dbo]" Table="[orders]" Column="order_id" /></OutputList>
        <MemoryFractions Input="1" Output="1" /><Sort Distinct="false"><OrderBy><OrderByColumn Ascending="true"><ColumnReference Database="[db]" Schema="[dbo]" Table="[orders]" Column="order_id" /></OrderByColumn></OrderBy>
        <RelOp NodeId="1" PhysicalOp="Index Seek" LogicalOp="Index Seek" EstimateRows="512" EstimatedRowsRead="512" EstimatedTotalSubtreeCost="1" Parallel="false">{runtime}
        <OutputList><ColumnReference Database="[db]" Schema="[dbo]" Table="[orders]" Column="order_id" /></OutputList>
        <IndexScan Ordered="false" Storage="RowStore"><Object Database="[db]" Schema="[dbo]" Table="[orders]" Index="[ix_orders_customer_id]" IndexKind="NonClustered" />
        <SeekPredicates><SeekPredicateNew><SeekKeys><Prefix ScanType="EQ"><RangeColumns><ColumnReference Database="[db]" Schema="[dbo]" Table="[orders]" Column="customer_id" /></RangeColumns><RangeExpressions><ScalarOperator ScalarString="(9264)"><Const ConstValue="(9264)" /></ScalarOperator></RangeExpressions></Prefix></SeekKeys></SeekPredicateNew></SeekPredicates></IndexScan>
        </RelOp></Sort></RelOp></QueryPlan></StmtSimple>"#
    )
}
