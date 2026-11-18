//! Only a closed two-capture owner may present a retained closure as bound to its original participant.
use super::*;
use culprit_core::{
    blocking::{BlockNode, BlockingChain},
    telemetry::*,
};
use culprit_engine::{LocalActivityRead, LocalBlockingAnalysis};

const ENTRY_LIMIT: usize = 5;
const NODE_LIMIT: usize = 5;

pub(super) fn captured(analysis: &LocalBlockingAnalysis) -> Result<Value> {
    let report = analysis.report();
    let original = analysis.original().frame()?;
    let followup = analysis.followup().frame()?;
    let original_participant = super::context::referenced_participant(&original, &report.original)?;
    let relationships = report.relationships.iter().take(ENTRY_LIMIT).collect::<Vec<_>>();
    let chains = report
        .blocking
        .iter()
        .flat_map(|blocking| &blocking.chains)
        .take(ENTRY_LIMIT)
        .map(|chain| chain_value(analysis.followup(), &followup, chain))
        .collect::<Result<Vec<_>>>()?;
    let available_chains = report.blocking.as_ref().map_or(0, |blocking| blocking.chains.len());
    Ok(json!({
        "blocking": {
            "title": report.title, "summary": report.summary, "limitations": report.limitations,
            "investigation": report.investigation, "verify": report.verify,
            "historical_context": true, "original_authenticated": true,
            "original_participant_context":original_participant,
            "binding": report.binding, "membership": report.membership,
            "request_end": report.request_end, "closure": report.closure,
            "source_state": report.source_state, "source_reason": report.source_reason,
            "read_complete": report.read_complete, "native_rows": report.native_rows,
            "observed_rows": report.observed_rows,
            "relationships_available": report.relationships.len(),
            "relationships_presented": relationships.len(),
            "relationships_omitted": report.relationships.len().saturating_sub(relationships.len()),
            "relationships": relationships,
            "waiting": report.blocking.as_ref().map(|blocking| blocking.waiting),
            "chains_available": available_chains, "chains_presented": chains.len(),
            "chains_omitted": available_chains.saturating_sub(chains.len()), "chains": chains,
        },
        "original": input(analysis.original())?,
        "followup": input(analysis.followup())?,
        "local_verification_bytes": analysis.bytes_read(),
    }))
}

fn input(read: &LocalActivityRead) -> Result<Value> {
    let source = read.source();
    let native = source.observation.activity().ok_or_else(|| DeliveryProblem::Integrity.error())?;
    let context = NativeReadContext::from_activity(source)?;
    Ok(json!({
        "reference": reference(read, "capture"),
        "native_read_bounds": {"from":native.observed_from,"until":native.observed_until},
        "native_observed_at":native.at, "scope":native.scope,
        "database_writable":native.database_writable,
        "qualified_local_owner":context.has_qualified_local_owner(),
        "population":native.population, "native_rows":native.native_rows,
        "retained_rows":native.observations, "read_complete":true,
        "native_source_state":native.coverage.state, "native_source_records":native.coverage.records,
        "native_source_complete":native.coverage.complete_scope,
        "native_error_code":native.coverage.error_code, "native_stopped":native.coverage.stopped,
        "collected_from":read.collected_from(), "collected_until":read.collected_until(),
        "local_verification_bytes":read.bytes_read(),
    }))
}

fn chain_value(read: &LocalActivityRead, frame: &ActivityFrameEvidence<'_>, chain: &BlockingChain) -> Result<Value> {
    let mut pending = vec![(&chain.tree, None)];
    let mut available = 0usize;
    let mut nodes = Vec::new();
    while let Some((node, parent)) = pending.pop() {
        available += 1;
        if nodes.len() < NODE_LIMIT {
            nodes.push(node_value(read, frame, node, parent)?);
        }
        pending.extend(node.children.iter().rev().map(|child| (child, Some(node.key.as_str()))));
    }
    Ok(json!({
        "reference":reference(read, &chain.tree.key), "head_kind":chain.head_kind,
        "headline":chain.headline, "advice":chain.advice, "waiters":chain.waiters,
        "longest_wait_ms":chain.longest_wait_ms, "head_ms":chain.head_ms,
        "transaction_ms":chain.transaction_ms, "wait_type":chain.wait_type,
        "nodes_available":available, "nodes_presented":nodes.len(),
        "nodes_omitted":available.saturating_sub(nodes.len()), "nodes":nodes,
    }))
}

fn node_value(
    read: &LocalActivityRead,
    frame: &ActivityFrameEvidence<'_>,
    node: &BlockNode,
    parent: Option<&str>,
) -> Result<Value> {
    let participant = node
        .context
        .as_ref()
        .map(|session| {
            let key = session.activity_key.as_ref().ok_or_else(|| DeliveryProblem::Integrity.error())?;
            super::context::graph_participant(frame, key, session.sql_context)
        })
        .transpose()?;
    let statement = participant.as_ref().and_then(|context| context.get("statement"));
    let preview = statement.and_then(|statement| statement.get("preview")).and_then(Value::as_str);
    let truncated =
        statement.and_then(|statement| statement.get("preview_truncated")).and_then(Value::as_bool) == Some(true);
    Ok(json!({
        "reference":reference(read, &node.key), "parent":parent.map(|parent|reference(read, parent)),
        "session":node.session, "owner":node.owner, "lock":node.lock, "waits_for":node.waits_for,
        "context_available":node.context.is_some(),
        "participant_context":participant,
        "state":node.context.as_ref().map(|context|context.state.as_str()),
        "sql_preview":preview,
        "sql_preview_truncated":truncated,
    }))
}

fn reference(read: &LocalActivityRead, key: &str) -> String {
    let capture = read.capture();
    digest(
        format!(
            "local-blocking:{}:{}:{}:{key}",
            capture.manifest_sha256, capture.source_scope_id, capture.capture.observation_id
        )
        .as_bytes(),
    )
}
