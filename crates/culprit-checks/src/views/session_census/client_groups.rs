//! One original census owns both session deduplication and observed client-label totals.
use super::{field, number};
use culprit_core::{
    SessionCensusClientGroup, SessionCensusClientGroups, SessionClientDatabase, SessionClientGroupKey, format, t,
    telemetry::*,
};
use std::collections::{BTreeMap, BTreeSet};

const MAX_PRESENTED_CLIENT_GROUPS: usize = 64;

#[derive(PartialEq, Eq)]
struct SessionFacts<'a> {
    key: SessionClientGroupKey,
    times: SessionTimes<'a>,
    elapsed_ms: Option<&'a str>,
    logical_reads: Option<&'a str>,
    writes: Option<&'a str>,
}
#[derive(PartialEq, Eq, Clone, Copy)]
enum SessionTimes<'a> {
    OffsetFree { login: &'a str, last_end: Option<&'a str> },
    BackendUtc { started: &'a str },
    Unavailable,
}
impl<'a> SessionFacts<'a> {
    fn from_row(row: &'a ActivityObservation) -> Option<Self> {
        let context = row.session_context.as_deref()?;
        let native = context.native.as_deref()?;
        let database = match native {
            SessionNativeContext::Mssql(native) => SessionClientDatabase::CurrentSession {
                native_id: native.session_database_id.as_deref().filter(|id| *id != "0")?.into(),
                name: native.session_database_name.clone(),
            },
            SessionNativeContext::Postgres(native) if native.is_client_backend() => SessionClientDatabase::Backend {
                native_id: native.database_oid.as_deref().filter(|id| *id != "0")?.into(),
                name: native.database_name.clone(),
            },
            SessionNativeContext::Mysql(native) if native.is_client_thread(row.command.as_deref()) => {
                SessionClientDatabase::CurrentDefault { name: native.default_database.clone()? }
            }
            _ => return None,
        };
        let client = row.client.as_ref();
        let mut facts = Self {
            key: SessionClientGroupKey {
                database,
                host: client.and_then(|client| client.host.clone()),
                application: client.and_then(|client| client.application.clone()),
                library: context.client_library.clone(),
            },
            times: SessionTimes::Unavailable,
            elapsed_ms: None,
            logical_reads: row.session_counters.logical_reads.as_deref(),
            writes: row.session_counters.writes.as_deref(),
        };
        match native {
            SessionNativeContext::Mssql(native) => {
                facts.times = SessionTimes::OffsetFree {
                    login: &native.times.login_without_offset,
                    last_end: native.times.last_request_end_without_offset.as_deref(),
                };
                facts.elapsed_ms = native.elapsed_ms.as_deref();
            }
            SessionNativeContext::Postgres(_) => {
                facts.times = SessionTimes::BackendUtc { started: &row.key.session.generation }
            }
            SessionNativeContext::Mysql(_) => (),
        }
        Some(facts)
    }
}

#[derive(Default)]
struct Group<'a> {
    sessions: Vec<SessionFacts<'a>>,
    requests: u32,
}

pub(super) fn explain(frame: &ActivityFrameEvidence<'_>) -> SessionCensusClientGroups {
    let identities = frame.rows().map(|row| &row.observation.key.session).collect::<BTreeSet<_>>();
    let verified = frame.is_qualified() && frame.read_complete();
    let mut groups = BTreeMap::<SessionClientGroupKey, Group<'_>>::new();
    if verified {
        for identity in &identities {
            if let Some((facts, requests)) = consistent_session(frame, identity) {
                let group = groups.entry(facts.key.clone()).or_default();
                group.sessions.push(facts);
                group.requests += requests;
            }
        }
    }
    let grouped_sessions = groups.values().map(|group| group.sessions.len() as u32).sum::<u32>();
    let grouped_requests = groups.values().map(|group| group.requests).sum::<u32>();
    let ungrouped_sessions = identities.len() as u32 - grouped_sessions;
    let available_groups = groups.len() as u32;
    let entries_limited = groups.len() > MAX_PRESENTED_CLIENT_GROUPS;
    let mut limitations = vec![
        t!("view.session_census.groups.limit.population"),
        t!("view.session_census.groups.limit.participants"),
        t!("view.session_census.limit.point"),
        t!("view.session_census.limit.database_ids"),
        t!("view.session_census.limit.client"),
        t!("view.session_census.groups.limit.lifetimes"),
        t!("view.session_census.groups.limit.consistency"),
    ];
    if !verified {
        limitations.push(t!("view.session_census.groups.limit.unverified"));
    }
    if entries_limited {
        limitations.push(t!(
            "view.session_census.groups.limit.presentation",
            presented = format::digits(MAX_PRESENTED_CLIENT_GROUPS.to_string()),
            available = format::digits(available_groups.to_string())
        ));
    }
    SessionCensusClientGroups {
        title: t!("view.session_census.groups.title"),
        summary: t!(
            "view.session_census.groups.summary",
            sessions = grouped_sessions,
            requests = grouped_requests,
            groups = available_groups,
            ungrouped = ungrouped_sessions
        ),
        limitations,
        grouped_sessions,
        ungrouped_sessions,
        available_groups,
        entries_limited,
        entries: groups.into_iter().take(MAX_PRESENTED_CLIENT_GROUPS).map(|(key, group)| entry(key, group)).collect(),
    }
}

fn consistent_session<'a>(
    frame: &ActivityFrameEvidence<'a>,
    identity: &NativeActivityIdentity,
) -> Option<(SessionFacts<'a>, u32)> {
    if !frame.has_session_generation_authority(identity) {
        return None;
    }
    let mut rows = frame.session_rows(identity);
    let first = rows.next()?.observation;
    let facts = SessionFacts::from_row(first)?;
    let mut requests = u32::from(first.key.request.is_some());
    for row in rows {
        if SessionFacts::from_row(row.observation).as_ref() != Some(&facts) {
            return None;
        }
        requests += u32::from(row.observation.key.request.is_some());
    }
    Some((facts, requests))
}

fn entry(key: SessionClientGroupKey, group: Group<'_>) -> SessionCensusClientGroup {
    let reads = sum(&group.sessions, |session| session.logical_reads);
    let writes = sum(&group.sessions, |session| session.writes);
    let elapsed = sum(&group.sessions, |session| session.elapsed_ms);
    let mut limitations = if group
        .sessions
        .iter()
        .any(|session| session.elapsed_ms.is_some_and(|value| value.parse::<i32>().is_ok_and(|native| native < 0)))
    {
        vec![t!("view.session_census.groups.limit.elapsed")]
    } else {
        Vec::new()
    };
    match &key.database {
        SessionClientDatabase::Backend { .. } => limitations.push(t!("view.session_census.groups.limit.backend")),
        SessionClientDatabase::CurrentDefault { .. } => {
            limitations.push(t!("view.session_census.groups.limit.default_schema"))
        }
        SessionClientDatabase::CurrentSession { .. } => (),
    }
    let observed_sessions = group.sessions.len() as u32;
    let mut fields = vec![
        field(
            "session_database_id",
            t!("view.session_census.field.session_database_id"),
            key.database.native_id().map(str::to_owned),
        ),
        field(
            "session_database_name",
            t!("view.session_census.field.session_database_name"),
            key.database.name().map(str::to_owned),
        ),
        field("host", t!("view.session_census.field.host"), key.host.clone()),
        field("application", t!("view.session_census.field.application"), key.application.clone()),
        field("library", t!("view.session_census.field.library"), key.library.clone()),
        number(
            "observed_sessions",
            t!("view.session_census.groups.field.sessions"),
            Some(&observed_sessions.to_string()),
        ),
        number("observed_requests", t!("view.session_census.groups.field.requests"), Some(&group.requests.to_string())),
        field(
            "database_basis",
            t!("view.session_census.groups.field.database_basis"),
            Some(match &key.database {
                SessionClientDatabase::CurrentSession { .. } => {
                    t!("view.session_census.groups.database.current_session")
                }
                SessionClientDatabase::Backend { .. } => t!("view.session_census.groups.database.backend"),
                SessionClientDatabase::CurrentDefault { .. } => {
                    t!("view.session_census.groups.database.current_default")
                }
            }),
        ),
    ];
    fields.extend(time_fields(&group.sessions));
    fields.extend([
        number("session_logical_reads", t!("view.session_census.groups.field.logical_reads"), reads.as_deref()),
        number("session_writes", t!("view.session_census.groups.field.writes"), writes.as_deref()),
        number("session_age_ms", t!("view.session_census.groups.field.elapsed"), elapsed.as_deref()),
    ]);
    SessionCensusClientGroup { key, observed_sessions, observed_requests: group.requests, limitations, fields }
}

fn time_fields(sessions: &[SessionFacts<'_>]) -> Vec<culprit_core::SessionCensusField> {
    match sessions.first().map(|session| session.times) {
        Some(SessionTimes::OffsetFree { .. }) => {
            let login = sessions
                .iter()
                .map(|session| match session.times {
                    SessionTimes::OffsetFree { login, .. } => Some(login),
                    _ => None,
                })
                .collect::<Option<Vec<_>>>()
                .and_then(|times| times.into_iter().min().map(str::to_owned));
            let end = sessions
                .iter()
                .map(|session| match session.times {
                    SessionTimes::OffsetFree { last_end, .. } => last_end,
                    _ => None,
                })
                .collect::<Option<Vec<_>>>()
                .and_then(|times| times.into_iter().max().map(str::to_owned));
            vec![
                field("earliest_login", t!("view.session_census.groups.field.earliest_login"), login),
                field("latest_end", t!("view.session_census.groups.field.latest_end"), end),
            ]
        }
        Some(SessionTimes::BackendUtc { .. }) => {
            let started = sessions
                .iter()
                .map(|session| match session.times {
                    SessionTimes::BackendUtc { started } => Some(started),
                    _ => None,
                })
                .collect::<Option<Vec<_>>>()
                .and_then(|times| times.into_iter().min().map(str::to_owned));
            vec![field("earliest_backend_start_utc", t!("view.session_census.groups.field.backend_start"), started)]
        }
        _ => Vec::new(),
    }
}

/// At most MAX_ACTIVITY_FRAME_OBSERVATIONS u64 values fit exactly in u128. Missing/overflow never becomes zero.
fn sum<'a>(sessions: &[SessionFacts<'a>], value: impl Fn(&SessionFacts<'a>) -> Option<&'a str>) -> Option<String> {
    sessions
        .iter()
        .try_fold(0u128, |total, session| total.checked_add(value(session)?.parse::<u128>().ok()?))
        .map(|total| total.to_string())
}

#[cfg(test)]
mod tests;
