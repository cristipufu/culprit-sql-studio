//! Shared host and TServer measurements. Never substitute them for database-specific pressure or RSS.
use crate::{global, sources};
use culprit_core::{Coverage, Error, ErrorCode, MemoryLimit, NodeResource, NodeResources, ProcessMemory, Result};
use culprit_postgres::wire::NoticeRead;
#[cfg(test)]
use culprit_postgres::wire::{BorrowedRead, ObservedSession};
use std::collections::BTreeMap;

const MAX_NODES: usize = 1000;

pub(crate) async fn collect(session: &mut impl NoticeRead, coverage: &mut Coverage) -> Option<NodeResources> {
    let result = crate::deadline(culprit_core::limits::OBSERVE_TIMEOUT, read_from(session, MAX_NODES)).await;
    let data = coverage.read(&sources::NODE_RESOURCES, result)?;
    coverage.completed(&sources::NODE_RESOURCES, data.nodes.len());
    if data.partial {
        coverage
            .partial(&sources::NODE_RESOURCES, culprit_core::message!("provider.yugabyte.coverage.resources_partial"));
    }
    Some(data)
}

async fn clock(session: &mut impl NoticeRead) -> Result<String> {
    session
        .query(&format!("SELECT {} AS at", crate::probe::timestamp("clock_timestamp()")), 2)
        .await?
        .into_iter()
        .next()
        .map(|r| r.string("at"))
        .filter(|s| culprit_core::format::epoch_seconds(s).is_some())
        .ok_or_else(|| Error::invalid(culprit_core::message!("provider.yugabyte.error.invalid_resource_time")))
}

fn number(value: Option<String>, fraction: bool, partial: &mut bool) -> Option<f64> {
    let n = if fraction {
        value
            .and_then(|value| value.parse::<f64>().ok())
            .filter(|value| value.is_finite() && (0.0..=1.0).contains(value))
    } else {
        value.and_then(|value| value.parse::<u64>().ok()).map(|value| value as f64)
    };
    *partial |= n.is_none();
    n
}

fn limit(value: Option<String>, partial: &mut bool) -> Option<MemoryLimit> {
    match value.as_deref() {
        Some("-1") => Some(MemoryLimit::Unlimited),
        Some(value) => match value.parse::<u64>() {
            Ok(value) => Some(MemoryLimit::Bytes(value as f64)),
            Err(_) => {
                *partial = true;
                None
            }
        },
        None => {
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

#[cfg(test)]
pub(crate) async fn read(session: &ObservedSession, maximum: usize) -> Result<NodeResources> {
    read_from(&mut BorrowedRead::new(session), maximum).await
}
pub(crate) struct ResourceReading {
    pub view: NodeResources,
    pub counters: Vec<culprit_core::telemetry::NativeCounterSet>,
}
pub(crate) async fn read_from(session: &mut impl NoticeRead, maximum: usize) -> Result<NodeResources> {
    Ok(read_native(session, maximum).await?.view)
}
pub(crate) async fn read_native(session: &mut impl NoticeRead, maximum: usize) -> Result<ResourceReading> {
    let maximum = maximum.clamp(1, MAX_NODES);
    let notices = session.notices();
    let before = Ok::<_, culprit_core::Error>(global::nodes_from(session).await?);
    let observed_from = clock(session).await?;
    // The native function fans out independently of global-view configuration. Project only known scalars;
    // do not transfer the full JSON object, node addresses or free-form RPC errors.
    let sql=format!("SELECT uuid,status,metrics->>'cpu_usage_user' AS cpu_user,metrics->>'cpu_usage_system' AS cpu_system,
        metrics->>'memory_total' AS total,metrics->>'memory_available' AS available,metrics->>'memory_free' AS free,
        metrics->>'tserver_root_memory_consumption' AS tracked,metrics->>'tserver_root_memory_limit' AS hard_limit,
        metrics->>'tserver_root_memory_soft_limit' AS soft_limit FROM pg_catalog.yb_servers_metrics() ORDER BY uuid LIMIT {}",maximum+1);
    let rows = session.query(&sql, maximum + 1).await.map_err(|error| {
        if error.code == ErrorCode::PermissionDenied {
            error.with_hint(culprit_core::message!("provider.yugabyte.error.resources_privilege_hint"))
        } else {
            error
        }
    })?;
    let truncated = rows.len() > maximum;
    let rows = rows.into_iter().take(maximum);
    let observed_until = clock(session).await?;
    let after = global::nodes_from(session).await;
    let mut partial = truncated || session.notices() != notices;
    partial |=
        culprit_core::format::epoch_seconds(&observed_until) < culprit_core::format::epoch_seconds(&observed_from);
    let mut nodes = BTreeMap::new();
    let mut native = BTreeMap::new();
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
            let name_message = culprit_core::message!("provider.yugabyte.resources.root_tracker");
            let name = name_message.render();
            entry.process_memory =
                Some(ProcessMemory { name_message: Some(name_message), name, tracked_bytes, hard_limit, soft_limit });
        } else {
            partial = true;
        }
        native.insert(node.clone(), native_measurements(Some(&row), &entry)?);
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
    let notes = vec![
        culprit_core::message!("provider.yugabyte.resources.shared"),
        culprit_core::message!("provider.yugabyte.resources.cpu"),
        culprit_core::message!("provider.yugabyte.resources.memory"),
    ];
    let rendered_notes = notes.iter().map(culprit_core::i18n::Message::render).collect();
    let source_message = culprit_core::message!("provider.yugabyte.resources.source");
    let source = source_message.render();
    let view = NodeResources {
        source_message: Some(source_message),
        note_messages: Some(notes),

        source,
        observed_from,
        observed_until,
        nodes_expected,
        partial,
        notes: rendered_notes,
        nodes: nodes.into_values().collect(),
    };
    let counters = view
        .nodes
        .iter()
        .map(|node| native.remove(&node.node).map(Ok).unwrap_or_else(|| native_measurements(None, node)))
        .collect::<Result<Vec<_>>>()?;
    Ok(ResourceReading { view, counters })
}

fn native_measurements(
    row: Option<&culprit_postgres::wire::Row>,
    node: &NodeResource,
) -> Result<culprit_core::telemetry::NativeCounterSet> {
    use culprit_core::{MeasurementScope, telemetry::*};
    let mut counters = vec![];
    let source = "yb_servers_metrics";
    let mut properties =
        BTreeMap::from([("available".into(), node.available.to_string()), ("shared-host".into(), "true".into())]);
    for (name, value) in
        [("cpu-user-fraction", node.cpu_user_fraction), ("cpu-system-fraction", node.cpu_system_fraction)]
    {
        if let Some(value) = value {
            counters.push(NativeCounterSample {
                value: MeasurementValue::Real(value),
                ..NativeCounterSample::gauge(name, 0, CounterUnit::Ratio, MeasurementScope::Node, source, name)
            });
        }
    }
    if let Some(row) = row.filter(|_| node.available) {
        for (name, column, visible) in [
            ("host-memory-total", "total", node.host_memory_total_bytes.is_some()),
            ("host-memory-available", "available", node.host_memory_available_bytes.is_some()),
            ("host-memory-free", "free", node.host_memory_free_bytes.is_some()),
            (
                "tserver-root-tracked-bytes",
                "tracked",
                node.process_memory.as_ref().and_then(|memory| memory.tracked_bytes).is_some(),
            ),
        ] {
            if visible {
                let value =
                    row.text(column).and_then(|value| value.parse::<u64>().ok()).ok_or_else(crate::native::invalid)?;
                counters.push(NativeCounterSample::gauge(
                    name,
                    value,
                    CounterUnit::Bytes,
                    MeasurementScope::Node,
                    source,
                    column,
                ));
            }
        }
        if let Some(memory) = &node.process_memory {
            for (name, column, limit) in [
                ("tserver-root-hard-limit", "hard_limit", memory.hard_limit.as_ref()),
                ("tserver-root-soft-limit", "soft_limit", memory.soft_limit.as_ref()),
            ] {
                match limit {
                    Some(MemoryLimit::Unlimited) => {
                        properties.insert(name.into(), "unlimited".into());
                    }
                    Some(MemoryLimit::Bytes(_)) => {
                        let value = row
                            .text(column)
                            .and_then(|value| value.parse::<u64>().ok())
                            .ok_or_else(crate::native::invalid)?;
                        counters.push(NativeCounterSample::gauge(
                            name,
                            value,
                            CounterUnit::Bytes,
                            MeasurementScope::Node,
                            source,
                            column,
                        ));
                    }
                    None => {}
                }
            }
        }
    }
    Ok(NativeCounterSet {
        identity: NativeIdentity::new(&node.node).qualified("node", &node.node),
        scope: MeasurementScope::Node,
        counters,
        properties,
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
