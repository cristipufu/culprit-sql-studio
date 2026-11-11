//! Unexpected backend terminations are an evictable record set, not an error rate or a complete SQL error log.
use crate::{global, probe::Probe, sources};
use culprit_core::{
    CounterScope, Coverage, Error, QueryTermination, Result, SnapshotOptions, TerminationHistory, TerminationReason,
};
use culprit_postgres::wire::NoticeRead;
#[cfg(test)]
use culprit_postgres::wire::{BorrowedRead, ObservedSession};
use std::collections::BTreeSet;

const MAX_RECORDS: usize = 1000;

pub(crate) async fn collect(
    session: &mut impl NoticeRead,
    probe: &Probe,
    options: &SnapshotOptions,
    coverage: &mut Coverage,
) -> Option<TerminationHistory> {
    let result =
        crate::deadline(culprit_core::limits::OBSERVE_TIMEOUT, read_from(session, probe, options, MAX_RECORDS)).await;
    let history = coverage.read(&sources::TERMINATIONS, result)?;
    coverage.completed(&sources::TERMINATIONS, history.events.len());
    if history.partial {
        coverage
            .partial(&sources::TERMINATIONS, culprit_core::message!("provider.yugabyte.coverage.terminations_partial"));
    }
    Some(history)
}

#[cfg(test)]
pub(crate) async fn read(
    session: &ObservedSession,
    probe: &Probe,
    options: &SnapshotOptions,
    limit: usize,
) -> Result<TerminationHistory> {
    read_from(&mut BorrowedRead::new(session), probe, options, limit).await
}
pub(crate) async fn read_from(
    session: &mut impl NoticeRead,
    probe: &Probe,
    options: &SnapshotOptions,
    limit: usize,
) -> Result<TerminationHistory> {
    let end = culprit_core::format::epoch_seconds(&probe.now)
        .ok_or_else(|| Error::invalid(culprit_core::message!("provider.yugabyte.error.invalid_termination_time")))?;
    let minutes = options.window_in_minutes().min(culprit_core::MAX_WINDOW_MINUTES);
    let start = end - f64::from(minutes) * 60.0;
    let before = session.notices();
    // Global views cannot filter an owner id for these records. Preserve native local visibility for restricted
    // logins instead of relying on a remote view's privilege mapping to establish a role boundary.
    let cluster = probe.global && probe.statement_role.is_none();
    let inventory = if cluster { Some(Ok::<_, culprit_core::Error>(global::nodes_from(session).await?)) } else { None };
    let relation = if cluster { "pg_catalog.\"gv$yb_terminated_queries\"" } else { "pg_catalog.yb_terminated_queries" };
    let node = if cluster {
        "server_uuid::text".into()
    } else {
        format!("'{}'::text", probe.node.as_deref().unwrap_or("unknown"))
    };
    let privilege = format!("{}{}", if probe.statement_role.is_some() { "NOT " } else { "" }, crate::probe::ALL_STATS);
    let limit = limit.clamp(1, MAX_RECORDS);
    // Never fetch query_text, backend PIDs or arbitrary native messages. Constant filters also bound FDW traffic.
    // The native function enforces visibility for the invoking login, including its inherited role privileges.
    let sql = format!(
        "SELECT query_id::text AS query,{node} AS node,{} AS started,{} AS ended,
        CASE WHEN termination_reason='Terminated by SIGKILL' THEN 'sigkill'
        WHEN termination_reason='Terminated by SIGSEGV' THEN 'sigsegv'
        WHEN termination_reason LIKE 'temporary file size exceeds temp_file_limit%' THEN 'temp-file-limit'
        ELSE 'other' END AS reason FROM {relation}
        WHERE databasename='{}' AND query_end_time>=to_timestamp({start}) AND query_end_time<to_timestamp({end})
        AND {privilege} ORDER BY query_end_time DESC,query_start_time DESC,query_id LIMIT {}",
        crate::probe::timestamp("query_start_time"),
        crate::probe::timestamp("query_end_time"),
        probe.database.replace('\'', "''"),
        limit + 1
    );
    let rows = session.query(&sql, limit + 1).await?;
    let truncated = rows.len() > limit;
    let rows = rows.into_iter().take(limit);
    let mut partial = truncated;
    let mut seen = BTreeSet::new();
    let mut events = Vec::with_capacity(rows.len());
    for row in rows {
        let Some((ended_at, ended)) = row.text("ended").and_then(|at| {
            let t = culprit_core::format::epoch_seconds(&at)?;
            (t >= start && t < end).then_some((at, t))
        }) else {
            partial = true;
            continue;
        };
        let started_at =
            row.text("started").filter(|at| culprit_core::format::epoch_seconds(at).is_some_and(|t| t <= ended));
        partial |= started_at.is_none();
        let node = row.text("node").and_then(|n| crate::probe::node_id(&n));
        if let Some(node) = &node {
            seen.insert(node.clone());
        } else {
            partial = true;
        }
        let query_id = row
            .text("query")
            .and_then(|id| id.parse::<i64>().ok())
            .filter(|id| !(0..=12).contains(id))
            .map(|id| id.to_string());
        let reason = match row.string("reason").as_str() {
            "sigkill" => TerminationReason::Sigkill,
            "sigsegv" => TerminationReason::Sigsegv,
            "temp-file-limit" => TerminationReason::TempFileLimit,
            _ => TerminationReason::Other,
        };
        events.push(QueryTermination { query_id, node, started_at, ended_at, reason });
    }
    let nodes = if let Some(inventory) = inventory {
        match inventory.ok().zip(global::nodes_from(session).await.ok()) {
            Some((a, b)) if a == b && seen.is_subset(&a) => a,
            _ => {
                partial = true;
                seen
            }
        }
    } else {
        partial |= probe.node.is_none();
        probe.node.iter().cloned().collect()
    };
    let window = session
        .query(
            &format!(
                "SELECT {} AS start,{} AS end,{} AS allowed",
                crate::probe::timestamp(&format!("to_timestamp({start})")),
                crate::probe::timestamp(&format!("to_timestamp({end})")),
                privilege
            ),
            2,
        )
        .await?
        .into_iter()
        .next()
        .ok_or_else(|| Error::invalid(culprit_core::message!("provider.yugabyte.error.no_termination_bounds")))?;
    if !window.flag("allowed") {
        return Err(Error::new(
            culprit_core::ErrorCode::PermissionDenied,
            culprit_core::message!("provider.yugabyte.error.privilege_changed_terminations"),
        ));
    }
    partial |= session.notices() != before;
    let mut notes = vec![
        culprit_core::message!("provider.yugabyte.terminations.scope"),
        culprit_core::message!("provider.yugabyte.terminations.dates"),
        culprit_core::message!("provider.yugabyte.terminations.sigkill"),
    ];
    if probe.global && !cluster {
        notes.push(culprit_core::message!("provider.yugabyte.terminations.local"));
    }
    let source_message = culprit_core::message!(
        "model.observation.source.ysql",
        relation = "yb_terminated_queries",
        scope = if cluster {
            "cluster"
        } else if probe.node.is_some() {
            "node"
        } else {
            "unknown"
        },
        node = probe.node.as_deref().unwrap_or_default(),
        limited = "no",
        role = ""
    );
    let source = source_message.render();
    let rendered_notes = notes.iter().map(culprit_core::i18n::Message::render).collect();
    let visibility_message = match probe.statement_role {
        None => culprit_core::message!("provider.yugabyte.terminations.complete_visibility"),
        Some(role) => culprit_core::message!("provider.yugabyte.terminations.role_visibility", role = role.to_string()),
    };
    let visibility = visibility_message.render();
    Ok(TerminationHistory {
        source_message: Some(source_message),
        visibility_message: Some(visibility_message),
        note_messages: Some(notes),

        source,
        requested_start: window.string("start"),
        requested_end: window.string("end"),
        scope: if cluster { CounterScope::Cluster } else { CounterScope::Node },
        nodes: nodes.into_iter().collect(),
        visibility,
        partial,
        notes: rendered_notes,
        events,
    })
}
