use crate::probe::{self, node_id};
use crate::read::query;
use culprit_core::{Engine, Environment, Error, ErrorCode, LiveState, LockWait, Result, Session, SessionAction, t};
use culprit_postgres::wire::{Connector, INTERACTIVE, NativeRead, ReadSession};
use serde_json::Value;
use std::collections::HashMap;

fn text(v: &Value, key: &str) -> Option<String> {
    v.get(key)?.as_str().map(str::to_string)
}
fn elapsed(now: &str, then: Option<&str>) -> Option<f64> {
    let seconds = culprit_core::format::epoch_seconds(now)? - culprit_core::format::epoch_seconds(then?)?;
    (seconds >= 0.0).then_some(seconds * 1000.0)
}
pub(crate) fn literal(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

pub(crate) async fn live(connector: &Connector, environment: Environment) -> Result<LiveState> {
    let mut observed = crate::read::application(connector.clone(), std::time::Duration::ZERO);
    let result = read_live(&mut observed, environment).await;
    observed.close().await;
    result
}
async fn read_live(observed: &mut ReadSession, environment: Environment) -> Result<LiveState> {
    let p = probe::read_from(observed).await?;
    if p.global {
        crate::global::nodes_from(observed).await?;
    }
    let relation = if p.global { "pg_catalog.\"gv$pg_stat_activity\"" } else { "pg_catalog.pg_stat_activity" };
    let count = observed.notices();
    let rows=query(observed,&format!("SELECT to_jsonb(a)::text AS sample FROM {relation} a WHERE datname={}
        AND backend_type IN ('client backend','yb-conn-mgr worker connection') AND COALESCE(application_name,'') <> 'culprit' LIMIT 1001",literal(&p.database))).await?;
    let mut notes = vec![];
    if !p.global {
        notes.push(match p.node.as_deref() {
            Some(node) => t!("provider.yugabyte.sessions.node_scope", node = node),
            None => t!("provider.yugabyte.sessions.unknown_node_scope"),
        });
    }
    if observed.notices() != count {
        notes.push(t!("provider.yugabyte.sessions.node_skipped"));
    }
    if rows.len() > 1000 {
        notes.push(t!("provider.yugabyte.sessions.truncated"));
    }
    let mut sessions = Vec::new();
    let mut transactions = HashMap::new();
    let mut backends = HashMap::new();
    for row in rows.into_iter().take(1000) {
        let Some(raw) = row.text("sample") else { continue };
        let v: Value =
            serde_json::from_str(&raw).map_err(|_| Error::invalid(t!("provider.yugabyte.error.malformed_session")))?;
        let Some(pid) = v.get("pid").and_then(Value::as_i64).filter(|n| *n > 0) else {
            notes.push(t!("provider.yugabyte.sessions.no_backend"));
            continue;
        };
        let node = if p.global { text(&v, "server_uuid").and_then(|s| node_id(&s)) } else { p.node.clone() };
        if node.is_none() {
            notes.push(t!("provider.yugabyte.sessions.unknown_node"));
        }
        let started = text(&v, "backend_start").unwrap_or_default();
        let id = format!("{}/{pid}/{started}", node.as_deref().unwrap_or("unknown"));
        if let Some(node) = node {
            backends.insert((node, pid.to_string()), id.clone());
        }
        if let Some(tx) = text(&v, "yb_backend_xid") {
            transactions.insert(tx.replace('-', ""), id.clone());
        }
        let sql = text(&v, "query").filter(|s| s != "<insufficient privilege>");
        if sql.is_none() && text(&v, "state").as_deref().is_none_or(|s| s == "active") {
            notes.push(t!("provider.yugabyte.sessions.hidden"));
        }
        sessions.push(Session {
            id,
            user: text(&v, "usename"),
            application: text(&v, "application_name"),
            host: text(&v, "client_addr"),
            database: text(&v, "datname"),
            state: text(&v, "state").unwrap_or_else(|| "unknown".into()),
            command: None,
            waiting_on_lock: Some(
                crate::waits::category(
                    text(&v, "wait_event_type").as_deref().unwrap_or(""),
                    text(&v, "wait_event").as_deref().unwrap_or(""),
                ) == "Lock",
            ),
            wait: match (text(&v, "wait_event_type"), text(&v, "wait_event")) {
                (Some(kind), Some(event)) => Some(format!("{kind}: {event}")),
                (kind, event) => event.or(kind),
            },
            wait_resources: None,
            wait_ms: None,
            blocked_by: None,
            elapsed_ms: if text(&v, "state").as_deref() == Some("active") {
                elapsed(&p.now, text(&v, "query_start").as_deref())
            } else {
                None
            },
            transaction_ms: elapsed(&p.now, text(&v, "xact_start").as_deref()),
            cpu_ms: None,
            reads: None,
            sql,
        });
    }
    // This session's metadata filter may include young waiters without changing application or cluster settings.
    // Native lock collection still has transaction/per-tablet caps; an empty result cannot establish absence.
    if query(observed, "SET yb_locks_min_txn_age=0").await.is_err() {
        notes.push(t!("provider.yugabyte.sessions.young_locks"));
    }
    notes.push(t!("provider.yugabyte.sessions.lock_limits"));
    let count = observed.notices();
    let locks = query(observed, &lock_query(p.oid, None, 2001)?).await;
    let mut waits = Vec::<LockWait>::new();
    match locks {
        Ok(rows) => {
            if rows.len() > 2000 || observed.notices() != count {
                notes.push(t!("provider.yugabyte.sessions.locks_partial"));
            }
            for row in rows.into_iter().take(2000) {
                let waiter = node_id(&row.string("node"))
                    .and_then(|node| backends.get(&(node, row.string("pid"))).cloned())
                    .or_else(|| row.text("transaction").and_then(|id| transactions.get(&id.replace('-', "")).cloned()));
                let Some(waiter) = waiter else { continue };
                let blockers = row.string("blocked_by");
                let blocker = blockers.trim_matches(['{', '}']).split(',').find_map(|tx| {
                    let tx = tx.trim().trim_matches('"').replace('-', "");
                    (!tx.is_empty())
                        .then(|| transactions.get(&tx).cloned().unwrap_or_else(|| format!("transaction/{tx}")))
                });
                if let Some(s) = sessions.iter_mut().find(|s| s.id == waiter) {
                    s.blocked_by = s.blocked_by.take().or(blocker);
                }
                waits.push(LockWait {
                    session: waiter,
                    resource: row.text("locktype"),
                    object: row.text("object"),
                    index: None,
                    mode: row.text("mode"),
                });
            }
        }
        Err(_) => notes.push(t!("provider.yugabyte.sessions.locks_unavailable")),
    }
    notes.sort();
    notes.dedup();
    let blocking = culprit_core::blocking::analyze(Engine::Yugabyte, &sessions, &waits, &[]);
    Ok(LiveState {
        taken_at: p.now,
        sessions,
        waits: vec![],
        notes,
        blocking,
        session_actions: if environment.is_production() || p.node.is_none() {
            vec![]
        } else {
            Engine::Yugabyte.spec().session_actions()
        },
        session_actions_note: if environment.is_production() {
            Some(t!("provider.yugabyte.sessions.actions_production"))
        } else {
            Some(t!("provider.yugabyte.sessions.actions_rules"))
        },
    })
}

pub(crate) fn target(id: &str) -> Result<(String, i32, String)> {
    culprit_core::ysql_session_target(id)
}

pub(crate) async fn interrupt(connector: &Connector, id: &str, action: SessionAction) -> Result<()> {
    let (node, pid, started) = target(id)?;
    let function = match action {
        SessionAction::Cancel => "pg_cancel_backend",
        SessionAction::Terminate => "pg_terminate_backend",
    };
    let client = connector.connect(INTERACTIVE).await?;
    let row=culprit_postgres::wire::query_one(&client,&format!("SELECT pg_catalog.{function}(a.pid) AS stopped FROM pg_catalog.pg_stat_activity a
        WHERE a.pid={pid} AND a.datname=pg_catalog.current_database() AND a.backend_type='client backend'
        AND a.pid <> pg_catalog.pg_backend_pid() AND COALESCE(a.application_name,'') <> 'culprit'
        AND a.backend_start={}::timestamptz AND pg_catalog.replace(pg_catalog.yb_get_local_tserver_uuid()::text,'-','')={}",literal(&started),literal(&node))).await?;
    if row.is_some_and(|r| r.flag("stopped")) {
        Ok(())
    } else {
        Err(Error::new(ErrorCode::NotFound, t!("provider.yugabyte.error.session_gone"))
            .with_hint(t!("provider.yugabyte.error.session_gone_hint")))
    }
}

/// Native DocDB lock projection shared by Live and targeted persistent-issue capture. No key bytes or SQL bodies.
pub(crate) fn lock_query(database: u32, transaction: Option<&str>, limit: usize) -> Result<String> {
    let transaction = match transaction {
        None => "NULL".into(),
        Some(value) if value.len() == 32 && value.bytes().all(|b| b.is_ascii_hexdigit()) => format!("'{value}'::uuid"),
        _ => return Err(culprit_core::telemetry::CollectionStop::InvalidPage.error()),
    };
    Ok(format!(
        "SELECT l.node,l.pid::text AS pid,l.transaction_id::text AS transaction,l.blocked_by::text,l.mode::text,l.granted,l.locktype,n.nspname||'.'||c.relname AS object,CASE WHEN l.waitstart IS NOT NULL AND clock_timestamp()>=l.waitstart THEN floor(extract(epoch FROM clock_timestamp()-l.waitstart)*1000)::text END AS wait_ms FROM pg_catalog.yb_lock_status(NULL,{transaction}) l LEFT JOIN pg_catalog.pg_class c ON c.oid=l.relation LEFT JOIN pg_catalog.pg_namespace n ON n.oid=c.relnamespace WHERE l.database={database} AND NOT l.granted LIMIT {limit}"
    ))
}
