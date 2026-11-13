//! The explicit scan and fleet dataset use the same bounded native reader. No default live poll reads headers.
use crate::{
    activity::{ActivityReader, ActivityScope, IdentityDetail, ReadOptions},
    conn::Pool,
    read::NativeRead,
    sources,
};
use culprit_core::{Coverage, telemetry::*};
use std::sync::Arc;

const SCAN_QUERIES: u32 = 16; // Up to three local-identity reads per bound, plus observed targets and guarded SQL.

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
        budget: CollectionBudget {
            max_snapshot_rows: MAX_PAGE_WAIT_OBSERVATIONS as u32 + 1,
            page_rows: MAX_PAGE_WAIT_OBSERVATIONS as u32,
            max_sql_bytes: 16 * 1024,
            max_queries: SCAN_QUERIES,
            statement_timeout_ms: 1_000,
            total_timeout_ms: 5_000,
            ..Default::default()
        },
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
