//! All native owner edges feed the existing graph, without a first-request/session representative.
use super::*;
use crate::{Result, telemetry::*};

pub fn analyze_activity(frame: &ActivityFrameEvidence<'_>) -> Result<Option<Blocking>> {
    let edges = ActivityEvidence::blocking_for_frame(frame);
    let mut owners = BTreeMap::<String, ActivityWaitOwner>::new();
    for edge in &edges {
        let waiter = session_owner(&edge.waiter.session);
        owners.insert(owner_key(&waiter)?, waiter);
        owners.insert(owner_key(&edge.owner)?, edge.owner.clone());
    }
    let mut children = BTreeMap::<&str, BTreeSet<&str>>::new();
    for edge in &edges {
        let waiter = stored_owner_key(&owners, &session_owner(&edge.waiter.session))?;
        let owner = stored_owner_key(&owners, &edge.owner)?;
        if owner != waiter {
            children.entry(owner).or_default().insert(waiter);
        }
    }
    let mut sessions = BTreeMap::<&str, Session>::new();
    let mut locks = BTreeMap::<&str, LockWait>::new();
    if frame.context().has_qualified_local_owner() {
        for (key, owner) in &owners {
            let Some(identity) = owner.session() else {
                continue;
            };
            let mut matches = frame.session_rows(&identity);
            let Some(row) = matches.next() else { continue };
            if matches.next().is_some() || !frame.has_session_generation_authority(&row.observation.key.session) {
                continue;
            }
            sessions.insert(key, session(row.observation));
            if let Some(resources) = row.observation.wait.as_ref().and_then(|wait| wait.resources.as_ref())
                && let [resource] = resources.as_slice()
            {
                locks.insert(key, resource.clone());
            }
        }
    }
    let contexts = sessions.iter().map(|(key, session)| (*key, session)).collect();
    let lock_of = locks.iter().map(|(key, lock)| (*key, lock)).collect();
    let Some(mut blocking) = graph(frame.source().target.engine(), &children, &contexts, &lock_of) else {
        return Ok(None);
    };
    for value in &mut blocking.chains {
        decorate(&mut value.tree, &owners);
        let head = value.tree.session.clone();
        *value = chain(frame.source().target.engine(), &head, value.tree.clone());
        if let Some(ActivityWaitOwner::Transaction { owner, .. }) = &value.tree.owner {
            value.headline = if *owner == TransactionWaitOwner::Prepared {
                t!(
                    "core.blocking.headline.prepared",
                    n = value.waiters,
                    count = format::digits(value.waiters.to_string())
                )
            } else {
                t!(
                    "core.blocking.headline.transaction",
                    n = value.waiters,
                    count = format::digits(value.waiters.to_string())
                )
            };
            value.advice = t!("core.blocking.advice.transaction");
        } else if value.tree.context.is_some() {
            value.advice = t!("core.blocking.advice.native_session");
        }
    }
    Ok(Some(blocking))
}

fn owner_key(owner: &ActivityWaitOwner) -> Result<String> {
    serde_json::to_vec(&("blocking-owner-v1", owner))
        .map(|bytes| digest(&bytes))
        .map_err(|_| CollectionStop::InvalidPage.error())
}
fn stored_owner_key<'a>(owners: &'a BTreeMap<String, ActivityWaitOwner>, owner: &ActivityWaitOwner) -> Result<&'a str> {
    owners
        .get_key_value(&owner_key(owner)?)
        .map(|(key, _)| key.as_str())
        .ok_or_else(|| CollectionStop::InvalidPage.error())
}
fn session_owner(identity: &NativeActivityIdentity) -> ActivityWaitOwner {
    ActivityWaitOwner::Session { id: identity.id.clone(), generation: Some(identity.generation.clone()) }
}
fn decorate(node: &mut BlockNode, owners: &BTreeMap<String, ActivityWaitOwner>) {
    if let Some(owner) = owners.get(&node.session) {
        node.session = match owner {
            ActivityWaitOwner::Session { id, .. } => id.clone(),
            ActivityWaitOwner::Transaction { native_id, .. } | ActivityWaitOwner::Unknown { native_id, .. } => {
                native_id.clone()
            }
        };
        node.owner = Some(owner.clone());
    }
    for child in &mut node.children {
        decorate(child, owners);
    }
}
fn number(value: &Option<String>) -> Option<f64> {
    value.as_deref()?.parse().ok()
}
fn session(row: &ActivityObservation) -> Session {
    Session {
        progress_eligible: None,
        sql_context: (row.key.request.is_some() && matches!(&row.statement, DefinitionPayload::Available { .. }))
            .then_some(crate::SessionStatementContext::CurrentRequest),
        activity_key: Some(row.key.clone()),
        id: row.key.session.id.clone(),
        action_target: None,
        user: row.client.as_ref().and_then(|client| client.user.clone()),
        application: row.client.as_ref().and_then(|client| client.application.clone()),
        host: row.client.as_ref().and_then(|client| client.host.clone()),
        database: row.database.clone(),
        state: row.state.clone(),
        command: row.command.clone(),
        wait: row.wait.as_ref().and_then(|wait| wait.native_type.clone()),
        wait_ms: row.wait.as_ref().and_then(|wait| number(&wait.elapsed_ms)),
        wait_resources: None,
        blocked_by: None,
        elapsed_ms: number(&row.elapsed_ms),
        transaction_ms: number(&row.transaction_age_ms),
        cpu_ms: None,
        reads: None,
        sql: match &row.statement {
            DefinitionPayload::Available { text, .. } => Some(text.clone()),
            _ => None,
        },
        waiting_on_lock: row.waiting_on_lock,
    }
}
