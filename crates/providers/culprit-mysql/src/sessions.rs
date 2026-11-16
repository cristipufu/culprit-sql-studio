//! Source-qualified live targets and bounded administrative signals. KILL still has a check-to-signal race.
#[cfg(test)]
mod tests;

use crate::native::local_context::{self as context, Context};
use crate::{
    connect::{Connector, Scope, TAG},
    native,
    read::{OwnedRead, QueryRead, ReadSession, confirmed_result},
    rows::Rec,
};
use culprit_core::telemetry::*;
use culprit_core::{Error, Result, SessionAction, SessionActionOutcome, SessionActionTarget, session_number, t};
use mysql_async::Value;
use std::sync::Arc;

// Seven setup phases +five at either native source bound +one prepared target (two phases) +one KILL.
const ACTION_QUERIES: u32 = 20;

pub(crate) async fn interrupt(
    connector: Arc<Connector>,
    target: &SessionActionTarget,
    action: SessionAction,
) -> Result<SessionActionOutcome> {
    let budget = CollectionBudget {
        max_queries: ACTION_QUERIES,
        statement_timeout_ms: culprit_core::limits::OBSERVE_TIMEOUT.as_millis() as u32,
        total_timeout_ms: culprit_core::limits::OBSERVE_TIMEOUT.as_millis() as u32,
        ..Default::default()
    };
    let mut reader = ReadSession::new(connector.clone(), budget, Scope::Server);
    interrupt_from(&mut reader, &connector, target, action).await
}

async fn interrupt_from<R: OwnedRead>(
    reader: &mut R,
    connector: &Connector,
    target: &SessionActionTarget,
    action: SessionAction,
) -> Result<SessionActionOutcome> {
    let result = signal_from(reader, connector, target, action).await;
    confirmed_result(reader, result).await
}

async fn signal_from<R: QueryRead>(
    reader: &mut R,
    connector: &Connector,
    target: &SessionActionTarget,
    action: SessionAction,
) -> Result<SessionActionOutcome> {
    if !target.is_valid_for(action) {
        return Err(SessionActionTarget::unavailable());
    }
    let id = session_number(&target.activity.session.id)?;
    if id == 0 || id.to_string() != target.activity.session.id {
        return Err(SessionActionTarget::unavailable());
    }
    let before = Context::read(reader, connector).await?.ok_or_else(SessionActionTarget::unavailable)?;
    let after = Context::read(reader, connector).await?.ok_or_else(SessionActionTarget::unavailable)?;
    let mut context = before.completed(&after).ok_or_else(SessionActionTarget::changed)?;
    let rows = reader
        .read(
            &native::activity::target_query(),
            vec![Value::from(id), Value::from(before.connection.database_name.clone())],
            1,
        )
        .await?;
    let [row] = rows.as_slice() else { return Err(SessionActionTarget::changed()) };
    context.observed_until = canonical_time(&native::text(row, "observed_at")?)?;
    let current = observed_target(row, &context, before.instrumented)?.ok_or_else(SessionActionTarget::changed)?;
    target.require_current(&current, action)?;
    let statement = match action {
        SessionAction::Cancel => format!("KILL QUERY {id}"),
        SessionAction::Terminate => format!("KILL CONNECTION {id}"),
    };
    // No further metadata or reconnect intervenes after the exact request check. MySQL KILL still selects a
    // connection/current request, not a native generation. A successful reply only accepts
    // the signal; it does not confirm completion. https://dev.mysql.com/doc/refman/8.4/en/kill.html
    reader.read(&statement, vec![], 0).await?;
    Ok(SessionActionOutcome::SignalSent)
}

pub(crate) struct LiveRows {
    pub rows: Vec<Rec>,
    context: Option<NativeReadContext>,
    instrumented: bool,
}

impl LiveRows {
    pub(crate) fn session_actions(&self) -> Vec<SessionAction> {
        let mut cancel = false;
        let mut terminate = false;
        for target in self.rows.iter().filter_map(|row| self.target(row)) {
            cancel |= target.is_valid_for(SessionAction::Cancel);
            terminate |= target.is_valid_for(SessionAction::Terminate);
        }
        [(SessionAction::Cancel, cancel), (SessionAction::Terminate, terminate)]
            .into_iter()
            .filter_map(|(action, supported)| supported.then_some(action))
            .collect()
    }

    pub(crate) fn target(&self, row: &Rec) -> Option<SessionActionTarget> {
        let context = self.context.as_ref()?;
        observed_target(row, context, self.instrumented).ok().flatten()
    }
}

fn observed_target(row: &Rec, context: &NativeReadContext, instrumented: bool) -> Result<Option<SessionActionTarget>> {
    let Some(database) = context.target.database().map(|database| database.database.as_str()) else { return Ok(None) };
    if row.text("info").is_some_and(|info| info.contains(TAG)) {
        return Err(Error::invalid(t!("provider.mysql.session.own", id = row.string("id"))));
    }
    if row.text("user_name").as_deref().is_none_or(|user| user.is_empty() || user == "system user")
        || matches!(row.text("command").as_deref(), Some("Daemon" | "Binlog Dump" | "Binlog Dump GTID"))
    {
        return Ok(None);
    }
    let Some(original) = native::activity::decode(row, database, instrumented)? else { return Ok(None) };
    if original.database.as_deref() != Some(database) {
        return Ok(None);
    }
    let target = SessionActionTarget {
        context: context.clone(),
        activity: original.key,
        statement: original.statement_identity,
    };
    Ok(target.is_valid_for(SessionAction::Terminate).then_some(target))
}

pub(crate) async fn live_rows<R: QueryRead>(
    reader: &mut R,
    connector: &Connector,
    production: bool,
    extra: &str,
) -> Result<LiveRows> {
    let before = if production { None } else { Context::read(reader, connector).await? };
    let rows = reader.read(&live_query(extra, before.is_some()), vec![], culprit_core::limits::MAX_RESULT_ROWS).await;
    let (rows, before) = match rows {
        Ok(rows) => (rows, before),
        Err(error) if before.is_some() && context::optional(&error) => {
            // Missing instrumentation visibility must not hide ordinary process-list diagnostics.
            (reader.read(&live_query(extra, false), vec![], culprit_core::limits::MAX_RESULT_ROWS).await?, None)
        }
        Err(error) => return Err(error),
    };
    let instrumented = before.as_ref().is_some_and(|context| context.instrumented);
    let context = match before {
        Some(before) => Context::read(reader, connector).await?.and_then(|after| before.completed(&after)),
        None => None,
    };
    Ok(LiveRows { rows, context, instrumented })
}

fn live_query(extra: &str, actions: bool) -> String {
    let generations = if actions { format!(", {}", native::activity::GENERATION_COLUMNS) } else { String::new() };
    let joins = if actions {
        "LEFT JOIN performance_schema.threads t ON t.PROCESSLIST_ID=p.ID
         LEFT JOIN performance_schema.events_statements_current e ON e.THREAD_ID=t.THREAD_ID"
    } else {
        ""
    };
    format!(
        "SELECT p.ID AS id, p.USER AS user_name, p.HOST AS host, p.DB AS db, p.COMMAND AS command, p.TIME AS seconds,
             p.STATE AS state, p.INFO AS info, {extra},
             TIMESTAMPDIFF(MICROSECOND,tx.trx_started,NOW(6))/1000 AS trx_ms{generations}
             FROM information_schema.PROCESSLIST p
             LEFT JOIN information_schema.INNODB_TRX tx ON tx.trx_mysql_thread_id=p.ID
             {joins}
             WHERE p.ID<>CONNECTION_ID() AND p.COMMAND NOT IN ('Daemon','Binlog Dump','Binlog Dump GTID')
               AND p.USER<>'system user'"
    )
}
