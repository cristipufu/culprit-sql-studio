//! A running statement on another backend: pg_stat_activity for state, waits and elapsed time, the pg_stat_progress_*
//! views for the commands that report progress, and the statement's estimated plan (EXPLAIN, never ANALYZE).

use culprit_core::format::digits;
use culprit_core::{Engine, Error, QueryProgress, Result, session_number, t};

use crate::connect::Connector;
use crate::{
    native,
    read::{NativeRead, QueryRead, ReadSession},
};
use culprit_core::telemetry::*;
mod index;
mod reader;
mod source;
use crate::rows::Row;
pub(crate) use source::source;
#[cfg(test)]
mod retained_tests;
#[cfg(test)]
pub(crate) mod test_support;

const MAX_PROGRESS_QUERIES: u32 = 8; // Owned setup, two database/member bounds, two actor points and progress.
const MAX_PROGRESS_ROWS: u32 = 1;
const PROGRESS_STATEMENT_TIMEOUT_MS: u32 = 1_000;
const PROGRESS_TOTAL_TIMEOUT_MS: u32 = 5_000;

/// Postgres doesn't count rows per operator while a query runs; only these commands report how far along they are.
fn no_operator_progress() -> String {
    t!("provider.postgres.progress.no_operator_progress")
}

struct Phase {
    view: &'static str,
    minimum_version: u32,
    sql: &'static str,
    describe: fn(&Row) -> (String, Option<f64>),
}

fn ratio(done: Option<f64>, total: Option<f64>) -> Option<f64> {
    match (done, total) {
        (Some(d), Some(t)) if t > 0.0 => Some((d / t).clamp(0.0, 1.0)),
        _ => None,
    }
}

fn phase_text(r: &Row, command: &str) -> String {
    match r.text("phase").filter(|p| !p.is_empty()) {
        Some(phase) => format!("{command}: {phase}"),
        None => command.to_string(),
    }
}

const PHASES: &[Phase] = &[
    Phase {
        view: "pg_stat_progress_vacuum",
        minimum_version: 90_600,
        sql: "SELECT phase, heap_blks_total, heap_blks_scanned FROM pg_stat_progress_vacuum WHERE pid = {pid}",
        describe: |r| (phase_text(r, "VACUUM"), ratio(r.number("heap_blks_scanned"), r.number("heap_blks_total"))),
    },
    Phase {
        view: "pg_stat_progress_analyze",
        minimum_version: 130_000,
        sql: "SELECT phase, sample_blks_total, sample_blks_scanned FROM pg_stat_progress_analyze WHERE pid = {pid}",
        describe: |r| (phase_text(r, "ANALYZE"), ratio(r.number("sample_blks_scanned"), r.number("sample_blks_total"))),
    },
    Phase {
        view: "pg_stat_progress_cluster",
        minimum_version: 120_000,
        sql: "SELECT command, phase, heap_blks_total, heap_blks_scanned FROM pg_stat_progress_cluster WHERE pid = {pid}",
        describe: |r| {
            (
                phase_text(r, &r.text("command").unwrap_or_else(|| "CLUSTER".into())),
                ratio(r.number("heap_blks_scanned"), r.number("heap_blks_total")),
            )
        },
    },
    Phase {
        view: "pg_stat_progress_copy",
        minimum_version: 140_000,
        sql: "SELECT command, type, bytes_processed, bytes_total, tuples_processed FROM pg_stat_progress_copy WHERE pid = {pid}",
        describe: |r| {
            (
                copy_text(r.text("command"), r.text("type"), r.number("tuples_processed")),
                ratio(r.number("bytes_processed"), r.number("bytes_total")),
            )
        },
    },
];

/// A COPY's step: its command and kind ("COPY FROM FILE"), and the rows it has processed so far.
fn copy_text(command: Option<String>, kind: Option<String>, rows: Option<f64>) -> String {
    let what = format!("{} {}", command.unwrap_or_else(|| "COPY".into()), kind.unwrap_or_default());
    let step = what.trim();
    match rows.map(|rows| rows as u64) {
        Some(n) => t!("provider.postgres.progress.copy_rows", step = step, n = n, count = digits(n.to_string())),
        None => step.to_string(),
    }
}

/// State, waits, elapsed time and command progress. The caller adds the estimated plan when asked for it.
pub(crate) fn bounded_budget(requested: &CollectionBudget) -> CollectionBudget {
    let mut budget = requested.clone();
    budget.max_queries = budget.max_queries.min(MAX_PROGRESS_QUERIES);
    budget.max_snapshot_rows = budget.max_snapshot_rows.min(MAX_PROGRESS_ROWS);
    budget.page_rows = budget.page_rows.min(MAX_PROGRESS_ROWS);
    budget.statement_timeout_ms = budget.statement_timeout_ms.min(PROGRESS_STATEMENT_TIMEOUT_MS);
    budget.total_timeout_ms = budget.total_timeout_ms.min(PROGRESS_TOTAL_TIMEOUT_MS);
    budget
}

pub(crate) async fn capture_from<R: NativeRead>(
    reader: &mut R,
    host: &str,
    port: u16,
    session: &str,
    original: Option<&ActivityRequestFocus>,
    budget: &CollectionBudget,
) -> Result<MaintenanceProgressSnapshot> {
    let pid = pid(session)?;
    Ok(reader::capture(reader, host, port, pid, original, budget, reader::Mode::IndexOnly).await?.snapshot)
}

pub(crate) async fn query_progress(connector: &Connector, session: &str) -> Result<QueryProgress> {
    let pid = pid(session)?;
    let mut budget = bounded_budget(&CollectionBudget::default());
    budget.max_queries = 12; // One owned index-point read plus the existing four other native progress views.
    let mut io = ReadSession::new(connector.clone(), budget.clone());
    let captured =
        reader::capture(&mut io, &connector.host, connector.port, pid, None, &budget, reader::Mode::Live).await;
    if io.close().await == SourceShutdown::Unconfirmed {
        return Err(CollectionStop::CancellationUnconfirmed.error());
    }
    let captured = captured?;
    let mut progress = QueryProgress::idle(session, Engine::Postgres, captured.snapshot.context.observed_until);
    if let Some(actor) = captured.actor {
        progress.running = actor.key.request.is_some();
        progress.state = Some(actor.state);
        progress.statement = captured.sql;
        progress.elapsed_ms = actor.elapsed_ms.and_then(|value| value.parse().ok());
        progress.wait = actor.wait.and_then(|wait| wait.native_type);
    }
    if let Some(point) = captured.snapshot.observations.first() {
        let fraction = match &point.work {
            IndexBuildProgressWork::PostgreSql { progress } => {
                if point.phase.contains("scanning") {
                    ratio(number(&progress.blocks_done), number(&progress.blocks_total))
                } else {
                    ratio(number(&progress.tuples_done), number(&progress.tuples_total)).filter(|value| *value > 0.0)
                }
            }
            _ => None,
        };
        progress.phase = Some(format!("{}: {}", native_command(point.command), point.phase));
        progress.phase_percent = fraction;
        progress.percent = fraction;
        progress.percent_basis = fraction.map(|_| t!("provider.postgres.progress.basis", view = index::SOURCE));
    } else if let Some((phase, fraction, view)) = captured.other_phase {
        progress.phase = Some(phase);
        progress.phase_percent = fraction;
        progress.percent = fraction;
        progress.percent_basis = fraction.map(|_| t!("provider.postgres.progress.basis", view = view));
    }
    if progress.phase.is_none() {
        progress.note = Some(no_operator_progress());
    }
    Ok(progress)
}

fn pid(session: &str) -> Result<u32> {
    u32::try_from(session_number(session)?)
        .ok()
        .filter(|pid| *pid > 0 && *pid <= i32::MAX as u32)
        .ok_or_else(|| Error::invalid(t!("provider.postgres.error.not_a_pid", session = session)))
}

fn number(value: &Option<String>) -> Option<f64> {
    value.as_deref()?.parse().ok()
}

fn native_command(command: IndexBuildCommand) -> &'static str {
    match command {
        IndexBuildCommand::CreateIndex => "CREATE INDEX",
        IndexBuildCommand::CreateIndexConcurrently => "CREATE INDEX CONCURRENTLY",
        IndexBuildCommand::Reindex => "REINDEX",
        IndexBuildCommand::ReindexConcurrently => "REINDEX CONCURRENTLY",
    }
}

async fn read_other_phase<R: QueryRead>(
    reader: &mut R,
    pid: u32,
    database_oid: &str,
    actor: &ActivityKey,
    version: u32,
) -> Result<Option<(String, Option<f64>, &'static str)>> {
    let database = database_oid.parse::<u32>().ok().filter(|oid| *oid > 0).ok_or_else(native::invalid)?;
    let backend = native::activity::generation_timestamp(&actor.session.generation)?;
    let query =
        native::activity::generation_timestamp(&actor.request.as_ref().ok_or_else(native::invalid)?.generation)?;
    for phase in PHASES.iter().filter(|phase| version >= phase.minimum_version) {
        let sql = format!(
            "{} AND EXISTS(SELECT 1 FROM pg_catalog.pg_stat_activity a WHERE a.pid={pid} AND a.datid={database} AND a.backend_start='{backend}'::timestamptz AND a.query_start='{query}'::timestamptz AND a.state='active') LIMIT 2",
            phase.sql.replace("{pid}", &pid.to_string())
        );
        match reader.query(&sql, 2).await {
            Ok(rows) if rows.len() > 1 => return Err(native::invalid()),
            Ok(rows) => {
                if let Some(row) = rows.first() {
                    let (text, fraction) = (phase.describe)(row);
                    return Ok(Some((text, fraction, phase.view)));
                }
            }
            Err(error)
                if error.detail.is_none()
                    && matches!(
                        error.code,
                        culprit_core::ErrorCode::PermissionDenied | culprit_core::ErrorCode::Unsupported
                    ) => {}
            Err(error) => return Err(error),
        }
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use culprit_core::ConnectionSpec;

    // What a running statement's progress says, word for word.
    #[test]
    fn progress_in_english() {
        assert_eq!(
            copy_text(Some("COPY FROM".into()), Some("FILE".into()), Some(1200.0)),
            "COPY FROM FILE · 1200 rows so far"
        );
        assert_eq!(copy_text(None, None, None), "COPY");
        assert_eq!(copy_text(Some("COPY TO".into()), None, Some(0.0)), "COPY TO · 0 rows so far");
        assert_eq!(
            no_operator_progress(),
            "PostgreSQL doesn't report how far a running query has got, operator by operator. Culprit shows the plan \
             PostgreSQL estimated for it and how long it has been running. CREATE INDEX, VACUUM, ANALYZE, CLUSTER and \
             COPY report their own progress."
        );
    }

    // A session that can't be a backend's pid is refused before anything connects.
    #[tokio::test]
    async fn a_session_that_is_no_pid_in_english() {
        let connector = Connector::new(&ConnectionSpec { host: "localhost".into(), ..Default::default() }).unwrap();
        let error = query_progress(&connector, "99999999999").await.err().unwrap();
        assert_eq!(error.message, "'99999999999' is not a pid");
    }
}
