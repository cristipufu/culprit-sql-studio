//! The explicit scan and collector reuse one native task reader. No default scan or Live poll adds this SQL.
use crate::{
    activity::{ActivityReader, ActivityScope, IdentityDetail, ReadOptions},
    conn::Pool,
    read::NativeRead,
    sources,
};
use culprit_core::{Coverage, telemetry::*};
use std::sync::Arc;

pub(crate) const MAX_QUERIES: u32 = 7; // Six acquisition reads plus the collector's final frozen-source verification.

pub(crate) fn bounded_budget(requested: &CollectionBudget) -> CollectionBudget {
    CollectionBudget {
        max_snapshot_rows: requested.max_snapshot_rows.min(crate::activity::tempdb::MAX_NATIVE_ROWS as u32),
        page_rows: requested.page_rows.min(crate::activity::tempdb::MAX_NATIVE_ROWS as u32),
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
) -> (Option<TaskSpaceSnapshot>, Coverage) {
    if !requested {
        let mut coverage = Coverage::default();
        coverage.not_applicable(&sources::TASK_SPACE, culprit_core::message!("provider.task_space.not_requested"));
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
            coverage.source_failed(&sources::TASK_SPACE, &error);
            (None, coverage)
        }
    }
}

pub(crate) async fn collect_from<R: NativeRead>(
    reader: &mut ActivityReader<R>,
) -> (Option<TaskSpaceSnapshot>, Coverage) {
    let result = reader.capture_task_space().await;
    let result = if reader.close().await == SourceShutdown::Unconfirmed {
        Err(CollectionStop::CancellationUnconfirmed.error())
    } else {
        result
    };
    let mut coverage = Coverage::default();
    match result {
        Ok(captured) => {
            coverage.completed(&sources::TASK_SPACE, captured.snapshot.observations.len());
            if captured.limited {
                coverage.partial(&sources::TASK_SPACE, culprit_core::message!("provider.mssql.task_space.limited"));
            }
            if !captured.context_complete {
                coverage
                    .partial(&sources::TASK_SPACE, culprit_core::message!("provider.mssql.task_space.context_partial"));
            }
            (Some(captured.snapshot), coverage)
        }
        Err(error) => {
            coverage.source_failed(&sources::TASK_SPACE, &error);
            (None, coverage)
        }
    }
}

#[cfg(test)]
mod tests;
