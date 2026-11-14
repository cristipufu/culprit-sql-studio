//! Explicit scan acquisition over the same native diagnostic reader used by background collection.
use crate::{
    connect::{Connector, Scope},
    native::{self, error_log, error_log::context},
    probe::Probe,
    read::{OwnedRead, QueryRead, ReadSession, confirmed_result},
    sources,
};
use culprit_core::{Coverage, Error, ErrorCode, Result, telemetry::*};
use std::sync::Arc;

// Owned setup takes seven statements. Scan acquisition takes another seven; frozen collection verification
// takes three more. Cancellation control retains its own cleanup admission after this ordinary-query budget.
const MAX_QUERIES: u32 = 24;

pub(crate) fn budget() -> CollectionBudget {
    CollectionBudget {
        max_queries: MAX_QUERIES,
        max_snapshot_rows: error_log::MAX_ROWS as u32,
        page_rows: error_log::MAX_ROWS as u32,
        page_bytes: 1024 * 1024,
        total_bytes: 8 * 1024 * 1024,
        statement_timeout_ms: 2_000,
        total_timeout_ms: 10_000,
        ..Default::default()
    }
}

pub(crate) fn bounded_budget(requested: &CollectionBudget) -> CollectionBudget {
    let native = budget();
    CollectionBudget {
        max_queries: requested.max_queries.min(native.max_queries),
        max_snapshot_rows: requested.max_snapshot_rows.min(native.max_snapshot_rows),
        page_rows: requested.page_rows.min(native.page_rows),
        page_bytes: requested.page_bytes.min(native.page_bytes),
        total_bytes: requested.total_bytes.min(native.total_bytes),
        statement_timeout_ms: requested.statement_timeout_ms.min(native.statement_timeout_ms),
        total_timeout_ms: requested.total_timeout_ms.min(native.total_timeout_ms),
        ..requested.clone()
    }
}

pub(crate) struct Capture {
    pub context: context::Context,
    pub observed_from: String,
    pub observed_until: String,
    pub observations: error_log::Observations,
}

impl Capture {
    fn into_snapshot(self) -> NativeFailureSnapshot {
        NativeFailureSnapshot {
            source: sources::NATIVE_FAILURES.name.into(),
            observed_from: self.observed_from,
            observed_until: self.observed_until,
            resource: Some(self.context.instance.resource),
            observations: self.observations.rows.into_iter().map(NativeFailureRecord::MysqlErrorLog).collect(),
        }
    }
}

pub(crate) async fn capture<R: QueryRead>(
    reader: &mut R,
    limit: usize,
    expected: Option<&SharedResourceIdentity>,
) -> Result<Capture> {
    let (context, observed_from) = context::read(reader).await?;
    if expected.is_some_and(|expected| expected != &context.instance.resource) {
        return Err(CollectionStop::SourceChanged.error());
    }
    let observations = error_log::read(reader, limit).await.map_err(source_error)?;
    let (after, observed_until) = context::read(reader).await?;
    if after != context {
        return Err(CollectionStop::SourceChanged.error());
    }
    Ok(Capture { context, observed_from, observed_until, observations })
}

fn source_error(error: Error) -> Error {
    if error.code == ErrorCode::PermissionDenied && error.detail.is_none() {
        error.with_hint(culprit_core::message!("provider.mysql.native_failures.permissions"))
    } else {
        error
    }
}

pub(crate) async fn collect(
    connector: Arc<Connector>,
    requested: bool,
    probe: &Probe,
) -> (Option<NativeFailureSnapshot>, Coverage) {
    let mut reader = ReadSession::new(connector, bounded_budget(&CollectionBudget::default()), Scope::Server);
    collect_from(
        &mut reader,
        requested,
        error_log::availability(probe.flavor, &probe.version, probe.performance_schema),
    )
    .await
}

async fn collect_from<R: OwnedRead>(
    reader: &mut R,
    requested: bool,
    availability: error_log::Availability,
) -> (Option<NativeFailureSnapshot>, Coverage) {
    let mut coverage = Coverage::default();
    if !requested {
        coverage.not_applicable(
            &sources::NATIVE_FAILURES,
            culprit_core::message!("provider.native_failures.not_requested"),
        );
        return (None, coverage);
    }
    if availability != error_log::Availability::Available {
        if availability == error_log::Availability::InstrumentationDisabled {
            coverage.source_failed(&sources::NATIVE_FAILURES, &context::unavailable(availability));
        } else {
            let reason = match availability {
                error_log::Availability::UnsupportedProduct => {
                    culprit_core::message!("provider.mysql.native_failures.mariadb")
                }
                _ => culprit_core::message!("provider.mysql.native_failures.version"),
            };
            coverage.not_applicable(&sources::NATIVE_FAILURES, reason);
        }
        return (None, coverage);
    }
    let result = capture(reader, error_log::MAX_ROWS, None).await;
    let result = confirmed_result(reader, result).await;
    match result {
        Ok(captured) => {
            if captured.observations.truncated {
                coverage.partial(
                    &sources::NATIVE_FAILURES,
                    culprit_core::message!("provider.mysql.native_failures.partial"),
                );
            } else {
                coverage.completed(&sources::NATIVE_FAILURES, captured.observations.rows.len());
            }
            let snapshot = captured.into_snapshot();
            if !snapshot.is_valid() {
                coverage.source_failed(&sources::NATIVE_FAILURES, &native::invalid());
                return (None, coverage);
            }
            (Some(snapshot), coverage)
        }
        Err(error) => {
            coverage.source_failed(&sources::NATIVE_FAILURES, &error);
            if let Some(hint) = error.hint_source() {
                coverage.note(sources::NATIVE_FAILURES.name, hint);
            }
            (None, coverage)
        }
    }
}

#[cfg(test)]
pub(crate) mod tests;
