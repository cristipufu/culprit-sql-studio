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
const MAX_QUERIES: u32 = 5; // Initial context, columns, grants, guarded current SQL, final context.

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
async fn staged<R: NativeRead>(reader: &mut ActivityReader<R>) -> Result<crate::activity::memory::MemoryCapture> {
    let captured = reader.capture_memory().await?;
    if !reader.verify_memory_context(&captured.context).await? {
        return Err(CollectionStop::SourceChanged.error());
    }
    Ok(captured)
}
async fn collect_from<R: NativeRead>(reader: &mut ActivityReader<R>) -> (Option<RequestMemorySnapshot>, Coverage) {
    let result = staged(reader).await;
    let result = if reader.close().await == SourceShutdown::Unconfirmed {
        Err(CollectionStop::CancellationUnconfirmed.error())
    } else {
        result
    };
    let mut coverage = Coverage::default();
    let (result, complete_scope, observed_at) = match result {
        Ok(captured) => (Ok(captured.observations), captured.complete_scope, Some(captured.context.at)),
        Err(error) => (Err(error), true, None),
    };
    let rows = coverage.read_rows(&sources::CURRENT_REQUEST_MEMORY, result);
    if !complete_scope {
        coverage
            .partial(&sources::CURRENT_REQUEST_MEMORY, culprit_core::message!("provider.mssql.request_memory.partial"));
    }
    let snapshot = rows.zip(observed_at).map(|(observations, observed_at)| RequestMemorySnapshot {
        source: sources::CURRENT_REQUEST_MEMORY.name.into(),
        observed_at,
        observations,
    });
    (snapshot, coverage)
}

#[cfg(test)]
mod tests;
