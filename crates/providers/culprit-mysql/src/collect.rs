//! Snapshot orchestration: collect independent source groups and account for their coverage once.
use crate::{
    catalog::{columns, foreign_keys, indexes, replica_role, settings, tables},
    connect::{Connector, Scope},
    probe::probe,
    sources,
    workload::collect_workload,
};
use culprit_core::{
    Capability, ConnectionSpec, Coverage, Engine, Result, SNAPSHOT_FORMAT, Setting, Snapshot, SnapshotOptions,
    WorkloadKind,
};
use std::time::Instant;

const PS_GRANT: &str = "GRANT SELECT ON performance_schema.* TO";

/// Collect a full snapshot. Each collector that fails becomes a note; only an unreachable server is an error.
pub async fn snapshot(
    connector: &std::sync::Arc<Connector>,
    spec: &ConnectionSpec,
    connection: &str,
    options: &SnapshotOptions,
) -> Result<Snapshot> {
    let started = Instant::now();
    let mut reader = crate::read::ReadSession::application(connector.clone(), Scope::Server);
    let probe = probe(&mut reader, connector).await?;
    let db = probe.database.clone();
    // Metadata readers share one native owner. A scan does not multiply pressure with parallel source connections.
    let workload = collect_workload(&mut reader, connector, &probe, options).await;
    let tables_result = tables(&mut reader, &db).await;
    let columns_result = columns(&mut reader, &db).await;
    let indexes_result = indexes(&mut reader, &db, &probe).await;
    let fks_result = foreign_keys(&mut reader, &db).await;
    let mut settings_result = settings(&mut reader, probe.flavor).await;
    if let (Ok(settings), Some(role)) = (settings_result.as_mut(), replica_role(&mut reader, &probe).await) {
        settings.push(Setting {
            name: culprit_core::REPLICA_SETTING.into(),
            value: role.into(),
            default_value: None,
            scope: "server".into(),
        });
    }
    let waits_result = crate::pressure::waits(&mut reader).await;
    let (pressure, pressure_coverage) = crate::pressure::collect(&mut reader).await;

    let mut server = probe.server_info();
    // What the sources could read, starting from what the probe found (`culprit_core::Coverage`).
    let mut coverage = Coverage::with(std::mem::take(&mut server.capabilities), std::mem::take(&mut server.missing));
    if options.collect_shared_resources {
        coverage.shared_resource_details_unavailable(Engine::Mysql);
    }
    if options.collect_request_memory {
        coverage.request_memory_unavailable(Engine::Mysql);
    }

    let mut source_waits = Vec::new();
    let workload = match workload {
        Ok(Some(collected)) => {
            coverage.completed(&sources::STATEMENTS, collected.workload.queries.len());
            source_waits = collected.waits;
            for (collector, message) in collected.notes {
                coverage.note(collector, message);
            }
            if collected.workload.kind == Some(WorkloadKind::SlowLog) {
                coverage.delivered(Capability::SlowLog);
            }
            let planned = collected.workload.queries.iter().any(|q| q.plan.is_some());
            if planned {
                coverage.delivered(Capability::QueryPlans);
            } else if let Some((reason, fix)) = collected.no_plans {
                let mut missing = culprit_core::MissingCapability::new(Capability::QueryPlans, reason);
                if let Some(fix) = fix {
                    missing = missing.with_fix(fix);
                }
                coverage.lost_capability(missing);
            } else {
                coverage.withdraw(Capability::QueryPlans);
            }
            Some(collected.workload)
        }
        Ok(None) => {
            coverage.recorded_message(
                &sources::STATEMENTS,
                culprit_core::SourceState::Unavailable,
                culprit_core::message!("provider.mysql.statements.unavailable"),
            );
            None
        }
        // The digests' grant is the fix whatever the error said.
        Err(e) => {
            coverage.source_failed(&sources::STATEMENTS, &e.with_hint(format!("{PS_GRANT} {};", probe.grant_target())));
            None
        }
    };

    // Each source through `Coverage::read`: one that fails is a note and its capabilities locked (`sources`).
    let indexes_out = coverage
        .read(&sources::INDEXES, indexes_result)
        .map(|(indexes, usage_note)| {
            coverage.completed(&sources::INDEXES, indexes.len());
            if let Some(message) = usage_note {
                coverage.note("index_usage", message.clone());
                coverage.lost(Capability::IndexUsage, message, Some(format!("{PS_GRANT} {};", probe.grant_target())));
            }
            indexes
        })
        .unwrap_or_default();
    let tables_out = coverage.read_rows(&sources::TABLES, tables_result).unwrap_or_default();
    let columns_out = coverage.read_rows(&sources::COLUMNS, columns_result).unwrap_or_default();
    let fks_out = coverage.read_rows(&sources::FOREIGN_KEYS, fks_result).unwrap_or_default();
    coverage.merge(pressure_coverage);
    let mut waits_out = coverage.read_rows(&sources::WAITS, waits_result).unwrap_or_default();
    if !waits_out.is_empty() {
        coverage.delivered(Capability::WaitStats);
    }
    let settings_out = coverage.read_rows(&sources::SETTINGS, settings_result).unwrap_or_default();
    let pressure_out = Some(pressure);
    if waits_out.is_empty() {
        waits_out = std::mem::take(&mut source_waits);
    }

    let sources = coverage.outcomes();
    let (have, missing, notes) = coverage.finish();
    server.capabilities = have;
    server.missing = missing;

    let mut snapshot = Snapshot {
        identity: None,
        sources,
        format: SNAPSHOT_FORMAT,
        id: probe.now.replace(['-', ':'], ""),
        connection: connection.to_string(),
        environment: spec.environment,
        taken_at: probe.now.clone(),
        duration_ms: started.elapsed().as_secs_f64() * 1000.0,
        server,
        workload,
        tables: tables_out,
        indexes: indexes_out,
        columns: columns_out,
        foreign_keys: fks_out,
        missing_indexes: Vec::new(),
        statistics: Vec::new(),
        waits: waits_out,
        resources: Vec::new(),
        settings: settings_out,
        pressure: pressure_out,
        notes,
        options: Some(options.clone()),
        query_families: Vec::new(),
        plan_cache: None,
        tuning: None,
        deadlocks: None,
        constraints: Vec::new(),
        histograms: None,
        escalations_before: None,
        contention_before: None,
        evictions_before: None,
        sizes_before: None,
        operations: Default::default(),
    };
    snapshot.bind_identity(culprit_core::DatabaseIdentity::server(
        Engine::Mysql,
        &snapshot.server.database,
        &connector.host,
        connector.port,
        None,
    ));
    crate::signals::collect(&mut reader, &probe, &mut snapshot).await;
    reader.close().await;
    snapshot.duration_ms = started.elapsed().as_secs_f64() * 1000.0;
    Ok(snapshot)
}
