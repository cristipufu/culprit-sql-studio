//! What actually happened, read from DMVs only: the last actual plan SQL Server kept for a query
//! (sys.dm_exec_query_plan_stats, 2019+) and the live progress of a running request (sys.dm_exec_query_profiles and
//! sys.dm_exec_query_statistics_xml). Nothing here runs the user's statement.

use culprit_core::{
    Error, ErrorCode, NodeProgress, NodeState, Plan, PlanKind, Result, t,
    telemetry::{CollectionStop, RetainedActualPlan, RetainedActualPlanRequest, RetainedPlanProvenance},
};

mod layout;
mod progress;
pub(crate) use progress::query_progress;
#[cfg(test)]
mod native_tests;
pub(crate) mod retained;
use crate::conn::{Pool, permission_hint};
use crate::plan_xml;
use crate::read::NativeRead;
use crate::value::Rec;
pub(crate) use layout::{Availability, Layout};
use std::sync::Arc;

const SETTING_SQL: &str =
    "SELECT CAST(value AS int) AS value FROM sys.database_scoped_configurations WHERE name = 'LAST_QUERY_PLAN_STATS'";

const TURN_ON: &str = "ALTER DATABASE SCOPED CONFIGURATION SET LAST_QUERY_PLAN_STATS = ON;";

/// A Query Store id that isn't one.
pub(crate) fn ids_are_numbers() -> Error {
    Error::invalid(t!("provider.mssql.query_store_ids"))
}

fn numeric(id: &str) -> Result<&str> {
    let id = id.trim();
    if id.is_empty() || id.len() > 19 || !id.bytes().all(|b| b.is_ascii_digit()) {
        return Err(ids_are_numbers());
    }
    Ok(id)
}

/// The plan cache can't be read with this login.
pub(crate) fn cache_unreadable() -> Error {
    Error::new(ErrorCode::PermissionDenied, t!("provider.mssql.actual.cache_unreadable")).with_hint(permission_hint())
}

/// The server predates sys.dm_exec_query_plan_stats.
pub(crate) fn needs_2019() -> Error {
    Error::new(ErrorCode::Unsupported, t!("provider.mssql.actual.needs_2019"))
        .with_hint(t!("provider.mssql.actual.needs_2019.hint"))
}

/// LAST_QUERY_PLAN_STATS is off, or the cached plan holds no runtime counters.
pub(crate) fn not_kept() -> Error {
    Error::new(ErrorCode::Unsupported, t!("provider.mssql.actual.not_kept"))
        .with_hint(t!("provider.mssql.actual.not_kept.hint", statement = TURN_ON))
        .with_detail(culprit_core::ErrorDetail::ActualPlanStatisticsUnavailable)
}

/// No retained plan can be linked to this Query Store identity; absence alone cannot establish eviction.
pub(crate) fn no_matching_plan() -> Error {
    Error::not_found(t!("provider.mssql.actual.no_matching_plan"))
        .with_hint(t!("provider.mssql.actual.no_matching_plan.hint"))
}

/// Read the retained actual plan independently of Query Store's runtime population or observation window.
pub(crate) async fn last_actual_plan(
    pool: &Arc<Pool>,
    host: &str,
    port: u16,
    request: &RetainedActualPlanRequest,
) -> Result<RetainedActualPlan> {
    retained::validate_request(request)?;
    let maximum = crate::read::ReadSession::application_budget().max_plan_bytes as usize;
    let host = host.to_owned();
    let request = request.clone();
    pool.observe(|reader| Box::pin(async move { read_retained_plan(reader, &host, port, &request, maximum).await }))
        .await
}

async fn read_retained_plan<R: NativeRead>(
    reader: &mut R,
    host: &str,
    port: u16,
    request: &RetainedActualPlanRequest,
    maximum: usize,
) -> Result<RetainedActualPlan> {
    let before = crate::local_source::Context::read(reader, host, port).await?;
    if let Some(expected) = &request.expected_target {
        let target = culprit_core::telemetry::CollectionTarget::from(before.activity.database.clone());
        if expected.telemetry_id()? != target.telemetry_id()? {
            return Err(CollectionStop::SourceChanged.error());
        }
    }
    let read = retained::read(reader, request, maximum).await?;
    let after = before.recheck(reader, host, port).await?;
    let context = before.completed(&after)?;
    let Some(row) = read.plan.as_ref() else {
        return Err(if read.setting_on == Some(false) { not_kept() } else { no_matching_plan() });
    };
    if row.context.database_id != before.database_id {
        return Err(CollectionStop::SourceChanged.error());
    }
    let provenance = RetainedPlanProvenance {
        read: context,
        statement: row.context.clone(),
        query: row.query.clone(),
        native_plan: row.native_plan.clone(),
    };
    let plan = plan_from_retained(read, &provenance.read)?;
    Ok(RetainedActualPlan { plan, provenance })
}

fn plan_from_retained(read: retained::Read, context: &culprit_core::telemetry::NativeReadContext) -> Result<Plan> {
    let Some(row) = read.plan else {
        return Err(if read.setting_on == Some(false) { not_kept() } else { no_matching_plan() });
    };
    let xml = match row.payload {
        crate::query_store::payload::Payload::Text(xml) => xml,
        crate::query_store::payload::Payload::Unavailable(culprit_core::telemetry::PayloadUnavailable::Oversized) => {
            return Err(culprit_core::telemetry::CollectionStop::ByteLimit.error());
        }
        crate::query_store::payload::Payload::Unavailable(
            culprit_core::telemetry::PayloadUnavailable::NotCollected
            | culprit_core::telemetry::PayloadUnavailable::SourceUnavailable,
        ) => return Err(no_matching_plan()),
        crate::query_store::payload::Payload::Unavailable(_) => return Err(not_kept()),
    };
    let mut plan = plan_xml::parse_cached_statement(
        &xml,
        plan_xml::CachedInputContext {
            statement: &row.context,
            module_parameters: row.module_parameters.as_ref(),
            text: row.statement.text(),
            origin: plan_xml::PlanInputOrigin::OriginalNative { read: Some(context) },
        },
    )?;
    if plan.kind != PlanKind::Actual {
        return Err(not_kept());
    }
    if row.current_type_schema.as_ref().is_some_and(|schema| {
        !culprit_core::telemetry::ObservationBounds::new(&context.observed_from, &context.observed_until)
            .is_some_and(|bounds| bounds.contains(&schema.observed_at))
    }) {
        return Err(culprit_core::telemetry::CollectionStop::SourceChanged.error());
    }
    plan.facts_mut().current_type_schema = row.current_type_schema.map(Box::new);
    plan.raw = None;
    clear_unrecorded_times(&mut plan);
    Ok(plan)
}

// Lightweight profiling keeps row counts for past runs, not time. Withdraw its placeholder zeros in both models.
pub(crate) fn clear_unrecorded_times(plan: &mut Plan) {
    let facts = plan.facts_mut();
    if facts.elapsed_ms == Some(0.0) {
        facts.elapsed_ms = None;
    }
    if facts.cpu_ms == Some(0.0) {
        facts.cpu_ms = None;
    }
    for key in ["ElapsedMs", "CpuMs"] {
        if plan.props.get(key).is_some_and(|v| v == "0") {
            plan.props.remove(key);
        }
    }
}

/// Why a running statement shows no per-operator progress.
pub(crate) fn no_profiling() -> String {
    t!("provider.mssql.progress.no_profiling")
}

/// What a command's own percentage (a backup, an index rebuild) is based on.
pub(crate) fn own_estimate(command: &str) -> String {
    t!("provider.mssql.progress.own_estimate", command = command)
}

fn metadata_unavailable() -> Error {
    Error::new(ErrorCode::PermissionDenied, t!("provider.mssql.actual.metadata_unavailable"))
        .with_hint("GRANT VIEW DEFINITION TO [<user>];")
}

struct Counters {
    rows: f64,
    est: f64,
    elapsed: Option<f64>,
    cpu: Option<f64>,
    reads: Option<f64>,
    end_of_scans: f64,
    replays: f64,
    closed: bool,
}

fn counters(r: &Rec) -> Counters {
    Counters {
        rows: r.f64_or_zero("row_count"),
        est: r.f64_or_zero("estimate_row_count"),
        elapsed: r.f64("elapsed_ms"),
        cpu: r.f64("cpu_ms"),
        reads: r.f64("logical_reads"),
        end_of_scans: r.f64_or_zero("end_of_scans"),
        replays: r.f64_or_zero("replays"),
        closed: r.f64("closed") == Some(1.0),
    }
}

fn producing(c: &Counters) -> bool {
    c.rows > 0.0 || c.elapsed.is_some_and(|t| t > 0.0) || c.cpu.is_some_and(|t| t > 0.0)
}

/// An operator ran to completion when every thread closed it, or it finished its scan without being re-executed, or
/// its inputs are done and it produced (about) what was estimated.
fn finished(c: &Counters, est: f64, inputs_done: bool) -> bool {
    // A re-executed operator (the inner side of a loop) is only done when its driver is; its counters can't say.
    c.closed
        || (c.replays <= 1.0
            && ((c.end_of_scans >= 1.0 && c.rows >= est * 0.99) || (inputs_done && est >= 1.0 && c.rows >= est * 0.9)))
}

fn node_progress(profiles: &[Rec], plan: Option<&Plan>) -> Vec<NodeProgress> {
    let raw: std::collections::HashMap<u32, Counters> =
        profiles.iter().filter_map(|r| Some((r.f64("node_id")? as u32, counters(r)))).collect();
    let progress = |id: u32, c: &Counters, est: f64, state: NodeState| NodeProgress {
        node_id: id,
        state,
        rows: c.rows,
        est_rows: est,
        elapsed_ms: c.elapsed,
        cpu_ms: c.cpu,
        logical_reads: c.reads,
    };
    let Some(plan) = plan else {
        // No plan to place operators in: judge each on its own counters.
        return raw
            .iter()
            .map(|(id, c)| {
                let state = if finished(c, c.est, true) {
                    NodeState::Done
                } else if producing(c) {
                    NodeState::Running
                } else {
                    NodeState::Pending
                };
                progress(*id, c, c.est, state)
            })
            .collect();
    };
    fn visit(
        node: &culprit_core::PlanNode,
        raw: &std::collections::HashMap<u32, Counters>,
        out: &mut Vec<(u32, NodeState, f64)>,
    ) -> NodeState {
        let inputs: Vec<NodeState> = node.children.iter().map(|c| visit(c, raw, out)).collect();
        let Some(c) = raw.get(&node.id) else {
            // Operators lightweight profiling doesn't track (Compute Scalar) follow their inputs.
            let state = if inputs.iter().all(|s| *s == NodeState::Done) && !inputs.is_empty() {
                NodeState::Done
            } else if inputs.iter().any(|s| *s != NodeState::Pending) {
                NodeState::Running
            } else {
                NodeState::Pending
            };
            return state;
        };
        let est = node.est_rows_total().max(c.est);
        let inputs_done = inputs.iter().all(|s| *s == NodeState::Done);
        let state = if finished(c, est, inputs_done) {
            NodeState::Done
        } else if producing(c) || inputs.iter().any(|s| *s != NodeState::Pending) {
            NodeState::Running
        } else {
            NodeState::Pending
        };
        out.push((node.id, state, est));
        state
    }
    let mut states = Vec::new();
    visit(&plan.root, &raw, &mut states);
    states.into_iter().filter_map(|(id, state, est)| raw.get(&id).map(|c| progress(id, c, est, state))).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    pub(in crate::actual) fn retained_row() -> retained::RetainedPlan {
        let xml = r#"<ShowPlanXML><BatchSequence><Batch><Statements><StmtSimple StatementText="SELECT id FROM @Rows" QueryHash="0x0123456789ABCDEF" StatementSubTreeCost="1"><QueryPlan><RelOp NodeId="0" PhysicalOp="Table Scan" LogicalOp="Table Scan" EstimateRows="100" EstimatedTotalSubtreeCost="1"><RunTimeInformation><RunTimeCountersPerThread Thread="0" ActualRows="10000" ActualExecutions="1" /></RunTimeInformation><TableScan><Object Table="[@Rows]" /></TableScan></RelOp></QueryPlan></StmtSimple></Statements></Batch></BatchSequence></ShowPlanXML>"#;
        retained::RetainedPlan {
            context: crate::cached_statements::context(&crate::grants::tests::row()).unwrap(),
            query: None,
            native_plan: None,
            statement: crate::query_store::payload::Payload::Unavailable(
                culprit_core::telemetry::PayloadUnavailable::NotCollected,
            ),
            statement_utf16_bytes: None,
            module_parameters: None,
            current_type_schema: None,
            source_utf16_bytes: Some(xml.encode_utf16().count() * 2),
            payload: crate::query_store::payload::Payload::Text(xml.into()),
            executions: "9007199254740993".into(),
            last_start_without_offset: Some("2020-01-01T10:01:00.0070000".into()),
        }
    }

    #[test]
    fn retained_runtime_rows_do_not_borrow_the_cache_counters_or_last_start_as_their_execution_identity() {
        let row = retained_row();
        assert_eq!(row.executions, "9007199254740993");
        assert!(row.last_start_without_offset.is_some());
        let plan = plan_from_retained(
            retained::Read { setting_on: Some(false), plan: Some(row) },
            &crate::plan_xml::test_read_context("app"),
        )
        .unwrap();
        assert_eq!(plan.kind, PlanKind::Actual);
        assert_eq!(plan.root.act_rows, Some(10000.0));
        assert!(plan.facts().last_run_at.is_none());
        assert!(!plan.props.contains_key("LastRunAt"));
        assert!(!plan.props.contains_key("CachedExecutions"));
        assert!(culprit_core::actual_summary(&plan).unwrap().last_run_at.is_none());
    }

    #[test]
    fn an_unverified_retained_payload_does_not_suggest_enabling_statistics() {
        let mut row = retained_row();
        row.payload = crate::query_store::payload::Payload::Unavailable(
            culprit_core::telemetry::PayloadUnavailable::SourceUnavailable,
        );
        let error = plan_from_retained(
            retained::Read { setting_on: Some(true), plan: Some(row) },
            &crate::plan_xml::test_read_context("app"),
        )
        .unwrap_err();
        assert_eq!(error.code, ErrorCode::NotFound);
        assert!(!error.hint.unwrap_or_default().contains(TURN_ON));
    }

    #[test]
    fn retained_type_metadata_keeps_its_point_inside_the_original_native_read() {
        use culprit_core::telemetry::{CollectionStop, MssqlCurrentTypeSchema, MssqlCurrentTypeSchemaStatus};
        let context = crate::plan_xml::test_read_context("app");
        for (clock, accepted) in
            [("2026-11-14T11:59:59Z", false), ("2026-11-14T12:00:00.5Z", true), ("2026-11-14T12:00:02Z", false)]
        {
            let mut row = retained_row();
            row.current_type_schema =
                Some(MssqlCurrentTypeSchema::unavailable(clock.into(), MssqlCurrentTypeSchemaStatus::Restricted));
            let result = plan_from_retained(retained::Read { setting_on: Some(true), plan: Some(row) }, &context);
            if accepted {
                assert_eq!(result.unwrap().facts().current_type_schema.as_ref().unwrap().observed_at, clock);
            } else {
                assert_eq!(CollectionStop::from_error(&result.unwrap_err()), CollectionStop::SourceChanged);
            }
        }
    }

    #[test]
    fn lightweight_profile_zero_times_withdraw_typed_placeholders_and_keep_measured_fallbacks() {
        let snapshot: culprit_core::Snapshot =
            serde_json::from_str(include_str!("../../../../fixtures/mssql-demo.json")).unwrap();
        let mut plan = snapshot.queries()[0].plan.as_ref().unwrap().clone();
        plan.kind = PlanKind::Actual;
        plan.root.children.clear();
        plan.root.act_time_ms = Some(10.0);
        plan.root.runtime = Some(culprit_core::NodeRuntime { cpu_ms: Some(2.0), ..Default::default() });
        plan.facts_mut().elapsed_ms = Some(0.0);
        plan.facts_mut().cpu_ms = Some(0.0);
        plan.props.insert("ElapsedMs".into(), "0".into());
        plan.props.insert("CpuMs".into(), "0".into());
        clear_unrecorded_times(&mut plan);
        let actual = culprit_core::actual_summary(&plan).unwrap();
        assert_eq!((actual.elapsed_ms, actual.cpu_ms), (Some(10.0), Some(2.0)));
        plan.facts_mut().elapsed_ms = Some(8.0);
        plan.facts_mut().cpu_ms = Some(3.0);
        clear_unrecorded_times(&mut plan);
        assert_eq!((plan.facts().elapsed_ms, plan.facts().cpu_ms), (Some(8.0), Some(3.0)));
    }

    fn counters(rows: f64, est: f64, end_of_scans: f64, replays: f64, elapsed: f64) -> Counters {
        Counters { rows, est, elapsed: Some(elapsed), cpu: None, reads: None, end_of_scans, replays, closed: false }
    }

    #[test]
    fn state_comes_from_counters_when_timestamps_are_zero() {
        // A finished scan, a spool replayed for every outer row, a loop still going.
        assert!(finished(&counters(1_500_000.0, 1_500_000.0, 0.0, 0.0, 69.0), 1_500_000.0, true));
        assert!(!finished(&counters(95_000_000.0, 104_000_000.0, 478.0, 478.0, 0.0), 104_000_000.0, true));
        assert!(!finished(&counters(9_000.0, 200_000.0, 0.0, 1.0, 22_000.0), 200_000.0, false));
        assert!(producing(&counters(0.0, 1.0, 0.0, 0.0, 5.0)));
        assert!(!producing(&Counters { elapsed: None, ..counters(0.0, 1.0, 0.0, 0.0, 0.0) }));
    }
}
