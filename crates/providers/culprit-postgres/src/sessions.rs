//! Actions retain observed backend/request generations. Native PID signalling still has a check-to-signal race.
mod context;
pub(crate) use context::Context;
#[cfg(test)]
mod tests;

use crate::{
    connect::Connector,
    native,
    read::{NativeRead, QueryRead, ReadSession},
};
use culprit_core::telemetry::*;
use culprit_core::{Error, Result, SessionAction, SessionActionOutcome, SessionActionTarget, session_number, t};

const ACTION_QUERIES: u32 = 7; // Owned setup, two metadata/member bounds, one backend point and one signal.

pub(crate) async fn interrupt(
    connector: &Connector,
    target: &SessionActionTarget,
    action: SessionAction,
) -> Result<SessionActionOutcome> {
    let budget = CollectionBudget {
        max_queries: ACTION_QUERIES,
        total_timeout_ms: culprit_core::limits::OBSERVE_TIMEOUT.as_millis() as u32,
        ..Default::default()
    };
    let mut reader = ReadSession::new(connector.clone(), budget.clone());
    let result = interrupt_from(&mut reader, connector, target, action, &budget).await;
    if reader.close().await == SourceShutdown::Unconfirmed {
        return Err(CollectionStop::CancellationUnconfirmed.error());
    }
    result
}

async fn interrupt_from<R: QueryRead>(
    reader: &mut R,
    connector: &Connector,
    target: &SessionActionTarget,
    action: SessionAction,
    budget: &CollectionBudget,
) -> Result<SessionActionOutcome> {
    if !target.is_valid_for(action) || target.context.target.engine() != culprit_core::Engine::Postgres {
        return Err(SessionActionTarget::unavailable());
    }
    canonical_time(&target.activity.session.generation).map_err(|_| SessionActionTarget::changed())?;
    if action == SessionAction::Cancel {
        canonical_time(&target.activity.request.as_ref().ok_or_else(SessionActionTarget::changed)?.generation)
            .map_err(|_| SessionActionTarget::changed())?;
    }
    let pid = u32::try_from(session_number(&target.activity.session.id)?)
        .ok()
        .filter(|pid| *pid > 0 && *pid <= i32::MAX as u32)
        .ok_or_else(|| {
            Error::invalid(t!("provider.postgres.error.not_a_pid", session = &target.activity.session.id))
        })?;
    let metadata = native::metadata::activity(reader, &connector.host, connector.port).await?;
    let before = Context::observe(reader, &metadata).await?.ok_or_else(SessionActionTarget::unavailable)?;
    let frame = native::activity::observe_rows(
        reader,
        &native::activity::target_query(metadata.version >= 140_000, pid, "'client backend'", ""),
        &metadata.database_oid,
        budget,
    )
    .await?;
    let original = frame.observations().into_iter().next().ok_or_else(SessionActionTarget::changed)?;
    if original.client.as_ref().and_then(|client| client.application.as_deref()) == Some("culprit") {
        return Err(Error::invalid(t!("provider.postgres.session.own", pid = pid.to_string())));
    }
    let context =
        before.recheck(reader, &connector.host, connector.port).await?.ok_or_else(SessionActionTarget::changed)?;
    let current = SessionActionTarget { context, activity: original.key, statement: original.statement_identity };
    target.require_current(&current, action)?;
    signal(reader, pid, target, action).await
}

async fn signal<R: QueryRead>(
    reader: &mut R,
    pid: u32,
    target: &SessionActionTarget,
    action: SessionAction,
) -> Result<SessionActionOutcome> {
    let function = match action {
        SessionAction::Cancel => "pg_cancel_backend",
        SessionAction::Terminate => "pg_terminate_backend",
    };
    let start = literal(&target.activity.session.generation);
    let request = match action {
        SessionAction::Cancel => format!(
            " AND a.state IN ('active','fastpath function call') AND a.query_start={}::timestamptz",
            literal(&target.activity.request.as_ref().ok_or_else(SessionActionTarget::changed)?.generation)
        ),
        SessionAction::Terminate => String::new(),
    };
    // WHERE shortens the native race; pg_*_backend accepts only a PID and does not atomically compare generations.
    let rows = reader
        .query(
            &format!(
                "SELECT pg_catalog.{function}(a.pid) AS signalled FROM pg_catalog.pg_stat_activity a
         WHERE a.pid={pid} AND a.backend_start={start}::timestamptz
           AND a.datid=(SELECT oid FROM pg_catalog.pg_database WHERE datname=pg_catalog.current_database())
           AND a.backend_type='client backend' AND a.pid<>pg_catalog.pg_backend_pid()
           AND COALESCE(a.application_name,'')<>'culprit'{request}"
            ),
            1,
        )
        .await?;
    if rows.len() != 1 {
        return Err(SessionActionTarget::changed());
    }
    if !matches!(rows[0].text("signalled").as_deref(), Some("t" | "true")) {
        return Err(SessionActionTarget::signal_failed());
    }
    Ok(SessionActionOutcome::SignalSent)
}

fn literal(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

pub(crate) fn target(
    context: &NativeReadContext,
    record: &native::activity::ActivityRecord,
) -> Option<SessionActionTarget> {
    let facts = record.facts.as_ref()?;
    if facts.scope != ActivityScopeBasis::SessionDatabase
        || facts.client.as_ref().and_then(|client| client.application.as_deref()) == Some("culprit")
    {
        return None;
    }
    let target = SessionActionTarget {
        context: context.clone(),
        activity: facts.key.clone(),
        statement: facts.statement_identity.clone(),
    };
    target.is_valid_for(SessionAction::Terminate).then_some(target)
}

pub(crate) fn offered_actions(
    sessions: &[culprit_core::Session],
    environment: culprit_core::Environment,
) -> Vec<SessionAction> {
    if environment.is_production() {
        return vec![];
    }
    culprit_core::Engine::Postgres
        .spec()
        .session_actions()
        .into_iter()
        .filter(|action| {
            sessions
                .iter()
                .any(|session| session.action_target.as_ref().is_some_and(|target| target.is_valid_for(*action)))
        })
        .collect()
}
