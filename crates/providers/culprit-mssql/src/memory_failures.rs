//! Explicit scan policy over the same retained native reader used by background collection.
//! Native rows remain provisional until their physical route/resource and owned shutdown are confirmed.
use crate::{
    conn::Pool,
    instance, oom_events,
    probe::{ENGINE_EDITION_AZURE_SQL_DATABASE, ENGINE_EDITION_MANAGED_INSTANCE},
    read::{NativeRead, ReadSession, invalid_data},
    resources::pool,
    sources,
};
use culprit_core::{Coverage, ErrorCode, MeasurementScope, Result, telemetry::*};
use std::sync::Arc;

const MAX_QUERIES: u32 = 6; // Two MI context reads, the platform guard and the retained source projection.
const MAX_ROWS: usize = 128;

#[derive(PartialEq, Eq)]
enum Context {
    ManagedInstance(instance::Context),
    Azure(pool::Membership),
}
impl Context {
    async fn read<R: NativeRead>(reader: &mut R, edition: i64) -> Result<(Self, String)> {
        match edition {
            ENGINE_EDITION_MANAGED_INSTANCE => {
                instance::read(reader).await.map(|(context, at)| (Self::ManagedInstance(context), at))
            }
            ENGINE_EDITION_AZURE_SQL_DATABASE => {
                pool::identity(reader).await.map(|(context, at)| (Self::Azure(context), at))
            }
            _ => Err(invalid_data()),
        }
    }
    fn resource(&self) -> Option<SharedResourceIdentity> {
        match self {
            Self::ManagedInstance(context) => Some(context.identity.clone()),
            Self::Azure(context) => context.pool.as_ref().map(|pool| pool.identity.clone()),
        }
    }
}

fn budget() -> CollectionBudget {
    CollectionBudget {
        max_queries: MAX_QUERIES,
        max_snapshot_rows: MAX_ROWS as u32,
        page_rows: MAX_ROWS as u32,
        page_bytes: 8 * 1024 * 1024,
        statement_timeout_ms: 3_000,
        total_timeout_ms: 10_000,
        ..Default::default()
    }
}

pub(crate) async fn collect(
    pool: Arc<Pool>,
    requested: bool,
    edition: i64,
) -> (Option<MemoryFailureSnapshot>, Coverage) {
    let mut reader = ReadSession::new(pool, budget());
    collect_from(&mut reader, requested, edition).await
}

async fn staged<R: NativeRead>(reader: &mut R, edition: i64) -> Result<Option<MemoryFailureSnapshot>> {
    let (context, observed_from) = Context::read(reader, edition).await?;
    let Some(observations) = oom_events::read(reader, MAX_ROWS).await? else { return Ok(None) };
    let (after, observed_until) = Context::read(reader, edition).await?;
    if after != context {
        return Err(CollectionStop::SourceChanged.error());
    }
    let resource = context.resource();
    let snapshot = MemoryFailureSnapshot {
        source: sources::MEMORY_FAILURES.name.into(),
        observed_from,
        observed_until,
        scope: if resource.as_ref().is_some_and(|resource| resource.kind == SharedResourceKind::Pool) {
            MeasurementScope::Pool
        } else {
            MeasurementScope::Server
        },
        resource,
        observations,
    };
    if !snapshot.is_valid() {
        return Err(invalid_data());
    }
    Ok(Some(snapshot))
}

async fn collect_from<R: NativeRead>(
    reader: &mut R,
    requested: bool,
    edition: i64,
) -> (Option<MemoryFailureSnapshot>, Coverage) {
    let mut coverage = Coverage::default();
    if !requested {
        coverage.not_applicable(
            &sources::MEMORY_FAILURES,
            culprit_core::message!("provider.memory_failures.not_requested"),
        );
        return (None, coverage);
    }
    if matches!(edition, 2..=4) {
        coverage.not_applicable(
            &sources::MEMORY_FAILURES,
            culprit_core::message!("provider.mssql.memory_failures.boxed_unavailable"),
        );
        return (None, coverage);
    }
    if !matches!(edition, ENGINE_EDITION_AZURE_SQL_DATABASE | ENGINE_EDITION_MANAGED_INSTANCE) {
        coverage.source_failed(
            &sources::MEMORY_FAILURES,
            &culprit_core::Error::new(ErrorCode::Unsupported, culprit_core::t!("collection.unsupported")),
        );
        return (None, coverage);
    }
    let result = staged(reader, edition).await;
    let result = if reader.close().await == SourceShutdown::Unconfirmed {
        Err(CollectionStop::CancellationUnconfirmed.error())
    } else {
        result
    };
    match result {
        Ok(Some(snapshot)) => {
            coverage.completed(&sources::MEMORY_FAILURES, snapshot.observations.len());
            (Some(snapshot), coverage)
        }
        Ok(None) => {
            coverage.not_applicable(
                &sources::MEMORY_FAILURES,
                culprit_core::message!("provider.mssql.memory_failures.boxed_unavailable"),
            );
            (None, coverage)
        }
        Err(error) => {
            coverage.source_failed(&sources::MEMORY_FAILURES, &error);
            if let Some(hint) = error.hint_source() {
                coverage.note(sources::MEMORY_FAILURES.name, hint);
            }
            (None, coverage)
        }
    }
}

#[cfg(test)]
mod tests;
