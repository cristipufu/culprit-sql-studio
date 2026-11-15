//! Explicit scan composition over the native reader also used by the collector; results remain provisional until
//! final identity verification and confirmed shutdown. The default policy performs no native read.
use crate::{
    activity::{ActivityReader, ActivityScope, IdentityDetail, ReadOptions},
    conn::Pool,
    read::NativeRead,
    sources,
};
use culprit_core::{Coverage, Result, telemetry::*};
use std::sync::Arc;

const MAX_ROWS: u32 = 200;
const MAX_QUERIES: u32 = 9; // Local owner3 +columns1 +grants1 +guarded SQL1 +local recheck3; Azure needs6.

pub(crate) fn bounded_budget(requested: &CollectionBudget) -> CollectionBudget {
    CollectionBudget {
        max_snapshot_rows: requested.max_snapshot_rows.min(MAX_ROWS),
        page_rows: requested.page_rows.min(MAX_ROWS),
        max_queries: requested.max_queries.min(MAX_QUERIES),
        page_bytes: requested.page_bytes.min(4 * 1024 * 1024),
        total_bytes: requested.total_bytes.min(64 * 1024 * 1024),
        max_sql_bytes: requested.max_sql_bytes.min(16 * 1024),
        statement_timeout_ms: requested.statement_timeout_ms.min(3_000),
        total_timeout_ms: requested.total_timeout_ms.min(10_000),
        ..requested.clone()
    }
}

pub(crate) async fn collect(
    pool: Arc<Pool>,
    requested: bool,
    host: String,
    port: u16,
) -> (Option<RequestMemorySnapshot>, Coverage) {
    if !requested {
        let mut coverage = Coverage::default();
        coverage.not_applicable(
            &sources::CURRENT_REQUEST_MEMORY,
            culprit_core::message!("provider.request_memory.not_requested"),
        );
        return (None, coverage);
    }
    let budget = bounded_budget(&CollectionBudget::default());
    let options = ReadOptions { budget, scope: ActivityScope::CurrentDatabase, identity: IdentityDetail::Endpoint };
    match ActivityReader::open(pool, options, host, port) {
        Ok(mut reader) => collect_from(&mut reader).await,
        Err(error) => {
            let mut coverage = Coverage::default();
            coverage.source_failed(&sources::CURRENT_REQUEST_MEMORY, &error);
            (None, coverage)
        }
    }
}
async fn staged<R: NativeRead>(reader: &mut ActivityReader<R>) -> Result<(RequestMemorySnapshot, bool)> {
    let captured = reader.capture_memory().await?;
    let context = reader.verify_memory_context(&captured.owner).await?;
    let snapshot = RequestMemorySnapshot {
        context,
        source: sources::CURRENT_REQUEST_MEMORY.name.into(),
        observed_at: captured.context.at,
        observations: captured.observations,
    };
    if !snapshot.is_valid() {
        return Err(crate::read::invalid_data());
    }
    Ok((snapshot, captured.complete_scope))
}
async fn collect_from<R: NativeRead>(reader: &mut ActivityReader<R>) -> (Option<RequestMemorySnapshot>, Coverage) {
    let result = staged(reader).await;
    let result = if reader.close().await == SourceShutdown::Unconfirmed {
        Err(CollectionStop::CancellationUnconfirmed.error())
    } else {
        result
    };
    let mut coverage = Coverage::default();
    match result {
        Ok((snapshot, complete_scope)) => {
            coverage.completed(&sources::CURRENT_REQUEST_MEMORY, snapshot.observations.len());
            if !complete_scope {
                coverage.partial(
                    &sources::CURRENT_REQUEST_MEMORY,
                    culprit_core::message!("provider.mssql.request_memory.partial"),
                );
            }
            (Some(snapshot), coverage)
        }
        Err(error) => {
            coverage.source_failed(&sources::CURRENT_REQUEST_MEMORY, &error);
            (None, coverage)
        }
    }
}

#[cfg(test)]
mod tests;
