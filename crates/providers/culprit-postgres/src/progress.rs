//! A running statement on another backend: pg_stat_activity for state, waits and elapsed time, the pg_stat_progress_*
//! views for the commands that report progress, and the statement's estimated plan (EXPLAIN, never ANALYZE).

use culprit_core::format::digits;
use culprit_core::{Engine, Error, QueryProgress, Result, session_number, t};

use crate::connect::Connector;
use crate::{
    native,
    read::{NativeRead, ReadSession},
};
use culprit_core::telemetry::*;
mod analyze;
mod cluster;
mod copy;
mod index;
mod maintenance;
mod reader;
mod selected;
mod source;
mod vacuum;
#[cfg(test)]
pub(crate) use source::fixture_source;
pub(crate) use source::source;
#[cfg(test)]
mod non_index_tests;
#[cfg(test)]
mod retained_tests;
#[cfg(test)]
pub(crate) mod test_support;

// One owned setup, two database/member bounds, two actor points and at most four selected progress views.
const MAX_PROGRESS_QUERIES: u32 = 11;
const MAX_LIVE_PROGRESS_QUERIES: u32 = MAX_PROGRESS_QUERIES + 1;
// One setup, two native metadata/member bounds, two actor points and one selected COPY read.
const MAX_CURRENT_WORK_QUERIES: u32 = 8;
const MAX_PROGRESS_ROWS: u32 = 1;
const PROGRESS_STATEMENT_TIMEOUT_MS: u32 = 1_000;
const PROGRESS_TOTAL_TIMEOUT_MS: u32 = 5_000;

/// Postgres doesn't count rows per operator while a query runs; only these commands report how far along they are.
fn no_operator_progress() -> String {
    t!("provider.postgres.progress.no_operator_progress")
}

fn ratio(done: Option<f64>, total: Option<f64>) -> Option<f64> {
    match (done, total) {
        (Some(d), Some(t)) if t > 0.0 => Some((d / t).clamp(0.0, 1.0)),
        _ => None,
    }
}

/// A native COPY caption has exact row counts and is not a progress phase.
fn copy_text(progress: &PostgresCopyProgress) -> String {
    let step = format!("{} {}", progress.command.as_native(), progress.io_type.as_native());
    match progress.tuples_processed.as_deref().and_then(|value| value.parse::<u64>().ok()) {
        Some(n) => t!("provider.postgres.progress.copy_rows", step = step, n = n, count = digits(n.to_string())),
        None => step,
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

pub(crate) fn current_work_budget(requested: &CollectionBudget) -> CollectionBudget {
    let mut budget = bounded_budget(requested);
    budget.max_queries = budget.max_queries.min(MAX_CURRENT_WORK_QUERIES);
    budget.max_sql_bytes = budget.max_sql_bytes.min(MAX_CURRENT_REQUEST_WORK_SQL_BYTES as u32);
    budget
}

#[cfg(test)]
pub(crate) async fn capture_from<R: NativeRead>(
    reader: &mut R,
    host: &str,
    port: u16,
    session: &str,
    original: Option<&ActivityRequestFocus>,
    budget: &CollectionBudget,
) -> Result<MaintenanceProgressSnapshot> {
    let pid = pid(session)?;
    match reader::capture(reader, host, port, pid, original, budget, reader::Mode::Retained).await?.progress {
        reader::CapturedProgress::Maintenance(snapshot) => Ok(snapshot),
        reader::CapturedProgress::CurrentWork(_) => Err(native::invalid()),
    }
}

pub(crate) async fn query_progress(connector: &Connector, session: &str) -> Result<QueryProgress> {
    let pid = pid(session)?;
    let mut budget = bounded_budget(&CollectionBudget::default());
    budget.max_queries = MAX_LIVE_PROGRESS_QUERIES;
    let mut io = ReadSession::new(connector.clone(), budget.clone());
    let captured =
        reader::capture(&mut io, &connector.host, connector.port, pid, None, &budget, reader::Mode::Live).await;
    if io.close().await == SourceShutdown::Unconfirmed {
        return Err(CollectionStop::CancellationUnconfirmed.error());
    }
    Ok(project(session, captured?))
}

fn project(session: &str, captured: reader::Acquired) -> QueryProgress {
    let context = captured.progress.context();
    let request_key = captured.actor.as_ref().and_then(|actor| {
        actor.statement_identity.as_ref().and_then(|identity| context.request_key(&actor.key, identity))
    });
    let mut progress = QueryProgress::unobserved(session, Engine::Postgres, context.observed_until.clone());
    progress.request_key = request_key;
    if let Some(actor) = captured.actor {
        progress.running = actor.key.request.as_ref().map(|_| true).or_else(|| actor.idle.then_some(false));
        progress.state = Some(actor.state);
        progress.statement = captured.sql;
        progress.elapsed_ms = actor.elapsed_ms.and_then(|value| value.parse().ok());
        progress.wait = actor.wait.and_then(|wait| wait.native_type);
    }
    match captured.progress {
        reader::CapturedProgress::Maintenance(snapshot) => {
            if let Some(point) = snapshot.observations.first() {
                let fraction = phase_fraction(point);
                progress.phase = Some(format!("{}: {}", point.command.as_native(), point.phase));
                progress.phase_percent = fraction;
                progress.phase_percent_basis =
                    fraction.map(|_| t!("provider.postgres.progress.basis", view = &snapshot.source));
            }
        }
        reader::CapturedProgress::CurrentWork(snapshot) => {
            if let Some(point) = snapshot.observations.first()
                && let CurrentRequestWorkNative::PostgresCopy { progress: copy } = &point.native
            {
                progress.taken_at = point.observed_at.clone();
                progress.note = Some(copy_text(copy));
            }
            progress.current_work = Some(Box::new(snapshot));
        }
    }
    if progress.phase.is_none() && progress.note.is_none() {
        progress.note = Some(no_operator_progress());
    }
    progress
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

fn phase_fraction(point: &MaintenanceProgressObservation) -> Option<f64> {
    match &point.work {
        MaintenanceProgressWork::PostgreSqlIndexBuild { progress } => {
            if point.phase.contains("scanning") {
                ratio(number(&progress.blocks_done), number(&progress.blocks_total))
            } else {
                ratio(number(&progress.tuples_done), number(&progress.tuples_total)).filter(|value| *value > 0.0)
            }
        }
        MaintenanceProgressWork::PostgreSqlVacuum { progress } if point.phase == "scanning heap" => {
            ratio(number(&progress.heap_blks_scanned), number(&progress.heap_blks_total))
        }
        MaintenanceProgressWork::PostgreSqlAnalyze { progress } if point.phase == "acquiring sample rows" => {
            ratio(number(&progress.sample_blks_scanned), number(&progress.sample_blks_total))
        }
        MaintenanceProgressWork::PostgreSqlCluster { progress }
            if matches!(point.phase.as_str(), "seq scanning heap" | "index scanning heap") =>
        {
            ratio(number(&progress.heap_blks_scanned), number(&progress.heap_blks_total))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use culprit_core::ConnectionSpec;

    // What a running statement's progress says, word for word.
    #[test]
    fn progress_in_english() {
        let copy = PostgresCopyProgress {
            database_oid: "1".into(),
            relation_oid: "0".into(),
            command: PostgresCopyCommand::From,
            io_type: PostgresCopyIo::File,
            bytes_processed: None,
            bytes_total: Some("0".into()),
            tuples_processed: Some("1200".into()),
            tuples_excluded: None,
            tuples_skipped: None,
        };
        assert_eq!(copy_text(&copy), "COPY FROM FILE · 1200 rows so far");
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
