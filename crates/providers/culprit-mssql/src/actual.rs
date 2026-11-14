//! What actually happened, read from DMVs only: the last actual plan SQL Server kept for a query
//! (sys.dm_exec_query_plan_stats, 2019+) and the live progress of a running request (sys.dm_exec_query_profiles and
//! sys.dm_exec_query_statistics_xml). Nothing here runs the user's statement.

use culprit_core::{
    Engine, Error, ErrorCode, NodeProgress, NodeState, Plan, PlanKind, QueryProgress, Result, session_number, t,
};

mod layout;
#[cfg(test)]
mod native_tests;
pub(crate) mod retained;
use crate::conn::{Pool, permission_hint};
use crate::plan_xml;
use crate::read::{NativeRead, rows as query};
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

/// The cached plan of a Query Store query, with the runtime counters of its last execution.
pub(crate) async fn last_actual_plan(pool: &Arc<Pool>, query_id: &str, plan_id: Option<&str>) -> Result<Plan> {
    let query_id = numeric(query_id)?.parse::<i64>().map_err(|_| ids_are_numbers())?;
    let plan_id = plan_id.map(|id| numeric(id)?.parse::<i64>().map_err(|_| ids_are_numbers())).transpose()?;
    let maximum = crate::read::ReadSession::application_budget().max_plan_bytes as usize;
    let read = pool
        .observe(|reader| Box::pin(async move { retained::read(reader, query_id, plan_id, maximum).await }))
        .await?;
    plan_from_retained(read)
}

fn plan_from_retained(read: retained::Read) -> Result<Plan> {
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
    let mut plan = plan_xml::parse_statement(&xml, row.context.query_hash.as_deref())?;
    if plan.kind != PlanKind::Actual {
        return Err(not_kept());
    }
    plan.raw = None;
    clear_unrecorded_times(&mut plan);
    Ok(plan)
}

// Lightweight profiling keeps row counts for past runs, not time. Withdraw its placeholder zeros in both models.
fn clear_unrecorded_times(plan: &mut Plan) {
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

const REQUEST_SQL: &str = r#"
SELECT r.status, r.command, r.wait_type, r.wait_time, r.total_elapsed_time AS elapsed_ms, r.cpu_time AS cpu_ms,
       r.logical_reads, r.percent_complete, CONVERT(varchar(20), r.query_hash, 1) AS query_hash,
       SUBSTRING(t.text, r.statement_start_offset / 2 + 1,
         (CASE r.statement_end_offset WHEN -1 THEN DATALENGTH(t.text) ELSE r.statement_end_offset END - r.statement_start_offset) / 2 + 1) AS statement_text,
       CONVERT(nvarchar(max), sx.query_plan) AS live_plan,
       qp.query_plan AS cached_plan
  FROM sys.dm_exec_requests r
 OUTER APPLY sys.dm_exec_sql_text(r.sql_handle) t
 OUTER APPLY sys.dm_exec_query_statistics_xml(r.session_id) sx
 OUTER APPLY sys.dm_exec_text_query_plan(r.plan_handle, r.statement_start_offset, r.statement_end_offset) qp
 WHERE r.session_id = {id}
"#;

/// Servers without sys.dm_exec_query_statistics_xml (2014 and older) or without permission to call it.
const REQUEST_BASIC_SQL: &str = r#"
SELECT r.status, r.command, r.wait_type, r.wait_time, r.total_elapsed_time AS elapsed_ms, r.cpu_time AS cpu_ms,
       r.logical_reads, r.percent_complete, CONVERT(varchar(20), r.query_hash, 1) AS query_hash,
       SUBSTRING(t.text, r.statement_start_offset / 2 + 1,
         (CASE r.statement_end_offset WHEN -1 THEN DATALENGTH(t.text) ELSE r.statement_end_offset END - r.statement_start_offset) / 2 + 1) AS statement_text,
       qp.query_plan AS cached_plan
  FROM sys.dm_exec_requests r
 OUTER APPLY sys.dm_exec_sql_text(r.sql_handle) t
 OUTER APPLY sys.dm_exec_text_query_plan(r.plan_handle, r.statement_start_offset, r.statement_end_offset) qp
 WHERE r.session_id = {id}
"#;

/// One row per operator, added up across threads. Lightweight profiling (the default) leaves the open/close timestamps
/// at zero, so state comes from rows, scans finished and re-executions instead.
const PROFILES_SQL: &str = r#"
SELECT node_id, SUM(row_count) AS row_count, MAX(estimate_row_count) AS estimate_row_count,
       MAX(elapsed_time_ms) AS elapsed_ms, SUM(cpu_time_ms) AS cpu_ms, SUM(logical_read_count) AS logical_reads,
       SUM(end_of_scan_count) AS end_of_scans, SUM(rebind_count) + SUM(rewind_count) AS replays,
       MIN(CASE WHEN close_time > 0 THEN 1 ELSE 0 END) AS closed
  FROM sys.dm_exec_query_profiles
 WHERE session_id = {id}
 GROUP BY node_id
"#;

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
async fn progress_rows<R: NativeRead>(reader: &mut R, id: u64) -> Result<(Vec<Rec>, Vec<Rec>)> {
    let layout = Layout::read(reader).await?;
    let sql = if layout.live_xml == Availability::Available { REQUEST_SQL } else { REQUEST_BASIC_SQL };
    let request = query(reader, &sql.replace("{id}", &id.to_string())).await?;
    let profiles = if layout.profiles == Availability::Available {
        query(reader, &PROFILES_SQL.replace("{id}", &id.to_string())).await?
    } else {
        vec![]
    };
    Ok((request, profiles))
}

pub(crate) async fn query_progress(pool: &Arc<Pool>, session: &str) -> Result<QueryProgress> {
    let id = session_number(session)?;
    let taken_at = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string();
    let (request, profiles) = pool.observe(|reader| Box::pin(async move { progress_rows(reader, id).await })).await?;
    let mut progress = QueryProgress::idle(session, Engine::Mssql, taken_at);
    let Some(r) = request.into_iter().next() else {
        return Ok(progress);
    };
    progress.running = true;
    progress.state = r.str("status");
    progress.wait = r.str("wait_type");
    progress.wait_ms = r.f64("wait_time").filter(|w| *w > 0.0);
    progress.elapsed_ms = r.f64("elapsed_ms");
    progress.cpu_ms = r.f64("cpu_ms");
    progress.logical_reads = r.f64("logical_reads");
    progress.statement = r.str("statement_text").map(|s| s.trim().to_string());
    let hash = r.str("query_hash");
    let live = r.str("live_plan").and_then(|xml| plan_xml::parse_statement(&xml, hash.as_deref()).ok());
    let plan =
        live.or_else(|| r.str("cached_plan").and_then(|xml| plan_xml::parse_statement(&xml, hash.as_deref()).ok()));
    progress.plan = plan.map(|mut p| {
        p.raw = None;
        p
    });
    progress.nodes = node_progress(&profiles, progress.plan.as_ref());
    // Backups, index rebuilds, DBCC and a few other commands report their own percentage; trust it over row counts.
    match r.f64("percent_complete").filter(|p| *p > 0.0) {
        Some(pct) => {
            progress.percent = Some((pct / 100.0).clamp(0.0, 1.0));
            progress.percent_basis = Some(own_estimate(r.string("command").trim()));
            if !progress.nodes.is_empty() {
                let percent = progress.percent;
                let basis = progress.percent_basis.clone();
                progress.summarize_nodes();
                progress.percent = percent;
                progress.percent_basis = basis;
            }
        }
        None => progress.summarize_nodes(),
    }
    if progress.nodes.is_empty() {
        progress.note = Some(no_profiling());
    }
    Ok(progress)
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

    fn retained_row() -> retained::RetainedPlan {
        let xml = r#"<ShowPlanXML><BatchSequence><Batch><Statements><StmtSimple StatementText="SELECT id FROM @Rows" QueryHash="0x0123456789ABCDEF" StatementSubTreeCost="1"><QueryPlan><RelOp NodeId="0" PhysicalOp="Table Scan" LogicalOp="Table Scan" EstimateRows="100" EstimatedTotalSubtreeCost="1"><RunTimeInformation><RunTimeCountersPerThread Thread="0" ActualRows="10000" ActualExecutions="1" /></RunTimeInformation><TableScan><Object Table="[@Rows]" /></TableScan></RelOp></QueryPlan></StmtSimple></Statements></Batch></BatchSequence></ShowPlanXML>"#;
        retained::RetainedPlan {
            context: crate::cached_statements::context(&crate::grants::tests::row()).unwrap(),
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
        let plan = plan_from_retained(retained::Read { setting_on: Some(false), plan: Some(row) }).unwrap();
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
        let error = plan_from_retained(retained::Read { setting_on: Some(true), plan: Some(row) }).unwrap_err();
        assert_eq!(error.code, ErrorCode::NotFound);
        assert!(!error.hint.unwrap_or_default().contains(TURN_ON));
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

#[cfg(test)]
mod reader_tests {
    use super::*;
    use crate::{
        read::{ReadError, invalid_data},
        resources::test_support::FakeRead,
        value::Val,
    };
    fn metadata(live: bool, profiles: bool) -> Rec {
        Rec::of(&[
            ("metadata_visibility", Val::Int(1)),
            ("last_actual", Val::Int(1)),
            ("live_xml", Val::Int(i64::from(live))),
            ("profiles", Val::Int(i64::from(profiles))),
        ])
    }
    #[tokio::test]
    async fn a_known_old_layout_selects_the_basic_query_before_any_request_and_native_failures_never_retry() {
        let mut old = FakeRead::new(vec![Ok(vec![metadata(false, false)]), Ok(vec![])]);
        let (request, profiles) = progress_rows(&mut old, 42).await.unwrap();
        assert!(request.is_empty() && profiles.is_empty());
        assert_eq!(old.calls.len(), 2);
        assert!(!old.calls[1].sql.contains("dm_exec_query_statistics_xml"));
        let mut failed = FakeRead::new(vec![Ok(vec![metadata(true, true)]), Err(ReadError::Source(invalid_data()))]);
        assert!(progress_rows(&mut failed, 42).await.is_err());
        assert_eq!(failed.calls.len(), 2);
        let mut profiles =
            FakeRead::new(vec![Ok(vec![metadata(true, true)]), Ok(vec![]), Err(ReadError::Source(invalid_data()))]);
        assert!(progress_rows(&mut profiles, 42).await.is_err());
        assert_eq!(profiles.calls.len(), 3);
    }
}
