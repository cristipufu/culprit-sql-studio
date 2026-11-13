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

const MAX_SCAN_ROWS: u32 = 200;
const MAX_SCAN_QUERIES: u32 = 5; // Initial context, columns, grants, guarded current SQL, final context.

pub(crate) async fn collect(
    pool: Arc<Pool>,
    requested: bool,
    host: String,
    port: u16,
) -> (Option<Vec<ExecutionGrantObservation>>, Coverage) {
    if !requested {
        let mut coverage = Coverage::default();
        coverage.not_applicable(
            &sources::CURRENT_REQUEST_MEMORY,
            culprit_core::message!("provider.request_memory.not_requested"),
        );
        return (None, coverage);
    }
    let budget = CollectionBudget {
        max_snapshot_rows: MAX_SCAN_ROWS,
        page_rows: MAX_SCAN_ROWS,
        max_queries: MAX_SCAN_QUERIES,
        statement_timeout_ms: 3_000,
        total_timeout_ms: 10_000,
        ..Default::default()
    };
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
async fn collect_from<R: NativeRead>(
    reader: &mut ActivityReader<R>,
) -> (Option<Vec<ExecutionGrantObservation>>, Coverage) {
    let result = staged(reader).await;
    let result = if reader.close().await == SourceShutdown::Unconfirmed {
        Err(CollectionStop::CancellationUnconfirmed.error())
    } else {
        result
    };
    let mut coverage = Coverage::default();
    let (result, complete_scope) = match result {
        Ok(captured) => (Ok(captured.observations), captured.complete_scope),
        Err(error) => (Err(error), true),
    };
    let rows = coverage.read_rows(&sources::CURRENT_REQUEST_MEMORY, result);
    if !complete_scope {
        coverage
            .partial(&sources::CURRENT_REQUEST_MEMORY, culprit_core::message!("provider.mssql.request_memory.partial"));
    }
    (rows, coverage)
}

#[cfg(test)]
mod tests;
