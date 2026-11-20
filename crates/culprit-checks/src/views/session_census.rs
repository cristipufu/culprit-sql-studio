//! One original native census remains informational: sessions, requests and connections are distinct populations.
use culprit_core::{
    Result, SessionCensusEntry, SessionCensusField, SessionCensusReport, SourceOutcome, SourceState, format, t,
    telemetry::*,
};
use std::collections::BTreeSet;

pub const MAX_CENSUS_PRESENTED_ROWS: usize = 256;

pub fn explain(read: &ActivityCensusRead) -> Result<SessionCensusReport> {
    explain_frame(&read.frame()?)
}

pub fn explain_frame(frame: &ActivityFrameEvidence<'_>) -> Result<SessionCensusReport> {
    let native = frame.native();
    let rows = frame.rows().collect::<Vec<_>>();
    let observed_sessions = rows.iter().map(|row| &row.observation.key.session).collect::<BTreeSet<_>>().len() as u32;
    let observed_requests = rows.iter().filter(|row| row.observation.key.request.is_some()).count() as u32;
    let observed_active_sessions = rows
        .iter()
        .filter(|row| !row.observation.idle)
        .map(|row| &row.observation.key.session)
        .collect::<BTreeSet<_>>()
        .len() as u32;
    let observed_blocked_sessions = rows
        .iter()
        .filter(|row| {
            row.observation.wait.as_ref().is_some_and(|wait| wait.owners.iter().any(ActivityWaitOwner::is_blocker))
        })
        .map(|row| &row.observation.key.session)
        .collect::<BTreeSet<_>>()
        .len() as u32;
    let entries = rows.iter().take(MAX_CENSUS_PRESENTED_ROWS).map(entry).collect();
    let limited = rows.len() > MAX_CENSUS_PRESENTED_ROWS;
    let gap = !frame.read_complete() || rows.len() as u32 != native.observations;
    let source_state =
        if gap && matches!(native.coverage.state, SourceState::Collected | SourceState::Empty | SourceState::Partial) {
            SourceState::Partial
        } else {
            native.coverage.state
        };
    let mut limitations = vec![
        t!("view.session_census.limit.point"),
        t!("view.session_census.limit.relationships"),
        t!("view.session_census.limit.client"),
        t!("view.session_census.limit.database_ids"),
    ];
    if native.population != ActivityPopulation::UserSessionsCurrentRequestsAndBlockers {
        limitations.push(t!("view.session_census.limit.population"));
    }
    if !native.coverage.complete_scope || gap {
        limitations.push(t!("view.session_census.limit.coverage"));
    }
    if limited {
        limitations.push(t!("view.session_census.limit.presentation"));
    }
    Ok(SessionCensusReport {
        context: frame.context(),
        observed_at: native.at.clone(),
        scope: native.scope,
        population: native.population,
        native_source: native.coverage.clone(),
        sources: vec![SourceOutcome {
            source: native.coverage.source.clone(),
            state: source_state,
            reason: if gap {
                Some(t!("view.session_census.limit.read"))
            } else {
                native.coverage.stopped.map(|stop| stop.error().message)
            },
            reason_message: None,
        }],
        native_rows: native.native_rows,
        observed_rows: rows.len() as u32,
        observed_sessions,
        observed_requests,
        observed_active_sessions,
        observed_blocked_sessions,
        presented_rows: rows.len().min(MAX_CENSUS_PRESENTED_ROWS) as u32,
        read_complete: frame.read_complete(),
        entries_limited: limited,
        title: t!("view.session_census.title"),
        summary: if rows.is_empty() {
            t!("view.session_census.summary.no_rows")
        } else {
            t!(
                "view.session_census.summary.recorded",
                sessions = format::digits(observed_sessions.to_string()),
                requests = format::digits(observed_requests.to_string())
            )
        },
        limitations,
        entries,
    })
}

fn field(key: &str, label: String, value: Option<String>) -> SessionCensusField {
    SessionCensusField { key: key.into(), label, value }
}
fn number(key: &str, label: String, value: Option<&str>) -> SessionCensusField {
    field(key, label, value.map(|value| format::digits(value.to_owned())))
}
fn lifecycle(value: SessionLifecycle) -> String {
    match value {
        SessionLifecycle::Running => t!("view.session_census.state.running"),
        SessionLifecycle::Idle => t!("view.session_census.state.idle"),
        SessionLifecycle::PoolResetPrelogin => t!("view.session_census.state.pool_reset"),
        SessionLifecycle::Connecting => t!("view.session_census.state.connecting"),
        SessionLifecycle::Unknown => t!("view.session_census.state.unknown"),
    }
}
fn entry(row: &ActivityFrameRow<'_>) -> SessionCensusEntry {
    let point = row.observation;
    let client = point.client.as_ref();
    let context = point.session_context.as_deref();
    let native = context.and_then(|context| context.native.as_deref()).map(|native| match native {
        SessionNativeContext::Mssql(native) => native,
    });
    let mut fields = vec![
        field("actor_state", t!("view.session_census.field.actor_state"), Some(point.state.clone())),
        field("database", t!("view.session_census.field.database"), point.database.clone()),
        field(
            "session_database_id",
            t!("view.session_census.field.session_database_id"),
            native.and_then(|native| native.session_database_id.clone()),
        ),
        field(
            "session_database_name",
            t!("view.session_census.field.session_database_name"),
            native.and_then(|native| native.session_database_name.clone()),
        ),
        field(
            "request_database_id",
            t!("view.session_census.field.request_database_id"),
            native.and_then(|native| native.request_database_id.clone()),
        ),
        field(
            "application",
            t!("view.session_census.field.application"),
            client.and_then(|value| value.application.clone()),
        ),
        field("login", t!("view.session_census.field.login"), client.and_then(|value| value.user.clone())),
        field("host", t!("view.session_census.field.host"), client.and_then(|value| value.host.clone())),
        field(
            "library",
            t!("view.session_census.field.library"),
            context.and_then(|value| value.client_library.clone()),
        ),
        number(
            "client_process",
            t!("view.session_census.field.client_process"),
            context.and_then(|value| value.client_process_id.as_deref()),
        ),
        number(
            "login_age_ms",
            t!("view.session_census.field.login_age"),
            context.and_then(|value| value.login_age_ms.as_deref()),
        ),
        number(
            "last_start_age_ms",
            t!("view.session_census.field.last_start_age"),
            point.last_request_age_ms.as_deref(),
        ),
        number(
            "last_end_age_ms",
            t!("view.session_census.field.last_end_age"),
            context.and_then(|value| value.last_request_end_age_ms.as_deref()),
        ),
        number(
            "current_elapsed_ms",
            t!("view.session_census.field.current_elapsed"),
            point.elapsed_ms.as_deref().filter(|_| point.key.request.is_some()),
        ),
        number(
            "open_transactions",
            t!("view.session_census.field.open_transactions"),
            point.open_transactions.as_ref().map(|value| value.to_string()).as_deref(),
        ),
    ];
    fields.extend(counter_fields(&point.session_counters, CounterLifetime::Session));
    if let Some(counters) = &point.request_counters {
        fields.extend(counter_fields(counters, CounterLifetime::CurrentRequest));
    }
    if let Some(native) = native {
        fields.extend([
            field(
                "native_session_state",
                t!("view.session_census.field.native_state"),
                Some(native.native_state.clone()),
            ),
            field(
                "login_established",
                t!("view.session_census.field.login_established"),
                Some(if native.login_established {
                    t!("view.session_census.yes")
                } else {
                    t!("view.session_census.no")
                }),
            ),
            field(
                "has_any_current_request",
                t!("view.session_census.field.current_presence"),
                Some(if native.has_current_request {
                    t!("view.session_census.yes")
                } else {
                    t!("view.session_census.no")
                }),
            ),
            number(
                "tds_protocol",
                t!("view.session_census.field.tds_protocol"),
                native.tds_protocol_version.as_deref(),
            ),
            field(
                "native_login_time",
                t!("view.session_census.field.native_login"),
                Some(native.times.login_without_offset.clone()),
            ),
            field(
                "native_last_start",
                t!("view.session_census.field.native_last_start"),
                native.times.last_request_start_without_offset.clone(),
            ),
            field(
                "native_last_end",
                t!("view.session_census.field.native_last_end"),
                native.times.last_request_end_without_offset.clone(),
            ),
            field(
                "request_connection_id",
                t!("view.session_census.field.request_connection"),
                native.request_connection_id.clone(),
            ),
        ]);
    }
    SessionCensusEntry {
        activity: point.key.clone(),
        reference: row.reference.cloned(),
        scope: point.scope,
        idle: point.idle,
        title: match &point.key.request {
            Some(request) => {
                t!("view.session_census.entry.request", session = &point.key.session.id, request = &request.id)
            }
            None => t!("view.session_census.entry.session", session = &point.key.session.id),
        },
        state: context.map_or_else(|| t!("view.session_census.state.unknown"), |context| lifecycle(context.lifecycle)),
        fields,
    }
}
#[derive(Clone, Copy)]
enum CounterLifetime {
    Session,
    CurrentRequest,
}
fn counter_fields(counters: &ActivityCounters, lifetime: CounterLifetime) -> Vec<SessionCensusField> {
    let (prefix, labels) = match lifetime {
        CounterLifetime::CurrentRequest => (
            "request",
            [
                t!("view.session_census.field.request_cpu"),
                t!("view.session_census.field.request_logical_reads"),
                t!("view.session_census.field.request_physical_reads"),
                t!("view.session_census.field.request_writes"),
            ],
        ),
        CounterLifetime::Session => (
            "session",
            [
                t!("view.session_census.field.session_cpu"),
                t!("view.session_census.field.session_logical_reads"),
                t!("view.session_census.field.session_physical_reads"),
                t!("view.session_census.field.session_writes"),
            ],
        ),
    };
    let mut fields: Vec<_> = [
        (&counters.cpu_ms, "cpu"),
        (&counters.logical_reads, "logical_reads"),
        (&counters.physical_reads, "physical_reads"),
        (&counters.writes, "writes"),
    ]
    .into_iter()
    .zip(labels)
    .map(|((value, key), label)| number(&format!("{prefix}_{key}"), label, value.as_deref()))
    .collect();
    if matches!(lifetime, CounterLifetime::CurrentRequest)
        && let Some(value) = counters.page_server_reads.as_deref()
    {
        fields.push(number(
            "request_page_server_reads",
            t!("view.session_census.field.request_page_server_reads"),
            Some(value),
        ));
    }
    fields
}

#[cfg(test)]
mod tests;
