//! Native task samples have weights, not elapsed durations. Never mix these with PGSS or exclusive wait timings.
use crate::{global, probe::Probe, sources};
use culprit_core::{
    ActivityEvent, ActivityHistory, ActivityPeriod, CounterScope, Coverage, Error, Result, SnapshotOptions,
};
use culprit_postgres::wire::NoticeRead;
#[cfg(test)]
use culprit_postgres::wire::{BorrowedRead, ObservedSession};
use std::collections::{BTreeMap, BTreeSet};

const MAX_SAMPLES: usize = 10_000;

pub(crate) async fn collect(
    session: &mut impl NoticeRead,
    probe: &Probe,
    options: &SnapshotOptions,
    coverage: &mut Coverage,
) -> Option<ActivityHistory> {
    let result =
        crate::deadline(culprit_core::limits::OBSERVE_TIMEOUT, read_from(session, probe, options, MAX_SAMPLES)).await;
    let history = coverage.read(&sources::ACTIVITY_HISTORY, result)?;
    coverage.completed(&sources::ACTIVITY_HISTORY, history.period.observations as usize);
    if history.period.partial {
        coverage
            .partial(&sources::ACTIVITY_HISTORY, culprit_core::message!("provider.yugabyte.coverage.activity_partial"));
    }
    Some(history)
}

fn relation(probe: &Probe, start: f64, end: f64) -> String {
    if probe.global {
        return "pg_catalog.\"gv$yb_active_session_history\"".into();
    }
    // OUT parameters supply the native row type; explicit time arguments avoid the view's unbounded default call.
    format!("pg_catalog.yb_active_session_history(to_timestamp({start}),to_timestamp({end}))")
}

#[cfg(test)]
pub(crate) async fn read(
    session: &ObservedSession,
    probe: &Probe,
    options: &SnapshotOptions,
    limit: usize,
) -> Result<ActivityHistory> {
    read_from(&mut BorrowedRead::new(session), probe, options, limit).await
}
pub(crate) async fn read_from(
    session: &mut impl NoticeRead,
    probe: &Probe,
    options: &SnapshotOptions,
    limit: usize,
) -> Result<ActivityHistory> {
    let end = culprit_core::format::epoch_seconds(&probe.now)
        .ok_or_else(|| Error::invalid(culprit_core::message!("provider.yugabyte.error.invalid_ash_time")))?;
    let minutes = options.window_in_minutes().min(culprit_core::MAX_WINDOW_MINUTES);
    let start = end - f64::from(minutes) * 60.0;
    let before = session.notices();
    let inventory =
        if probe.global { Some(Ok::<_, culprit_core::Error>(global::nodes_from(session).await?)) } else { None };
    let relation = relation(probe, start, end);
    let role = probe
        .statement_role
        .map(|id| format!(" AND ysql_userid={id}"))
        .unwrap_or_else(|| format!(" AND {}", crate::probe::ALL_STATS));
    let node = if probe.global {
        "server_uuid::text".into()
    } else {
        format!("'{}'::text", probe.node.as_deref().unwrap_or("unknown"))
    };
    let limit = limit.clamp(1, MAX_SAMPLES);
    // Constant bounds are important: global views otherwise ship each node's entire circular buffer before
    // evaluating now(). Project no client addresses, request identifiers, process memory or wait-event aux text.
    let sql = format!(
        "SELECT {} AS at,query_id::text AS query,wait_event_component AS component,
        wait_event_type AS kind,wait_event AS event,sample_weight::float8 AS weight,{node} AS node
        FROM {relation} WHERE ysql_dbid={} AND sample_time >= to_timestamp({start})
        AND sample_time < to_timestamp({end}){role}
        ORDER BY sample_time DESC,query_id,wait_event_component,wait_event LIMIT {}",
        crate::probe::timestamp("sample_time"),
        probe.oid,
        limit + 1
    );
    let rows = session.query(&sql, limit + 1).await?;
    let truncated = rows.len() > limit;
    let rows = rows.into_iter().take(limit);
    let mut partial = truncated;
    let mut seen = BTreeSet::new();
    let mut events = BTreeMap::<(String, String, String, Option<String>, Option<String>), ActivityEvent>::new();
    let mut first: Option<String> = None;
    let mut last: Option<String> = None;
    let mut observations = 0.0;
    let mut weighted = 0.0;
    for row in rows {
        let (Some(at), Some(component), Some(event), Some(weight)) = (
            row.text("at"),
            row.text("component"),
            row.text("event"),
            row.number("weight").filter(|w| w.is_finite() && *w > 0.0),
        ) else {
            partial = true;
            continue;
        };
        if culprit_core::format::epoch_seconds(&at).is_none_or(|t| t < start || t >= end) {
            partial = true;
            continue;
        }
        if !(weighted + weight).is_finite() {
            partial = true;
            continue;
        }
        let node = row.text("node").and_then(|n| crate::probe::node_id(&n));
        if let Some(node) = &node {
            seen.insert(node.clone());
        } else {
            partial = true;
        }
        let query = match row.text("query").and_then(|id| id.parse::<i64>().ok()) {
            Some(id) if !(0..=12).contains(&id) => Some(id.to_string()),
            Some(_) => None, // Native reserved ids describe background work or an unresolved query, not PGSS ids.
            None => {
                partial = true;
                None
            }
        };
        first = Some(first.map_or_else(|| at.clone(), |a| a.min(at.clone())));
        last = Some(last.map_or_else(|| at.clone(), |a| a.max(at.clone())));
        let category = culprit_core::waits::category_key(crate::waits::category(&row.string("kind"), &event));
        let key = (category.to_string(), component.clone(), event.clone(), query.clone(), node.clone());
        let entry = events.entry(key).or_insert(ActivityEvent {
            category: category.into(),
            component,
            wait_event: event,
            query_id: query,
            node,
            observations: 0.0,
            weighted_observations: 0.0,
        });
        entry.observations += 1.0;
        entry.weighted_observations += weight;
        observations += 1.0;
        weighted += weight;
    }
    let nodes = if let Some(inventory) = inventory {
        let after = global::nodes_from(session).await;
        match inventory.ok().zip(after.ok()) {
            Some((a, b)) if a == b && seen.is_subset(&a) => a,
            _ => {
                partial = true;
                seen
            }
        }
    } else {
        if probe.node.is_none() {
            partial = true;
        }
        probe.node.iter().cloned().collect()
    };
    partial |= session.notices() != before;
    let window = session
        .query(
            &format!(
                "SELECT {} AS start,{} AS end,{} AS all_stats",
                crate::probe::timestamp(&format!("to_timestamp({start})")),
                crate::probe::timestamp(&format!("to_timestamp({end})")),
                crate::probe::ALL_STATS
            ),
            2,
        )
        .await?
        .into_iter()
        .next()
        .ok_or_else(|| Error::invalid(culprit_core::message!("provider.yugabyte.error.no_ash_bounds")))?;
    if probe.statement_role.is_none() && !window.flag("all_stats") {
        return Err(Error::new(
            culprit_core::ErrorCode::PermissionDenied,
            culprit_core::message!("provider.yugabyte.error.privilege_changed_activity"),
        ));
    }
    let mut notes = vec![
        culprit_core::message!("provider.yugabyte.activity.weights"),
        culprit_core::message!("provider.yugabyte.activity.buffer"),
        culprit_core::message!("provider.yugabyte.activity.scope"),
    ];
    if partial {
        notes.push(culprit_core::message!("provider.yugabyte.activity.partial"));
    }
    let source_message = culprit_core::message!(
        "model.observation.source.ysql",
        relation = "yb_active_session_history",
        scope = if probe.global {
            "cluster"
        } else if probe.node.is_some() {
            "node"
        } else {
            "unknown"
        },
        node = probe.node.as_deref().unwrap_or_default(),
        limited = if probe.statement_role.is_some() { "yes" } else { "no" },
        role = probe.statement_role.map(|id| id.to_string()).unwrap_or_default()
    );
    let source = source_message.render();
    let rendered_notes = notes.iter().map(culprit_core::i18n::Message::render).collect();
    Ok(ActivityHistory {
        period: ActivityPeriod {
            source_message: Some(source_message),
            note_messages: Some(notes),

            source,
            requested_start: window.string("start"),
            requested_end: window.string("end"),
            first_sample: first,
            last_sample: last,
            scope: if probe.global { CounterScope::Cluster } else { CounterScope::Node },
            nodes: nodes.into_iter().collect(),
            observations,
            weighted_observations: weighted,
            partial,
            notes: rendered_notes,
        },
        events: events.into_values().collect(),
    })
}
