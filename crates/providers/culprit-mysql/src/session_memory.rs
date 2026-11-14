//! Explicit scan and collector acquisition share one source-owned session allocation recipe.
use crate::{
    connect::{Connector, Scope},
    native::{self, session_memory as native_memory},
    probe::Probe,
    read::{OwnedRead, ReadSession, confirmed_result},
    sources,
};
use culprit_core::{Coverage, Error, ErrorCode, Result, telemetry::*};
use std::{collections::BTreeMap, sync::Arc};

// Seven owned setup statements, two four-query contexts, up to two data reads, and a final four-query collector
// verification fit within this allowance. Native cancellation retains its separate cleanup admission.
const MAX_QUERIES: u32 = 24;

pub(crate) fn budget() -> CollectionBudget {
    CollectionBudget {
        max_queries: MAX_QUERIES,
        max_snapshot_rows: native_memory::MAX_ROWS as u32,
        page_rows: native_memory::MAX_ROWS as u32,
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
    pub context: native_memory::Context,
    pub snapshot: SessionMemorySnapshot,
    pub truncated: bool,
    pub contexts_available: bool,
    pub context_failure: Option<Error>,
}

impl Capture {
    fn tracking_disabled(&self) -> bool {
        !self.context.consumers.memory_enabled() || self.snapshot.observations.iter().any(|row| !row.tracking_enabled)
    }

    pub(crate) fn source_coverage(&self) -> StateSourceCoverage {
        let complete = !self.truncated && self.contexts_available && !self.tracking_disabled();
        let mut coverage = StateSourceCoverage::observed(
            sources::SESSION_MEMORY.name.into(),
            self.snapshot.observations.len() as u32,
            complete,
        );
        coverage.error_code = self.context_failure.as_ref().map(|error| error.code);
        coverage
    }
}

pub(crate) async fn capture<R: OwnedRead>(
    reader: &mut R,
    limit: usize,
    expected: Option<&SharedResourceIdentity>,
) -> Result<Capture> {
    let (context, observed_from) = native_memory::context(reader).await?;
    if expected.is_some_and(|expected| expected != &context.instance.resource) {
        return Err(CollectionStop::SourceChanged.error());
    }
    let observed = native_memory::read(reader, &context, limit).await.map_err(source_error)?;
    let (after, observed_until) = native_memory::context(reader).await?;
    if after != context {
        return Err(CollectionStop::SourceChanged.error());
    }
    let snapshot = snapshot(&context, observed_from, observed_until, observed.sessions, observed.current)?;
    Ok(Capture {
        context,
        snapshot,
        truncated: observed.truncated,
        contexts_available: observed.contexts_available,
        context_failure: observed.context_failure,
    })
}

fn snapshot(
    context: &native_memory::Context,
    observed_from: String,
    observed_until: String,
    sessions: Vec<native_memory::Session>,
    mut current: BTreeMap<String, native_memory::CurrentEvent>,
) -> Result<SessionMemorySnapshot> {
    let rows = sessions
        .into_iter()
        .map(|session| {
            let identity = NativeIdentity::new(&session.session.generation)
                .qualified("server_uuid", &context.instance.resource.native.id)
                .qualified("server_started_second", context.instance.started_second.to_string());
            let current_context = current.remove(&session.session.generation).map(|event| SessionMemoryContext {
                request: event.request,
                observed_at: event.observed_at,
                database: event.database,
                query_hash: event.query_hash,
            });
            SessionMemoryObservation {
                source: sources::SESSION_MEMORY.name.into(),
                identity,
                observed_at: session.observed_at,
                session: session.session,
                tracking_enabled: session.instrumented && context.consumers.memory_enabled(),
                current_bytes: session.current_bytes,
                controlled_current_bytes: session.controlled_current_bytes,
                high_water_bytes: session.high_water_bytes,
                controlled_high_water_bytes: session.controlled_high_water_bytes,
                current_context,
                native_properties: [
                    ("global_instrumentation".into(), yes_no(context.consumers.global).into()),
                    ("thread_instrumentation".into(), yes_no(context.consumers.thread).into()),
                    ("INSTRUMENTED".into(), yes_no(session.instrumented).into()),
                ]
                .into(),
            }
        })
        .collect();
    let snapshot = SessionMemorySnapshot {
        source: sources::SESSION_MEMORY.name.into(),
        observed_from,
        observed_until,
        resource: context.instance.resource.clone(),
        observations: rows,
    };
    if !snapshot.is_valid() {
        return Err(native::invalid());
    }
    Ok(snapshot)
}

fn yes_no(value: bool) -> &'static str {
    if value { "YES" } else { "NO" }
}

fn source_error(error: Error) -> Error {
    if error.code == ErrorCode::PermissionDenied && error.detail.is_none() {
        error.with_hint(culprit_core::message!("provider.mysql.session_memory.permissions"))
    } else {
        error
    }
}

pub(crate) async fn collect(
    connector: Arc<Connector>,
    requested: bool,
    probe: &Probe,
) -> (Option<SessionMemorySnapshot>, Coverage) {
    let mut reader = ReadSession::new(connector, bounded_budget(&CollectionBudget::default()), Scope::Server);
    collect_from(
        &mut reader,
        requested,
        native_memory::availability(probe.flavor, &probe.version, probe.performance_schema),
    )
    .await
}

async fn collect_from<R: OwnedRead>(
    reader: &mut R,
    requested: bool,
    availability: native_memory::Availability,
) -> (Option<SessionMemorySnapshot>, Coverage) {
    let mut coverage = Coverage::default();
    if !requested {
        coverage
            .not_applicable(&sources::SESSION_MEMORY, culprit_core::message!("provider.session_memory.not_requested"));
        return (None, coverage);
    }
    if availability != native_memory::Availability::Available {
        if availability == native_memory::Availability::InstrumentationDisabled {
            coverage.source_failed(&sources::SESSION_MEMORY, &native_memory::unavailable(availability));
        } else {
            coverage
                .not_applicable(&sources::SESSION_MEMORY, native_memory::unavailable(availability).message_source());
        }
        return (None, coverage);
    }
    let provisional = capture(reader, native_memory::MAX_ROWS, None).await;
    match confirmed_result(reader, provisional).await {
        Ok(captured) => {
            if captured.truncated {
                coverage
                    .partial(&sources::SESSION_MEMORY, culprit_core::message!("provider.mysql.session_memory.partial"));
            } else if captured.tracking_disabled() {
                coverage.partial(
                    &sources::SESSION_MEMORY,
                    culprit_core::message!("provider.mysql.session_memory.tracking_disabled"),
                );
            } else if let Some(error) = &captured.context_failure {
                coverage.partial(&sources::SESSION_MEMORY, error.message_source());
            } else if !captured.contexts_available {
                coverage.partial(
                    &sources::SESSION_MEMORY,
                    culprit_core::message!("provider.mysql.session_memory.context_disabled"),
                );
            } else {
                coverage.completed(&sources::SESSION_MEMORY, captured.snapshot.observations.len());
            }
            if let Some(error) = &captured.context_failure {
                coverage.note(sources::SESSION_MEMORY.name, error.message_source());
            }
            (Some(captured.snapshot), coverage)
        }
        Err(error) => {
            coverage.source_failed(&sources::SESSION_MEMORY, &error);
            if let Some(hint) = error.hint_source() {
                coverage.note(sources::SESSION_MEMORY.name, hint);
            }
            (None, coverage)
        }
    }
}

#[cfg(test)]
pub(crate) mod tests;
