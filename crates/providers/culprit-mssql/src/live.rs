use std::collections::BTreeMap;

use culprit_core::{Error, ErrorCode, LiveState, Result, Session, SessionAction, WaitStat};

use crate::conn::Pool;
use crate::waits::{BENIGN_WAITS, wait_type_category};
mod actions;
pub(crate) use actions::interrupt;
#[cfg(test)]
mod tests;

const LIVE_ACTIVITY_ROWS: u32 = 10_000;
const LIVE_ACTIVITY_PAGE_BYTES: u32 = 8 * 1024 * 1024;
const LIVE_STATEMENT_TIMEOUT_MS: u32 = 10_000;
const LIVE_ACTION_QUERIES: u32 = 9;

pub(crate) async fn collect(
    pool: std::sync::Arc<Pool>,
    host: String,
    port: u16,
    qualify_actions: bool,
) -> Result<LiveState> {
    use crate::activity::{ActivityReader, IdentityDetail, ReadOptions};
    use culprit_core::telemetry::CollectionBudget;
    let mut budget = CollectionBudget {
        max_snapshot_rows: LIVE_ACTIVITY_ROWS,
        page_bytes: LIVE_ACTIVITY_PAGE_BYTES,
        statement_timeout_ms: LIVE_STATEMENT_TIMEOUT_MS,
        ..CollectionBudget::default()
    };
    if qualify_actions {
        budget.max_queries = LIVE_ACTION_QUERIES;
    }
    let options = ReadOptions { budget, identity: IdentityDetail::Endpoint };
    let mut reader = ActivityReader::open(pool, options, host, port)?;
    collect_qualified_from(&mut reader, qualify_actions).await
}

#[cfg(test)]
pub(crate) async fn collect_from<R: crate::read::NativeRead>(
    reader: &mut crate::activity::ActivityReader<R>,
) -> Result<LiveState> {
    collect_qualified_from(reader, false).await
}

async fn collect_qualified_from<R: crate::read::NativeRead>(
    reader: &mut crate::activity::ActivityReader<R>,
    qualify_actions: bool,
) -> Result<LiveState> {
    use culprit_core::telemetry::{CollectionStop, SourceShutdown};
    let result = read(reader, qualify_actions).await;
    if reader.close().await == SourceShutdown::Unconfirmed {
        return Err(CollectionStop::CancellationUnconfirmed.error());
    }
    result
}

pub(crate) async fn read<R: crate::read::NativeRead>(
    reader: &mut crate::activity::ActivityReader<R>,
    qualify_actions: bool,
) -> Result<LiveState> {
    use culprit_core::telemetry::ActivityDetailStatus;
    let authority = if qualify_actions {
        crate::activity::capture::ActivityAuthorityPolicy::AdministrativeCandidates
    } else {
        crate::activity::capture::ActivityAuthorityPolicy::MetadataOnly
    };
    let mut pending = crate::activity::capture::PendingActivityFrame::begin(reader, authority).await?;
    pending.require_complete()?;
    let keys = pending.baseline.frame.sql_context_keys();
    let limit = reader.sql_limit();
    let details = reader.enrich(&mut pending.baseline.frame, &keys, true, limit).await?;
    let completed = pending.finish(reader).await?;
    let context = completed.action_context;
    let frame = completed.frame;
    let activity_census = culprit_core::telemetry::ActivityCensusRead::new(
        completed.description,
        frame.records.iter().cloned().map(crate::activity::ActivityRecord::into_redacted_observation).collect(),
        true,
    )?;
    let mut sessions: Vec<_> = {
        let census = activity_census.frame()?;
        frame
            .records
            .iter()
            .map(|row| {
                let context =
                    context.as_ref().filter(|_| census.has_session_generation_authority(&row.facts.key.session));
                session_of(row, context)
            })
            .collect()
    };
    sessions.sort_by(|a, b| b.elapsed_ms.partial_cmp(&a.elapsed_ms).unwrap_or(std::cmp::Ordering::Equal));
    let mut notes = if matches!(details, ActivityDetailStatus::Unavailable { .. }) {
        vec![culprit_core::t!("provider.mssql.activity.details_unavailable")]
    } else {
        Vec::new()
    };
    if qualify_actions && context.is_none() {
        notes.push(culprit_core::t!("model.session_action.unavailable"));
    }
    let session_actions = if qualify_actions
        && sessions.iter().any(|session| {
            session.action_target.as_ref().is_some_and(|target| target.is_valid_for(SessionAction::Terminate))
        }) {
        vec![SessionAction::Terminate]
    } else {
        Vec::new()
    };
    Ok(LiveState {
        session_census: None,
        activity_census: Some(Box::new(activity_census)),
        notes,
        taken_at: frame.context.at,
        waits: current_waits(&sessions),
        sessions,
        session_actions,
        session_actions_note: None,
        session_action_route: None,
        blocking: None,
    })
}

/// Cancelling a statement, which SQL Server can only do by ending its session.
pub(crate) fn cancel_unsupported() -> Error {
    Error::new(ErrorCode::Unsupported, culprit_core::t!("provider.mssql.session.cancel_unsupported"))
        .with_hint(culprit_core::t!("provider.mssql.session.cancel_unsupported.hint"))
}

pub(crate) fn session_gone(id: u64) -> Error {
    Error::not_found(culprit_core::t!("provider.mssql.session.gone", id = id.to_string()))
}

pub(crate) fn own_session(id: u64) -> Error {
    Error::invalid(culprit_core::t!("provider.mssql.session.own", id = id.to_string()))
}

pub(crate) fn system_session(id: u64) -> Error {
    Error::invalid(culprit_core::t!("provider.mssql.session.system", id = id.to_string()))
}

fn number(value: Option<&str>) -> Option<f64> {
    value.and_then(|value| value.parse().ok())
}
fn session_of(
    record: &crate::activity::ActivityRecord,
    context: Option<&culprit_core::telemetry::NativeReadContext>,
) -> Session {
    use culprit_core::telemetry::ActivityWaitOwner;
    let facts = &record.facts;
    let counters = facts.counters();
    let client = facts.client.as_ref();
    let wait = facts.wait.as_ref();
    let wait_type = wait.and_then(|wait| wait.native_type.clone());
    let blocked_by = wait.and_then(|wait| {
        wait.owners.iter().find_map(|owner| match owner {
            ActivityWaitOwner::Session { id, .. } => Some(id.clone()),
            ActivityWaitOwner::Transaction { native_id, .. } => Some(native_id.clone()),
            ActivityWaitOwner::Unknown { .. } => None,
        })
    });
    let sql = if facts.key.request.is_some() { &record.current_sql } else { &record.previous_sql };
    Session {
        progress_eligible: None,
        sql_context: sql.text().map(|_| {
            if facts.key.request.is_some() {
                culprit_core::SessionStatementContext::CurrentRequest
            } else {
                culprit_core::SessionStatementContext::PriorSessionBatch
            }
        }),
        activity_key: Some(facts.key.clone()),
        id: facts.key.session.id.clone(),
        action_target: actions::target(record, context),
        user: client.and_then(|client| client.user.clone()),
        application: client.and_then(|client| client.application.clone()).filter(|value| !value.is_empty()),
        host: client.and_then(|client| client.host.clone()),
        database: facts.database.clone(),
        state: facts.state.clone(),
        command: facts.command.clone(),
        waiting_on_lock: facts.waiting_on_lock,
        wait: wait_type,
        wait_ms: number(wait.and_then(|wait| wait.elapsed_ms.as_deref())),
        wait_resources: wait.map(|wait| wait.identified_resources.clone()).filter(|resources| !resources.is_empty()),
        blocked_by,
        elapsed_ms: number(facts.elapsed_ms.as_deref().filter(|_| facts.key.request.is_some())),
        transaction_ms: number(facts.transaction_age_ms.as_deref()),
        cpu_ms: number(counters.cpu_ms.as_deref()),
        reads: number(counters.logical_reads.as_deref()),
        sql: sql.text().map(str::trim).filter(|sql| !sql.is_empty()).map(String::from),
    }
}

fn current_waits(sessions: &[Session]) -> Vec<WaitStat> {
    let mut by: BTreeMap<(&'static str, Option<culprit_core::telemetry::WaitMechanism>), (f64, f64, String)> =
        BTreeMap::new();
    for s in sessions {
        let Some(wait) = s.wait.as_deref() else {
            continue;
        };
        if BENIGN_WAITS.contains(&wait) {
            continue;
        }
        let Some(wait_ms) = s.wait_ms else { continue };
        let entry = by.entry((wait_type_category(wait), crate::waits::mechanism(wait))).or_default();
        entry.0 += wait_ms;
        entry.1 += 1.0;
        if entry.2.is_empty() {
            entry.2 = wait.to_string();
        }
    }
    let mut out: Vec<WaitStat> = by
        .into_iter()
        .map(|((category, mechanism), (ms, tasks, wait_type))| WaitStat {
            mechanism,
            category: category.into(),
            wait_type: Some(wait_type),
            wait_ms: ms,
            tasks: Some(tasks),
        })
        .collect();
    out.sort_by(|a, b| b.wait_ms.total_cmp(&a.wait_ms));
    out
}
