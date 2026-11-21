//! One original census owns both session deduplication and observed client-label totals.
use super::{field, number};
use culprit_core::{
    SessionCensusClientGroup, SessionCensusClientGroups, SessionClientGroupKey, format, t, telemetry::*,
};
use std::collections::{BTreeMap, BTreeSet};

const MAX_PRESENTED_CLIENT_GROUPS: usize = 64;

#[derive(PartialEq, Eq)]
struct SessionFacts<'a> {
    key: SessionClientGroupKey,
    login: &'a str,
    last_end: Option<&'a str>,
    elapsed_ms: Option<&'a str>,
    logical_reads: Option<&'a str>,
    writes: Option<&'a str>,
}
impl<'a> SessionFacts<'a> {
    fn from_row(row: &'a ActivityObservation) -> Option<Self> {
        let context = row.session_context.as_deref()?;
        let SessionNativeContext::Mssql(native) = context.native.as_deref()?;
        let database_id = native.session_database_id.as_deref().filter(|id| *id != "0")?;
        let client = row.client.as_ref();
        Some(Self {
            key: SessionClientGroupKey {
                database_id: database_id.into(),
                database_name: native.session_database_name.clone(),
                host: client.and_then(|client| client.host.clone()),
                application: client.and_then(|client| client.application.clone()),
                library: context.client_library.clone(),
            },
            login: &native.times.login_without_offset,
            last_end: native.times.last_request_end_without_offset.as_deref(),
            elapsed_ms: native.elapsed_ms.as_deref(),
            logical_reads: row.session_counters.logical_reads.as_deref(),
            writes: row.session_counters.writes.as_deref(),
        })
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
            sessions = format::digits(grouped_sessions.to_string()),
            requests = format::digits(grouped_requests.to_string()),
            groups = format::digits(available_groups.to_string()),
            ungrouped = format::digits(ungrouped_sessions.to_string())
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
    let earliest_login = group.sessions.iter().map(|session| session.login).min().map(str::to_owned);
    let latest_end = group
        .sessions
        .iter()
        .map(|session| session.last_end)
        .collect::<Option<Vec<_>>>()
        .and_then(|times| times.into_iter().max().map(str::to_owned));
    let reads = sum(&group.sessions, |session| session.logical_reads);
    let writes = sum(&group.sessions, |session| session.writes);
    let elapsed = sum(&group.sessions, |session| session.elapsed_ms);
    let limitations = if group
        .sessions
        .iter()
        .any(|session| session.elapsed_ms.is_some_and(|value| value.parse::<i32>().is_ok_and(|native| native < 0)))
    {
        vec![t!("view.session_census.groups.limit.elapsed")]
    } else {
        Vec::new()
    };
    let observed_sessions = group.sessions.len() as u32;
    let fields = vec![
        field(
            "session_database_id",
            t!("view.session_census.field.session_database_id"),
            Some(key.database_id.clone()),
        ),
        field(
            "session_database_name",
            t!("view.session_census.field.session_database_name"),
            key.database_name.clone(),
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
        field("earliest_login", t!("view.session_census.groups.field.earliest_login"), earliest_login),
        field("latest_end", t!("view.session_census.groups.field.latest_end"), latest_end),
        number("session_logical_reads", t!("view.session_census.groups.field.logical_reads"), reads.as_deref()),
        number("session_writes", t!("view.session_census.groups.field.writes"), writes.as_deref()),
        number("session_age_ms", t!("view.session_census.groups.field.elapsed"), elapsed.as_deref()),
    ];
    SessionCensusClientGroup { key, observed_sessions, observed_requests: group.requests, limitations, fields }
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
