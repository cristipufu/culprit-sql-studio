//! The explicit scan and fleet dataset use the same bounded native reader. No default live poll reads headers.
use crate::{
    activity::{ActivityReader, ActivityScope, IdentityDetail, ReadOptions},
    conn::Pool,
    read::NativeRead,
    sources,
};
use culprit_core::{Coverage, telemetry::*};
use std::sync::Arc;

const MAX_QUERIES: u32 = 18; // Local identity guards, observed pages/SQL and at most two targeted-index reads.

pub(crate) fn bounded_budget(requested: &CollectionBudget) -> CollectionBudget {
    CollectionBudget {
        max_snapshot_rows: requested.max_snapshot_rows.min(MAX_PAGE_WAIT_OBSERVATIONS as u32 + 1),
        page_rows: requested.page_rows.min(MAX_PAGE_WAIT_OBSERVATIONS as u32),
        max_sql_bytes: requested.max_sql_bytes.min(16 * 1024),
        max_queries: requested.max_queries.min(MAX_QUERIES),
        statement_timeout_ms: requested.statement_timeout_ms.min(1_000),
        total_timeout_ms: requested.total_timeout_ms.min(5_000),
        ..requested.clone()
    }
}

pub(crate) async fn collect(
    pool: Arc<Pool>,
    requested: bool,
    host: String,
    port: u16,
) -> (Option<PageWaitSnapshot>, Coverage) {
    if !requested {
        let mut coverage = Coverage::default();
        coverage.not_applicable(
            &sources::PAGE_WAIT_TARGETS,
            culprit_core::message!("provider.page_wait_targets.not_requested"),
        );
        return (None, coverage);
    }
    let options = ReadOptions {
        budget: bounded_budget(&CollectionBudget::default()),
        scope: ActivityScope::CurrentDatabase,
        identity: IdentityDetail::Endpoint,
    };
    match ActivityReader::open(pool, options, host, port) {
        Ok(mut reader) => collect_from(&mut reader).await,
        Err(error) => {
            let mut coverage = Coverage::default();
            coverage.source_failed(&sources::PAGE_WAIT_TARGETS, &error);
            (None, coverage)
        }
    }
}

pub(crate) async fn collect_from<R: NativeRead>(
    reader: &mut ActivityReader<R>,
) -> (Option<PageWaitSnapshot>, Coverage) {
    let result = reader.capture_page_wait_targets().await;
    let result = if reader.close().await == SourceShutdown::Unconfirmed {
        Err(CollectionStop::CancellationUnconfirmed.error())
    } else {
        result
    };
    let mut coverage = Coverage::default();
    let snapshot = match result {
        Ok(captured) => {
            coverage.completed(&sources::PAGE_WAIT_TARGETS, captured.snapshot.observations.len());
            // Request waits describe the coordinator only, including when the bounded read returned no rows.
            // They cannot establish that no parallel task waited or that these participants held the latch.
            coverage.partial(
                &sources::PAGE_WAIT_TARGETS,
                culprit_core::message!("provider.mssql.page_wait_targets.partial"),
            );
            if captured.limited {
                coverage.partial(
                    &sources::PAGE_WAIT_TARGETS,
                    culprit_core::message!("provider.mssql.page_wait_targets.limited"),
                );
            }
            Some(captured.snapshot)
        }
        Err(error) => {
            coverage.source_failed(&sources::PAGE_WAIT_TARGETS, &error);
            None
        }
    };
    (snapshot, coverage)
}
