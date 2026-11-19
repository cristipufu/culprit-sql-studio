//! Source-owned retained XML shared by both warehouse readers and the offline plan interpreter.
//! Counts model native runtime attributes, not an executed workload or measured Hyperscale qualification.
use super::*;

const HASH: &str = "0x0123456789ABCDEF";
const OTHER_HASH: &str = "0xFEDCBA9876543210";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PageServerIoCase {
    Complete,
    Zero,
    PartialThreads,
    InvalidCounter,
    DuplicateThread,
    Overflow,
    Absent,
    Estimated,
    SiblingOnlyRuntime,
    MissingNativeStatement,
    AmbiguousNativeStatement,
    UnqualifiedOwner,
    PartialSource,
    SourceUnavailable,
    PayloadUnavailable,
}
impl PageServerIoCase {
    pub const ALL: [Self; 15] = [
        Self::Complete,
        Self::Zero,
        Self::PartialThreads,
        Self::InvalidCounter,
        Self::DuplicateThread,
        Self::Overflow,
        Self::Absent,
        Self::Estimated,
        Self::SiblingOnlyRuntime,
        Self::MissingNativeStatement,
        Self::AmbiguousNativeStatement,
        Self::UnqualifiedOwner,
        Self::PartialSource,
        Self::SourceUnavailable,
        Self::PayloadUnavailable,
    ];
}

pub fn page_server_io_evidence(case: PageServerIoCase) -> (CollectionDescription, Vec<TelemetryRecord>) {
    let state = match case {
        PageServerIoCase::PartialSource => SourceState::Partial,
        PageServerIoCase::SourceUnavailable => SourceState::Unavailable,
        _ => SourceState::Collected,
    };
    let mut source =
        qualified_source(SnapshotDataset::RetainedPlans, usize::from(state != SourceState::Unavailable), state);
    if case == PageServerIoCase::UnqualifiedOwner {
        source.partition = SourcePartition::Target;
    }
    if state == SourceState::Unavailable {
        return (source, vec![]);
    }
    let sql = format!(
        "SELECT order_id FROM dbo.orders WHERE customer_id=9264 /* private-page-sql-canary {} */",
        "bounded_original_statement ".repeat(60),
    );
    let mut context = cached_definition().context;
    context.identity.plan_handle = "0x5041474553".into();
    context.identity.sql_handle = "0x534552564552".into();
    context.source_text_bytes = Some((sql.encode_utf16().count() * 2).to_string());
    let native = MssqlRetainedPlanDefinition {
        context,
        statement: cached_payload(&sql, DefinitionPayload::sql),
        module_parameters: None,
        current_type_schema: None,
        relational_input_proof: None,
        parameter_context_proof: None,
        // This is independent cache history, deliberately different from ActualExecutions in the XML.
        statement_executions: Some("9007199254740993".into()),
        last_start_without_offset: Some("2020-01-01T11:59:00.003".into()),
        database_last_query_plan_stats: Some(true),
        batch_plan: if case == PageServerIoCase::PayloadUnavailable {
            missing_payload(PayloadUnavailable::Restricted)
        } else {
            cached_payload(&document(case, &sql), DefinitionPayload::plan)
        },
    };
    assert!(native.is_valid());
    let record = ObjectDefinitionRecord::new(
        "native-object-definitions".into(),
        ObjectDefinitionFact::MssqlRetainedPlan(Box::new(native)),
    )
    .unwrap();
    (source, vec![TelemetryRecord::ObjectDefinition(record)])
}

fn document(case: PageServerIoCase, sql: &str) -> String {
    let selected_hash = if case == PageServerIoCase::MissingNativeStatement { OTHER_HASH } else { HASH };
    let mut statements = statement(selected_hash, sql, &population(case));
    if matches!(case, PageServerIoCase::SiblingOnlyRuntime | PageServerIoCase::AmbiguousNativeStatement) {
        let hash = if case == PageServerIoCase::AmbiguousNativeStatement { HASH } else { OTHER_HASH };
        statements.push_str(&statement(hash, sql, &population(PageServerIoCase::Complete)));
    }
    format!(
        "<ShowPlanXML><BatchSequence><Batch><Statements>{statements}</Statements></Batch></BatchSequence></ShowPlanXML>"
    )
}

fn statement(hash: &str, sql: &str, runtime: &str) -> String {
    // A real structural body larger than the inline preview, without padding private SQL into display fields.
    let columns = (0..48)
        .map(|column| format!(r#"<ColumnReference Schema="[dbo]" Table="[orders]" Column="output_{column}" />"#))
        .collect::<String>();
    format!(
        r#"<StmtSimple QueryHash="{hash}" StatementText="{sql}" StatementSubTreeCost="1"><QueryPlan>
        <RelOp NodeId="7" PhysicalOp="Index Scan" LogicalOp="Index Scan" EstimateRows="10" EstimatedTotalSubtreeCost="1">
        <OutputList>{columns}</OutputList>{runtime}<IndexScan><Object Schema="[dbo]" Table="[orders]" Index="[ix_orders]" /></IndexScan>
        </RelOp></QueryPlan></StmtSimple>"#
    )
}

fn population(case: PageServerIoCase) -> String {
    if matches!(case, PageServerIoCase::Estimated | PageServerIoCase::SiblingOnlyRuntime) {
        return String::new();
    }
    let counters = match case {
        PageServerIoCase::Zero => thread("0", [Some("0"); 4]),
        PageServerIoCase::Absent => thread("0", [None; 4]),
        PageServerIoCase::PartialThreads => {
            thread("1", [None, Some("0"), Some("2"), None]) + &thread("2", [Some("9"), Some("0"), Some("3"), None])
        }
        PageServerIoCase::InvalidCounter => thread("0", [Some("-1"), Some("0"), Some("2"), Some("3")]),
        PageServerIoCase::DuplicateThread => thread("1", [Some("1"); 4]) + &thread("1", [Some("2"); 4]),
        PageServerIoCase::Overflow => {
            thread("1", [Some("18446744073709551615"), Some("0"), Some("2"), Some("3")])
                + &thread("2", [Some("1"), Some("0"), Some("2"), Some("3")])
        }
        _ => {
            thread("1", [Some("9007199254740993"), Some("2"), Some("3"), Some("0")])
                + &thread("2", [Some("2"), Some("5"), Some("7"), Some("0")])
        }
    };
    format!("<RunTimeInformation>{counters}</RunTimeInformation>")
}
fn thread(id: &str, values: [Option<&str>; 4]) -> String {
    let mut attributes = String::new();
    for (key, value) in [
        "ActualPageServerReads",
        "ActualPageServerReadAheads",
        "ActualLobPageServerReads",
        "ActualLobPageServerReadAheads",
    ]
    .into_iter()
    .zip(values)
    {
        if let Some(value) = value {
            attributes.push_str(&format!(r#" {key}="{value}""#));
        }
    }
    format!(
        r#"<RunTimeCountersPerThread Thread="{id}" ActualRows="10" ActualRowsRead="10" ActualExecutions="1" {attributes} />"#
    )
}
