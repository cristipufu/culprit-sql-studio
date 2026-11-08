//! Shared host and TServer measurements. Never substitute them for database-specific pressure or RSS.
use crate::{global, sources};
use culprit_core::{Coverage, Error, ErrorCode, MemoryLimit, NodeResource, NodeResources, ProcessMemory, Result, t};
use culprit_postgres::wire::{ObservedSession, query_bounded, query_one};
use std::collections::BTreeMap;

const MAX_NODES: usize = 1000;

pub(crate) async fn collect(session: &ObservedSession, coverage: &mut Coverage) -> Option<NodeResources> {
    let result = crate::deadline(culprit_core::limits::OBSERVE_TIMEOUT, read(session, MAX_NODES)).await;
    let data = coverage.read(&sources::NODE_RESOURCES, result)?;
    coverage.completed(&sources::NODE_RESOURCES, data.nodes.len());
    if data.partial {
        coverage.partial(&sources::NODE_RESOURCES, t!("provider.yugabyte.coverage.resources_partial"));
    }
    Some(data)
}

async fn clock(session: &ObservedSession) -> Result<String> {
    query_one(&session.client, &format!("SELECT {} AS at", crate::probe::timestamp("clock_timestamp()")))
        .await?
        .map(|r| r.string("at"))
        .filter(|s| culprit_core::format::epoch_seconds(s).is_some())
        .ok_or_else(|| Error::invalid(t!("provider.yugabyte.error.invalid_resource_time")))
}

fn number(value: Option<String>, fraction: bool, partial: &mut bool) -> Option<f64> {
    let n = value
        .and_then(|s| s.parse::<f64>().ok())
        .filter(|n| n.is_finite() && *n >= 0.0 && if fraction { *n <= 1.0 } else { n.fract() == 0.0 });
    *partial |= n.is_none();
    n
}

fn limit(value: Option<String>, partial: &mut bool) -> Option<MemoryLimit> {
    let n = value.and_then(|s| s.parse::<f64>().ok());
    match n {
        Some(-1.0) => Some(MemoryLimit::Unlimited),
        Some(n) if n.is_finite() && n >= 0.0 && n.fract() == 0.0 => Some(MemoryLimit::Bytes(n)),
        _ => {
            *partial = true;
            None
        }
    }
}

fn unavailable(node: String) -> NodeResource {
    NodeResource {
        node,
        available: false,
        cpu_user_fraction: None,
        cpu_system_fraction: None,
        host_memory_total_bytes: None,
        host_memory_available_bytes: None,
        host_memory_free_bytes: None,
        process_memory: None,
    }
}

pub(crate) async fn read(session: &ObservedSession, maximum: usize) -> Result<NodeResources> {
    let maximum = maximum.clamp(1, MAX_NODES);
    let notices = session.notices();
    let before = global::nodes(session).await;
    let observed_from = clock(session).await?;
    // The native function fans out independently of global-view configuration. Project only known scalars;
    // do not transfer the full JSON object, node addresses or free-form RPC errors.
    let sql=format!("SELECT uuid,status,metrics->>'cpu_usage_user' AS cpu_user,metrics->>'cpu_usage_system' AS cpu_system,
        metrics->>'memory_total' AS total,metrics->>'memory_available' AS available,metrics->>'memory_free' AS free,
        metrics->>'tserver_root_memory_consumption' AS tracked,metrics->>'tserver_root_memory_limit' AS hard_limit,
        metrics->>'tserver_root_memory_soft_limit' AS soft_limit FROM pg_catalog.yb_servers_metrics() ORDER BY uuid LIMIT {}",maximum+1);
    let (rows, truncated) = query_bounded(&session.client, &sql, maximum).await.map_err(|error| {
        if error.code == ErrorCode::PermissionDenied {
            error.with_hint(t!("provider.yugabyte.error.resources_privilege_hint"))
        } else {
            error
        }
    })?;
    let observed_until = clock(session).await?;
    let after = global::nodes(session).await;
    let mut partial = truncated || session.notices() != notices;
    partial |=
        culprit_core::format::epoch_seconds(&observed_until) < culprit_core::format::epoch_seconds(&observed_from);
    let mut nodes = BTreeMap::new();
    for row in rows {
        let Some(node) = row.text("uuid").and_then(|s| crate::probe::node_id(&s)) else {
            partial = true;
            continue;
        };
        let mut entry = unavailable(node.clone());
        if row.string("status") == "OK" {
            entry.cpu_user_fraction = number(row.text("cpu_user"), true, &mut partial);
            entry.cpu_system_fraction = number(row.text("cpu_system"), true, &mut partial);
            if entry.cpu_user_fraction.zip(entry.cpu_system_fraction).is_some_and(|(u, s)| u + s > 1.000001) {
                partial = true;
                entry.cpu_user_fraction = None;
                entry.cpu_system_fraction = None;
            }
            entry.host_memory_total_bytes = number(row.text("total"), false, &mut partial).filter(|n| *n > 0.0);
            partial |= entry.host_memory_total_bytes.is_none();
            entry.host_memory_available_bytes = number(row.text("available"), false, &mut partial);
            entry.host_memory_free_bytes = number(row.text("free"), false, &mut partial);
            for value in [&mut entry.host_memory_available_bytes, &mut entry.host_memory_free_bytes] {
                if value.zip(entry.host_memory_total_bytes).is_some_and(|(v, t)| v > t) {
                    partial = true;
                    *value = None;
                }
            }
            let tracked_bytes = number(row.text("tracked"), false, &mut partial);
            let hard_limit = limit(row.text("hard_limit"), &mut partial);
            let soft_limit = limit(row.text("soft_limit"), &mut partial);
            entry.available = entry.cpu_user_fraction.is_some()
                || entry.cpu_system_fraction.is_some()
                || entry.host_memory_total_bytes.is_some()
                || tracked_bytes.is_some();
            entry.process_memory = Some(ProcessMemory {
                name: t!("provider.yugabyte.resources.root_tracker"),
                tracked_bytes,
                hard_limit,
                soft_limit,
            });
        } else {
            partial = true;
        }
        if nodes.insert(node, entry).is_some() {
            partial = true;
        }
    }
    let nodes_expected = match before.ok().zip(after.ok()) {
        Some((a, b)) if a == b => {
            partial |= a.len() != nodes.len() || nodes.keys().any(|n| !a.contains(n));
            for node in &a {
                if nodes.len() >= maximum {
                    break;
                }
                nodes.entry(node.clone()).or_insert_with(|| unavailable(node.clone()));
            }
            Some(a.len())
        }
        _ => {
            partial = true;
            None
        }
    };
    Ok(NodeResources {
        source: t!("provider.yugabyte.resources.source"),
        observed_from,
        observed_until,
        nodes_expected,
        partial,
        notes: vec![
            t!("provider.yugabyte.resources.shared"),
            t!("provider.yugabyte.resources.cpu"),
            t!("provider.yugabyte.resources.memory"),
        ],
        nodes: nodes.into_values().collect(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn invalid_resource_values_stay_unknown_and_unlimited_is_not_zero_or_missing() {
        for value in [None, Some("NaN"), Some("inf"), Some("-1"), Some("1.1")] {
            let mut partial = false;
            assert!(number(value.map(str::to_string), true, &mut partial).is_none());
            assert!(partial);
        }
        let mut partial = false;
        assert_eq!(number(Some("0".into()), true, &mut partial), Some(0.0));
        assert!(!partial);
        assert_eq!(limit(Some("-1".into()), &mut partial), Some(MemoryLimit::Unlimited));
        assert_eq!(limit(Some("0".into()), &mut partial), Some(MemoryLimit::Bytes(0.0)));
        assert_eq!(limit(None, &mut partial), None);
        assert!(partial);
    }
}
