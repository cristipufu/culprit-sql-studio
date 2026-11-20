//! One installed-view acquisition for scans and retained job diagnostics. No job or policy is executed.
use super::{QueryRead, Row, flag, invalid, iso, metadata, text};
use crate::read::NativeRead;
use culprit_core::{
    ErrorCode, Result,
    serialization::{JsonSizeError, json_size},
    telemetry::*,
};
use std::collections::BTreeMap;
mod continuous_aggregates;
mod decode;
mod discovery;
mod sql;
use discovery::Discovery;

pub(crate) const MAX_JOB_STATE_ROWS: u32 = 256;
pub(crate) const MAX_JOB_RUN_ROWS: u32 = 256;
const MAX_JOBS_QUERIES: u32 = 8;
const MAX_JOBS_PAGE_BYTES: u32 = 512 * 1024;
const MAX_JOBS_BYTES: u32 = 4 * 1024 * 1024;
const MAX_JOBS_PAGE_ROWS: u32 = 64;
const MAX_JOBS_PAGES: u32 = 16;
const JOBS_STATEMENT_TIMEOUT_MS: u32 = 1_000;
const JOBS_TOTAL_TIMEOUT_MS: u32 = 5_000;

pub(crate) fn bounded_budget(requested: &CollectionBudget) -> CollectionBudget {
    let mut budget = requested.clone();
    budget.max_snapshot_rows = budget.max_snapshot_rows.min(MAX_TIMESCALE_JOB_OBSERVATIONS as u32);
    budget.page_rows = budget.page_rows.min(MAX_JOBS_PAGE_ROWS);
    budget.page_bytes = budget.page_bytes.min(MAX_JOBS_PAGE_BYTES);
    budget.total_bytes = budget.total_bytes.min(MAX_JOBS_BYTES);
    budget.max_pages = budget.max_pages.min(MAX_JOBS_PAGES);
    budget.max_queries = budget.max_queries.min(MAX_JOBS_QUERIES);
    budget.statement_timeout_ms = budget.statement_timeout_ms.min(JOBS_STATEMENT_TIMEOUT_MS);
    budget.total_timeout_ms = budget.total_timeout_ms.min(JOBS_TOTAL_TIMEOUT_MS);
    budget
}

pub(crate) async fn capture<R: NativeRead>(
    reader: &mut R,
    host: &str,
    port: u16,
    budget: &CollectionBudget,
) -> Result<TimescaleJobsSnapshot> {
    budget.validate()?;
    let before = metadata::activity(reader, host, port).await?;
    let catalog = discovery::read(reader, &before.database_oid, &before.at).await?;
    let mut observations = Vec::new();
    let mut sources = Vec::new();
    let state_limit = budget.max_snapshot_rows.min(MAX_JOB_STATE_ROWS);
    let states = read_states(reader, &catalog, state_limit, &mut observations).await?;
    sources.push(states);
    let remaining = budget.max_snapshot_rows.saturating_sub(observations.len() as u32).min(MAX_JOB_RUN_ROWS);
    sources.push(read_runs(reader, &catalog, remaining, &mut observations).await?);
    let remaining =
        budget.max_snapshot_rows.saturating_sub(observations.len() as u32).min(MAX_TIMESCALE_CONTINUOUS_AGGREGATE_ROWS);
    sources.push(continuous_aggregates::read(reader, &catalog, remaining, &mut observations).await?);
    let after = metadata::activity(reader, host, port).await?;
    if !before.same_activity_source(&after) || before.full_visibility != after.full_visibility {
        return Err(CollectionStop::SourceChanged.error());
    }
    let final_catalog = discovery::read(reader, &before.database_oid, &before.at).await?;
    if catalog != final_catalog {
        return Err(CollectionStop::SourceChanged.error());
    }
    let database_writable = Some(before.writable_role());
    let snapshot = TimescaleJobsSnapshot {
        context: NativeReadContext {
            target: before.database.into(),
            partition: SourcePartition::Target,
            observed_from: before.at,
            observed_until: after.at,
            database_writable,
        },
        metadata: catalog.metadata(database_writable),
        sources,
        observations,
    };
    if !snapshot.is_valid() {
        return Err(invalid());
    }
    json_size(&snapshot, budget.total_bytes as usize).map_err(|error| match error {
        JsonSizeError::LimitExceeded => CollectionStop::ByteLimit.error(),
        JsonSizeError::Serialization => invalid(),
    })?;
    Ok(snapshot)
}

async fn read_states<R: QueryRead>(
    reader: &mut R,
    catalog: &Discovery,
    limit: u32,
    output: &mut Vec<OperationalObservation>,
) -> Result<StateSourceCoverage> {
    let Some(view) = catalog.available(TimescaleJobViewKind::Jobs) else {
        return Ok(catalog.unavailable(TIMESCALE_JOBS_SOURCE, TimescaleJobViewKind::Jobs));
    };
    if limit == 0 {
        return Ok(exhausted(TIMESCALE_JOBS_SOURCE));
    }
    let stats = catalog.available(TimescaleJobViewKind::Statistics);
    let mut rows = reader.query(&sql::states(view, stats, limit + 1), limit as usize + 1).await?;
    let truncated = rows.len() > limit as usize;
    rows.truncate(limit as usize);
    let count = rows.len() as u32;
    for (position, row) in rows.iter().enumerate() {
        let point = decode::state(row, position as u32 + 1)?;
        output.push(OperationalObservation {
            source: TIMESCALE_JOBS_SOURCE.into(),
            key: point.key()?,
            fact: OperationalFact::TimescaleJobState(Box::new(point)),
        });
    }
    let mut coverage =
        StateSourceCoverage::observed(TIMESCALE_JOBS_SOURCE.into(), count, !truncated && stats.is_some());
    if truncated {
        coverage.stopped = Some(CollectionStop::RowLimit);
    }
    if stats.is_none() {
        coverage.error_code = Some(
            catalog
                .unavailable(TIMESCALE_JOBS_SOURCE, TimescaleJobViewKind::Statistics)
                .error_code
                .unwrap_or(ErrorCode::Unsupported),
        );
    }
    Ok(coverage)
}
async fn read_runs<R: QueryRead>(
    reader: &mut R,
    catalog: &Discovery,
    limit: u32,
    output: &mut Vec<OperationalObservation>,
) -> Result<StateSourceCoverage> {
    let Some(view) = catalog.run_view() else {
        let kind = if catalog
            .view(TimescaleJobViewKind::History)
            .is_some_and(|view| view.state == TimescaleJobViewState::PermissionDenied)
        {
            TimescaleJobViewKind::History
        } else {
            TimescaleJobViewKind::Errors
        };
        return Ok(catalog.unavailable(TIMESCALE_JOB_RUNS_SOURCE, kind));
    };
    if limit == 0 {
        return Ok(exhausted(TIMESCALE_JOB_RUNS_SOURCE));
    }
    let mut rows = reader.query(&sql::runs(view, &catalog.window, limit + 1), limit as usize + 1).await?;
    let truncated = rows.len() > limit as usize;
    rows.truncate(limit as usize);
    let count = rows.len() as u32;
    for (position, row) in rows.iter().enumerate() {
        let point = decode::run(row, position as u32 + 1, catalog.procedure_origin())?;
        output.push(OperationalObservation {
            source: TIMESCALE_JOB_RUNS_SOURCE.into(),
            key: point.key()?,
            fact: OperationalFact::TimescaleJobRun(Box::new(point)),
        });
    }
    // Public history is retention-, logging- and owner-filtered even when this selected response drained.
    let mut coverage = StateSourceCoverage::observed(TIMESCALE_JOB_RUNS_SOURCE.into(), count, false);
    if truncated {
        coverage.stopped = Some(CollectionStop::RowLimit);
    }
    Ok(coverage)
}
fn exhausted(source: &str) -> StateSourceCoverage {
    StateSourceCoverage::unavailable(
        source.into(),
        CollectionStop::RowLimit.error().code,
        Some(CollectionStop::RowLimit),
    )
}
#[cfg(test)]
pub(crate) mod test_support;
#[cfg(test)]
mod tests;
