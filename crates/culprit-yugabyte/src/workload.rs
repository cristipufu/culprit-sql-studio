use crate::{
    probe::{Probe, node_id},
    sources,
};
use culprit_core::{
    CounterScope, CounterSet, Coverage, QueryStat, Spread, Workload, WorkloadKind,
    signals::{DocdbMetrics, QueryCosts},
    t,
};
use culprit_postgres::wire::{ObservedSession, Row, query, query_bounded};
use serde_json::json;
use std::collections::BTreeMap;

async fn epochs(session: &ObservedSession, probe: &Probe, coverage: &mut Coverage) -> CounterSet {
    let mut result = CounterSet {
        scope: if probe.cluster { CounterScope::Cluster } else { CounterScope::Node },
        members: BTreeMap::new(),
        complete: false,
    };
    let relation = if probe.cluster {
        let found = crate::global::configured(&session.client, "pg_stat_statements_info").await.ok().flatten();
        match found {
            Some(relation) => relation,
            None => {
                coverage.partial(&sources::EPOCHS, t!("provider.yugabyte.coverage.epochs_not_configured"));
                return result;
            }
        }
    } else {
        culprit_core::Engine::Yugabyte
            .dialect()
            .qualified(&[probe.statement_schema.as_deref().unwrap_or("pg_catalog"), "pg_stat_statements_info"])
    };
    let before = session.notices();
    let sql = format!(
        "SELECT {} AS reset{} FROM {relation}",
        crate::probe::timestamp("stats_reset"),
        if probe.cluster { ",server_uuid::text AS node" } else { "" }
    );
    if let Some(rows) = coverage.read_rows(&sources::EPOCHS, query(&session.client, &sql).await) {
        result.complete = session.notices() == before;
        for row in rows {
            if let Some(node) = if probe.cluster { node_id(&row.string("node")) } else { probe.node.clone() } {
                result.members.insert(node, row.text("reset"));
            } else {
                result.complete = false;
            }
        }
        if probe.cluster {
            match query(&session.client, "SELECT uuid::text AS node FROM yb_servers()").await {
                Ok(rows) => {
                    let expected: Option<std::collections::BTreeSet<_>> =
                        rows.iter().map(|r| node_id(&r.string("node"))).collect();
                    if expected
                        .as_ref()
                        .is_none_or(|ids| ids.is_empty() || ids != &result.members.keys().cloned().collect())
                    {
                        result.complete = false;
                    }
                }
                Err(_) => result.complete = false,
            }
        }
    }
    if !result.epochs_known() {
        coverage.partial(&sources::EPOCHS, t!("provider.yugabyte.coverage.epochs_incomplete"));
    }
    result
}
async fn statement_rows(
    session: &ObservedSession,
    probe: &Probe,
    relation: &str,
    top: u32,
) -> culprit_core::Result<(Vec<Row>, bool)> {
    let columns: std::collections::BTreeSet<_> = query(
        &session.client,
        &format!(
            "SELECT attname FROM pg_attribute WHERE attrelid='{}'::regclass AND attnum>0 AND NOT attisdropped",
            relation.replace('\'', "''")
        ),
    )
    .await?
    .into_iter()
    .map(|row| row.string("attname"))
    .collect();
    let time_column = |modern: &str, legacy: &str| {
        if columns.contains(modern) {
            Ok(modern.to_string())
        } else if columns.contains(legacy) {
            Ok(legacy.to_string())
        } else {
            Err(culprit_core::Error::invalid(t!("provider.yugabyte.error.no_time_columns")))
        }
    };
    let total = time_column("total_exec_time", "total_time")?;
    let min = time_column("min_exec_time", "min_time")?;
    let max = time_column("max_exec_time", "max_time")?;
    let mean = time_column("mean_exec_time", "mean_time")?;
    let stddev = time_column("stddev_exec_time", "stddev_time")?;
    let role_filter = probe.statement_role.map(|role| format!(" AND userid={role}")).unwrap_or_default();
    let extras = [
        ("docdb_read_rpcs", "read_requests"),
        ("docdb_write_rpcs", "write_requests"),
        ("docdb_read_operations", "read_operations"),
        ("docdb_write_operations", "write_operations"),
        ("docdb_rows_scanned", "rows_scanned"),
        ("docdb_rows_returned", "rows_returned"),
        ("docdb_read_time", "read_ms"),
        ("docdb_write_time", "write_ms"),
        ("docdb_wait_time", "wait_ms"),
        ("catalog_wait_time", "catalog_ms"),
        ("total_retries", "retries"),
        ("conflict_retries", "conflict_retries"),
        ("read_restart_retries", "read_restarts"),
        ("docdb_obsolete_rows_scanned", "obsolete_rows_scanned"),
        ("docdb_seeks", "seeks"),
        ("docdb_nexts", "nexts"),
        ("docdb_prevs", "prevs"),
    ]
    .iter()
    .map(|(key, alias)| {
        if columns.contains(*key) {
            format!("CASE WHEN count({key})=count(*) THEN sum({key}::float8) END AS {alias}")
        } else {
            format!("NULL::float8 AS {alias}")
        }
    })
    .collect::<Vec<_>>()
    .join(",");
    // Project only the fields used here. A whole-row JSON conversion also fetches latency histograms and every
    // unrelated column across the global FDW, before aggregating even a small top-query request.
    let sql = format!(
        "WITH grouped AS (
         SELECT queryid::text AS id,min(query) AS text,sum(calls::float8) AS calls,
         sum({total}::float8) AS ms,sum(rows::float8) AS rows,
         min({min}::float8) AS min_ms,max({max}::float8) AS max_ms,
         sum(calls::float8*(power({stddev}::float8,2)+power({mean}::float8,2))) AS moment,
         {extras} FROM {relation} WHERE dbid={} AND calls>0{role_filter}
         AND query NOT LIKE '%/* culprit */%' GROUP BY queryid)
         SELECT *,sum(ms) OVER() AS all_ms,sum(calls) OVER() AS all_calls,count(*) OVER() AS all_queries
         FROM grouped ORDER BY ms DESC,id LIMIT {}",
        probe.oid,
        top.clamp(1, 1000)
    );
    query_bounded(&session.client, &sql, top.clamp(1, 1000) as usize).await
}

pub(crate) async fn read(
    session: &ObservedSession,
    probe: &Probe,
    top: u32,
    coverage: &mut Coverage,
) -> Option<Workload> {
    if !probe.statements {
        coverage.recorded(
            &sources::WORKLOAD,
            culprit_core::SourceState::Unavailable,
            Some(t!("provider.yugabyte.coverage.statements_unavailable")),
        );
        coverage.lacking(
            culprit_core::Capability::PgStatStatements,
            t!("provider.yugabyte.capability.statements.unavailable"),
            Some(t!("provider.yugabyte.capability.statements.enable")),
        );
        return None;
    }
    let mut counter_set = epochs(session, probe, coverage).await;
    let relation = if probe.cluster {
        "pg_catalog.\"gv$pg_stat_statements\"".into()
    } else {
        culprit_core::Engine::Yugabyte
            .dialect()
            .qualified(&[probe.statement_schema.as_deref().unwrap_or("pg_catalog"), "pg_stat_statements"])
    };
    let before = session.notices();
    let result = statement_rows(session, probe, &relation, top).await;
    let truncated = result.as_ref().is_ok_and(|(_, truncated)| *truncated);
    let rows = coverage.read_rows(&sources::WORKLOAD, result.map(|(rows, _)| rows))?;
    if truncated {
        coverage.partial(&sources::WORKLOAD, t!("provider.yugabyte.coverage.statements_budget"));
        if rows.is_empty() {
            coverage.lacking(
                culprit_core::Capability::PgStatStatements,
                t!("provider.yugabyte.capability.statements.no_row"),
                None,
            );
            return None;
        }
    }
    if rows.iter().any(|row| row.text("id").is_none()) {
        coverage.recorded(
            &sources::WORKLOAD,
            culprit_core::SourceState::Unavailable,
            Some(t!("provider.yugabyte.coverage.visibility_changed")),
        );
        coverage.lacking(
            culprit_core::Capability::PgStatStatements,
            t!("provider.yugabyte.capability.statements.visibility_changed"),
            Some(t!("provider.yugabyte.capability.statements.grant")),
        );
        return None;
    }
    let partial = session.notices() != before;
    let after = epochs(session, probe, coverage).await;
    if counter_set != after || partial {
        counter_set.complete = false;
        coverage.partial(&sources::WORKLOAD, t!("provider.yugabyte.coverage.contributors_changed"));
    }
    if !probe.cluster {
        let scope = match probe.node.as_deref() {
            Some(node) => t!("provider.yugabyte.coverage.node_scope", node = node),
            None => t!("provider.yugabyte.coverage.unknown_node_scope"),
        };
        coverage.note("YSQL scope", scope);
    }
    if let Some(role) = probe.statement_role {
        coverage.note("YSQL role scope", t!("provider.yugabyte.coverage.role_scope", role = role.to_string()));
    }
    let (metrics, rpc_metrics) = metric_settings(session, probe, coverage).await;
    let mut queries = Vec::new();
    for r in &rows {
        let Some(id) = r.text("id") else {
            coverage.partial(&sources::WORKLOAD, t!("provider.yugabyte.coverage.identities_hidden"));
            continue;
        };
        let (Some(calls), Some(ms)) = (r.number("calls").filter(|n| *n > 0.0), r.number("ms")) else { continue };
        let mean = ms / calls;
        let metric = |key: &str| if metrics { r.number(key).filter(|n| *n >= 0.0) } else { None };
        let costs = QueryCosts {
            docdb: Some(DocdbMetrics {
                read_requests: if rpc_metrics { r.number("read_requests").filter(|n| *n >= 0.0) } else { None },
                write_requests: if rpc_metrics { r.number("write_requests").filter(|n| *n >= 0.0) } else { None },
                read_operations: metric("read_operations"),
                write_operations: metric("write_operations"),
                rows_scanned: metric("rows_scanned"),
                rows_returned: metric("rows_returned"),
                read_ms: metric("read_ms"),
                write_ms: metric("write_ms"),
                wait_ms: metric("wait_ms"),
                catalog_ms: metric("catalog_ms"),
                retries: metric("retries"),
                conflict_retries: metric("conflict_retries"),
                read_restarts: metric("read_restarts"),
                obsolete_rows_scanned: metric("obsolete_rows_scanned"),
                seeks: metric("seeks"),
                nexts: metric("nexts"),
                prevs: metric("prevs"),
            }),
            ..Default::default()
        };
        let value = json!({"id":id,"text":r.string("text"),"executions":calls,"total_time_ms":ms,"mean_time_ms":mean,"max_time_ms":r.number("max_ms"),
            "stddev_time_ms":r.number("moment").map(|m|(m/calls-mean*mean).max(0.0).sqrt()),"rows":r.number("rows"),"plans":[],"waits":[],
            "costs":costs,"spread":Spread{min_ms:r.number("min_ms"),..Default::default()}});
        match serde_json::from_value::<QueryStat>(value) {
            Ok(q) => queries.push(q),
            Err(_) => {
                coverage.partial(&sources::WORKLOAD, t!("provider.yugabyte.coverage.row_unreadable"));
            }
        }
    }
    let start = if counter_set.epochs_known() {
        let mut at = counter_set.members.values().flatten();
        let first = at.next().cloned();
        first.filter(|first| at.all(|s| s == first))
    } else {
        None
    };
    let hours = start
        .as_deref()
        .and_then(culprit_core::format::epoch_seconds)
        .zip(culprit_core::format::epoch_seconds(&probe.now))
        .map(|(a, b)| ((b - a) / 3600.0).max(0.0))
        .unwrap_or(0.0);
    let head = rows.first();
    coverage.delivered(culprit_core::Capability::PgStatStatements);
    Some(Workload {
        source: format!(
            "{}{}",
            if probe.cluster {
                "YSQL pg_stat_statements (cluster)".into()
            } else {
                format!("YSQL pg_stat_statements (node {})", probe.node.as_deref().unwrap_or("unknown"))
            },
            probe.statement_role.map(|role| format!("; role {role} only")).unwrap_or_default()
        ),
        kind: Some(WorkloadKind::StatementStats),
        windowed: false,
        log: None,
        window_start: start,
        window_end: Some(probe.now.clone()),
        window_hours: hours,
        total_time_ms: head.and_then(|r| r.number("all_ms")).unwrap_or(0.0),
        total_cpu_ms: None,
        total_executions: head.and_then(|r| r.number("all_calls")).unwrap_or(0.0),
        query_count: head.and_then(|r| r.number("all_queries")).unwrap_or(0.0),
        queries,
        timeline: None,
        untracked_time_ms: None,
        hourly_time_ms: None,
        min_logged_ms: None,
        history_inputs: vec![],
        counter_set: Some(counter_set),
    })
}

async fn metric_settings(session: &ObservedSession, probe: &Probe, coverage: &mut Coverage) -> (bool, bool) {
    if !probe.cluster {
        coverage.recorded(
            &sources::COSTS,
            if probe.metrics && probe.rpc_metrics {
                culprit_core::SourceState::Collected
            } else {
                culprit_core::SourceState::Partial
            },
            Some(t!("provider.yugabyte.coverage.native_flags")),
        );
        return (probe.metrics, probe.rpc_metrics);
    }
    let Some(relation) = crate::global::configured(&session.client, "pg_settings").await.ok().flatten() else {
        coverage.partial(&sources::COSTS, t!("provider.yugabyte.coverage.costs_need_global_settings"));
        return (false, false);
    };
    let before = session.notices();
    let expected = crate::global::nodes(session).await;
    let result = query(&session.client, &format!("SELECT server_uuid::text AS node,
        bool_or(name='yb_enable_pg_stat_statements_docdb_metrics' AND setting='on') AS metrics,
        bool_or(name='yb_enable_pg_stat_statements_rpc_stats' AND setting='on') AS rpc_metrics
        FROM {relation} WHERE name IN ('yb_enable_pg_stat_statements_docdb_metrics','yb_enable_pg_stat_statements_rpc_stats')
        GROUP BY server_uuid")).await;
    let rows = coverage.read_rows(&sources::COSTS, result).unwrap_or_default();
    let actual: Option<std::collections::BTreeSet<_>> = rows.iter().map(|r| node_id(&r.string("node"))).collect();
    let after = crate::global::nodes(session).await;
    let complete = expected
        .ok()
        .zip(after.ok())
        .zip(actual)
        .is_some_and(|((before, after), actual)| before == after && before == actual)
        && session.notices() == before
        && !rows.is_empty();
    if !complete {
        coverage.partial(&sources::COSTS, t!("provider.yugabyte.coverage.costs_settings_incomplete"));
        return (false, false);
    }
    let flags = (rows.iter().all(|r| r.flag("metrics")), rows.iter().all(|r| r.flag("rpc_metrics")));
    if !flags.0 || !flags.1 {
        coverage.partial(&sources::COSTS, t!("provider.yugabyte.coverage.costs_partial"));
    }
    flags
}
