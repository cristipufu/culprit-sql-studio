use std::collections::BTreeMap;

use culprit_core::{Error, ErrorCode, LiveState, Result, Session, SessionAction, WaitStat, session_number};

use crate::conn::{Pool, execute};
use crate::read::rows as query;
use crate::waits::{BENIGN_WAITS, wait_type_category};

const LIVE_ACTIVITY_ROWS: u32 = 10_000;
const LIVE_ACTIVITY_PAGE_BYTES: u32 = 8 * 1024 * 1024;
const LIVE_STATEMENT_TIMEOUT_MS: u32 = 10_000;

pub(crate) async fn collect(pool: std::sync::Arc<Pool>, host: String, port: u16) -> Result<LiveState> {
    use crate::activity::{ActivityReader, ActivityScope, IdentityDetail, ReadOptions};
    use culprit_core::telemetry::CollectionBudget;
    let options = ReadOptions {
        budget: CollectionBudget {
            max_snapshot_rows: LIVE_ACTIVITY_ROWS,
            page_bytes: LIVE_ACTIVITY_PAGE_BYTES,
            statement_timeout_ms: LIVE_STATEMENT_TIMEOUT_MS,
            ..CollectionBudget::default()
        },
        scope: ActivityScope::Visible,
        identity: IdentityDetail::Endpoint,
    };
    let mut reader = ActivityReader::open(pool, options, host, port)?;
    collect_from(&mut reader).await
}

pub(crate) async fn collect_from<R: crate::read::NativeRead>(
    reader: &mut crate::activity::ActivityReader<R>,
) -> Result<LiveState> {
    use culprit_core::telemetry::{CollectionStop, SourceShutdown};
    let result = read(reader).await;
    if reader.close().await == SourceShutdown::Unconfirmed {
        return Err(CollectionStop::CancellationUnconfirmed.error());
    }
    result
}

pub(crate) async fn read<R: crate::read::NativeRead>(
    reader: &mut crate::activity::ActivityReader<R>,
) -> Result<LiveState> {
    use culprit_core::telemetry::ActivityDetailStatus;
    let mut frame = reader.observe().await?;
    let keys = frame.keys();
    let limit = reader.sql_limit();
    let details = reader.enrich(&mut frame, &keys, true, limit).await?;
    let mut sessions: Vec<_> = frame.records.iter().map(session_of).collect();
    sessions.sort_by(|a, b| b.elapsed_ms.partial_cmp(&a.elapsed_ms).unwrap_or(std::cmp::Ordering::Equal));
    let notes = if matches!(details, ActivityDetailStatus::Unavailable { .. }) {
        vec![culprit_core::t!("provider.mssql.activity.details_unavailable")]
    } else {
        Vec::new()
    };
    Ok(LiveState {
        notes,
        taken_at: frame.context.at,
        waits: current_waits(&sessions),
        sessions,
        session_actions: Vec::new(),
        session_actions_note: None,
        blocking: None,
    })
}

/// SQL Server has no way to stop another session's statement short of `KILL`, which ends the session and rolls back its
/// transaction. Culprit's own sessions and system sessions are refused.
pub(crate) async fn interrupt(pool: &std::sync::Arc<Pool>, session: &str, action: SessionAction) -> Result<()> {
    let id = session_number(session)?;
    if action == SessionAction::Cancel {
        return Err(cancel_unsupported());
    }
    let target = format!(
        "SELECT CAST(is_user_process AS int) AS user_process, program_name, CASE WHEN session_id = @@SPID THEN 1 ELSE 0 END AS own \
         FROM sys.dm_exec_sessions WHERE session_id = {id}"
    );
    let rows = pool
        .observe(|c| {
            let sql = target.clone();
            Box::pin(async move { query(c, &sql).await })
        })
        .await?;
    let Some(row) = rows.first() else {
        return Err(session_gone(id));
    };
    if row.f64("own") == Some(1.0) || row.str("program_name").is_some_and(|p| p.eq_ignore_ascii_case("culprit")) {
        return Err(own_session(id));
    }
    if row.f64("user_process") != Some(1.0) {
        return Err(system_session(id));
    }
    pool.with(|c| Box::pin(async move { execute(c, &format!("KILL {id}")).await })).await
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
fn session_of(record: &crate::activity::ActivityRecord) -> Session {
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
        id: facts.key.session.id.clone(),
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
        elapsed_ms: number(if facts.key.request.is_some() {
            facts.elapsed_ms.as_deref()
        } else {
            facts.last_request_age_ms.as_deref()
        }),
        transaction_ms: number(facts.transaction_age_ms.as_deref()),
        cpu_ms: number(counters.cpu_ms.as_deref()),
        reads: number(counters.logical_reads.as_deref()),
        sql: sql.text().map(str::trim).filter(|sql| !sql.is_empty()).map(String::from),
    }
}

fn current_waits(sessions: &[Session]) -> Vec<WaitStat> {
    let mut by: BTreeMap<&'static str, (f64, f64, String)> = BTreeMap::new();
    for s in sessions {
        let Some(wait) = s.wait.as_deref() else { continue };
        if BENIGN_WAITS.contains(&wait) {
            continue;
        }
        let Some(wait_ms) = s.wait_ms else { continue };
        let entry = by.entry(wait_type_category(wait)).or_default();
        entry.0 += wait_ms;
        entry.1 += 1.0;
        if entry.2.is_empty() {
            entry.2 = wait.to_string();
        }
    }
    let mut out: Vec<WaitStat> = by
        .into_iter()
        .map(|(category, (ms, tasks, wait_type))| WaitStat {
            category: category.into(),
            wait_type: Some(wait_type),
            wait_ms: ms,
            tasks: Some(tasks),
        })
        .collect();
    out.sort_by(|a, b| b.wait_ms.total_cmp(&a.wait_ms));
    out
}
