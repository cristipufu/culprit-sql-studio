use super::{literal, text};
use crate::{native, probe, read};
use culprit_core::{Result, SessionAction, SessionActionOutcome, SessionActionTarget, telemetry::*};
use culprit_postgres::wire::{Connector, NativeRead, QueryRead, activity};
use serde_json::Value;

const ACTION_QUERIES: u32 = 9; // Setup, three identity reads at each bound, one exact backend and one signal.

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
    let mut reader = culprit_postgres::wire::ReadSession::for_engine(
        connector.clone(),
        budget.clone(),
        culprit_core::Engine::Yugabyte,
    );
    let result = interrupt_from(&mut reader, &connector.host, connector.port, target, action, &budget).await;
    if reader.close().await == SourceShutdown::Unconfirmed {
        return Err(CollectionStop::CancellationUnconfirmed.error());
    }
    result
}

async fn interrupt_from<R: QueryRead>(
    reader: &mut R,
    host: &str,
    port: u16,
    target: &SessionActionTarget,
    action: SessionAction,
    budget: &CollectionBudget,
) -> Result<SessionActionOutcome> {
    if !target.is_valid_for(action) || target.context.target.engine() != culprit_core::Engine::Yugabyte {
        return Err(SessionActionTarget::unavailable());
    }
    canonical_time(&target.activity.session.generation).map_err(|_| SessionActionTarget::changed())?;
    if action == SessionAction::Cancel {
        canonical_time(&target.activity.request.as_ref().ok_or_else(SessionActionTarget::changed)?.generation)
            .map_err(|_| SessionActionTarget::changed())?;
    }
    let (node, pid) = target.activity.session.id.split_once('/').ok_or_else(SessionActionTarget::changed)?;
    let node = probe::node_id(node)
        .filter(|node| node.bytes().any(|byte| byte != b'0'))
        .ok_or_else(SessionActionTarget::changed)?;
    let pid = pid
        .parse::<u32>()
        .ok()
        .filter(|pid| *pid > 0 && *pid <= i32::MAX as u32)
        .ok_or_else(SessionActionTarget::changed)?;
    let before = native::metadata::read(reader, host, port).await?;
    let after = native::metadata::read(reader, host, port).await?;
    if !before.same_activity_source(&after) {
        return Err(SessionActionTarget::changed());
    }
    let mut context = before.completed_action_context(&after).ok_or_else(SessionActionTarget::unavailable)?;
    if node != after.node {
        return Err(SessionActionTarget::changed());
    }
    let frame = activity::observe_rows(
        reader,
        &activity::target_query(false, pid, "'client backend'", ""),
        &after.probe.oid.to_string(),
        budget,
    )
    .await?;
    let record = frame.records.first().ok_or_else(SessionActionTarget::changed)?;
    let mut original = record.facts.clone().ok_or_else(SessionActionTarget::changed)?;
    if original.client.as_ref().and_then(|client| client.application.as_deref()) == Some("culprit") {
        return Err(SessionActionTarget::unavailable());
    }
    qualify(&mut original, &node);
    context.observed_until = canonical_time(&frame.at)?;
    let current = SessionActionTarget { context, activity: original.key, statement: original.statement_identity };
    target.require_current(&current, action)?;
    let function = match action {
        SessionAction::Cancel => "pg_cancel_backend",
        SessionAction::Terminate => "pg_terminate_backend",
    };
    let request = match action {
        SessionAction::Cancel => format!(
            " AND a.state IN ('active','fastpath function call') AND a.query_start={}::timestamptz",
            literal(&target.activity.request.as_ref().ok_or_else(SessionActionTarget::changed)?.generation)
        ),
        SessionAction::Terminate => String::new(),
    };
    // The native function signals a PID, not a generation. These predicates narrow the race but are not atomic CAS.
    let row = read::query_one(
        reader,
        &format!(
            "SELECT pg_catalog.{function}(a.pid) AS signalled FROM pg_catalog.pg_stat_activity a
        WHERE a.pid={pid} AND a.datid={} AND a.backend_start={}::timestamptz
          AND a.backend_type='client backend' AND a.pid<>pg_catalog.pg_backend_pid()
          AND COALESCE(a.application_name,'')<>'culprit'
          AND pg_catalog.replace(pg_catalog.yb_get_local_tserver_uuid()::text,'-','')={}{request}",
            after.probe.oid,
            literal(&target.activity.session.generation),
            literal(&node)
        ),
    )
    .await?
    .ok_or_else(SessionActionTarget::changed)?;
    if !row.flag("signalled") {
        return Err(SessionActionTarget::signal_failed());
    }
    Ok(SessionActionOutcome::SignalSent)
}

pub(super) fn qualify(row: &mut ActivityObservation, node: &str) {
    row.key.session.id = format!("{node}/{}", row.key.session.id);
    if let Some(identity) = &mut row.statement_identity {
        identity.qualifiers.insert("node".into(), node.into());
    }
}

/// Native JSON aliases feed the same generation decoder as the metadata baseline. No SQL text or timing estimate
/// supplies identity, and malformed/unavailable native generations leave the original display row without actions.
pub(super) fn original(row: &Value, p: &probe::Probe, node: &str) -> Option<ActivityObservation> {
    let mut fields: Vec<(String, Option<String>)> = [
        ("pid", "pid"),
        ("database_oid", "datid"),
        ("database", "datname"),
        ("username", "usename"),
        ("application_name", "application_name"),
        ("host", "client_addr"),
        ("state", "state"),
        ("wait_event_type", "wait_event_type"),
        ("wait_event", "wait_event"),
    ]
    .into_iter()
    .map(|(alias, key)| {
        let value = row.get(key).and_then(|value| match value {
            Value::String(value) => Some(value.clone()),
            Value::Number(value) => Some(value.to_string()),
            _ => None,
        });
        (alias.into(), value)
    })
    .collect();
    for (alias, key) in
        [("backend_start", "backend_start"), ("query_start", "query_start"), ("transaction_start", "xact_start")]
    {
        let value = text(row, key).map(|value| activity::generation_timestamp(&value)).transpose().ok()?;
        fields.push((alias.into(), value));
    }
    let record = activity::decode_row(culprit_postgres::wire::Row::from_fields(fields), &p.oid.to_string()).ok()?;
    let mut original = record.facts?;
    qualify(&mut original, node);
    Some(original)
}

pub(super) fn target(
    row: &Value,
    original: &ActivityObservation,
    context: Option<&NativeReadContext>,
    node: &str,
) -> Option<SessionActionTarget> {
    let context = context?;
    if text(row, "backend_type").as_deref() != Some("client backend")
        || original.scope != ActivityScopeBasis::SessionDatabase
    {
        return None;
    }
    let SourcePartition::InstanceLocal { member: Some(member), .. } = &context.partition else { return None };
    if member.native.id != node {
        return None;
    }
    let target = SessionActionTarget {
        context: context.clone(),
        activity: original.key.clone(),
        statement: original.statement_identity.clone(),
    };
    target.is_valid_for(SessionAction::Terminate).then_some(target)
}

pub(super) fn offered(
    sessions: &[culprit_core::Session],
    environment: culprit_core::Environment,
) -> Vec<SessionAction> {
    if environment.is_production() {
        return vec![];
    }
    culprit_core::Engine::Yugabyte
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

#[cfg(test)]
mod tests;
