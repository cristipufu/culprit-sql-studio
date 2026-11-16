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
    let mut statement_work = Box::new(culprit_core::telemetry::StatementCounterSnapshot::unavailable(
        crate::native::statements::SOURCE,
        culprit_core::ErrorCode::Unsupported,
        None,
    ));
    let workload = match workload {
        Ok(Some(collected)) => {
            if let Some(counters) = collected.statement_work {
                statement_work = counters;
            }
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
        Err(e) => {
            record_statement_failure(
                &mut coverage,
                &mut statement_work,
                e,
                probe.performance_schema,
                &probe.grant_target(),
            );
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
        runtime_populations: None,
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
        compilations_before: None,
        contention_before: None,
        evictions_before: None,
        sizes_before: None,
        operations: culprit_core::operations::OperationalStats {
            statement_work: Some(statement_work),
            index_usage_scope: Some(culprit_core::operations::IndexUsageScope::ConnectedMember),
            ..Default::default()
        },
    };
    snapshot.bind_identity(culprit_core::DatabaseIdentity::server(
        Engine::Mysql,
        &snapshot.server.database,
        &connector.host,
        connector.port,
        None,
    ));
    let replication = crate::signals::collect(&mut reader, &probe, &mut snapshot).await;
    let ordinary_shutdown = reader.close().await;
    if ordinary_shutdown != culprit_core::telemetry::SourceShutdown::Quiescent {
        snapshot.operations.statement_work =
            Some(Box::new(culprit_core::telemetry::StatementCounterSnapshot::unavailable(
                crate::native::statements::SOURCE,
                culprit_core::ErrorCode::QueryFailed,
                Some(culprit_core::telemetry::CollectionStop::CancellationUnconfirmed),
            )));
    }
    crate::signals::settle_replication(&mut snapshot, replication, ordinary_shutdown);
    // The optional source has its own observation bounds and ownership. Never start a new native operation
    // after the ordinary scan left uncertain work on this connector.
    if ordinary_shutdown == culprit_core::telemetry::SourceShutdown::Quiescent {
        let (native_failures, coverage) =
            crate::native_failures::collect(connector.clone(), options.collect_native_failures, &probe).await;
        snapshot.operations.native_failures = native_failures;
        coverage.append_to(&mut snapshot);
        let (session_memory, coverage) =
            crate::session_memory::collect(connector.clone(), options.collect_session_memory, &probe).await;
        snapshot.operations.session_memory = session_memory;
        coverage.append_to(&mut snapshot);
    } else {
        let mut coverage = Coverage::default();
        for (requested, source) in [
            (options.collect_native_failures, &sources::NATIVE_FAILURES),
            (options.collect_session_memory, &sources::SESSION_MEMORY),
        ] {
            if requested {
                coverage
                    .source_failed(source, &culprit_core::telemetry::CollectionStop::CancellationUnconfirmed.error());
            }
        }
        coverage.append_to(&mut snapshot);
    }
    snapshot.duration_ms = started.elapsed().as_secs_f64() * 1000.0;
    Ok(snapshot)
}

/// A slow-log failure belongs to that source, not the disabled Performance Schema counter source.
fn record_statement_failure(
    coverage: &mut Coverage,
    counters: &mut culprit_core::telemetry::StatementCounterSnapshot,
    error: culprit_core::Error,
    performance_schema: bool,
    grant_target: &str,
) {
    if performance_schema {
        let stopped = match &error.detail {
            Some(culprit_core::ErrorDetail::CollectionStopped(stop)) => Some(*stop),
            _ => None,
        };
        *counters = culprit_core::telemetry::StatementCounterSnapshot::unavailable(
            crate::native::statements::SOURCE,
            error.code,
            stopped,
        );
    }
    let error = statement_error(error, performance_schema, grant_target);
    coverage.source_failed(&sources::STATEMENTS, &error);
}

fn statement_error(error: culprit_core::Error, performance_schema: bool, grant_target: &str) -> culprit_core::Error {
    if error.code != culprit_core::ErrorCode::PermissionDenied
        || matches!(error.detail, Some(culprit_core::ErrorDetail::CollectionStopped(_)))
    {
        return error;
    }
    let grant = if performance_schema { PS_GRANT } else { "GRANT SELECT ON mysql.slow_log TO" };
    error.with_hint(format!("{grant} {grant_target};"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use culprit_core::{Error, ErrorCode, SourceState, telemetry::CollectionStop};

    #[test]
    fn workload_permission_guidance_names_the_source_that_actually_failed() {
        for (instrumented, source) in [(true, "performance_schema.*"), (false, "mysql.slow_log")] {
            let mut coverage = Coverage::with(vec![Capability::PerformanceSchema], vec![]);
            let mut counters = culprit_core::telemetry::StatementCounterSnapshot::unavailable(
                crate::native::statements::SOURCE,
                ErrorCode::Unsupported,
                None,
            );
            record_statement_failure(
                &mut coverage,
                &mut counters,
                Error::new(ErrorCode::PermissionDenied, "native permission denied"),
                instrumented,
                "'monitor'@'%'",
            );
            let outcome =
                coverage.outcomes().into_iter().find(|outcome| outcome.source == sources::STATEMENTS.name).unwrap();
            assert_eq!(outcome.state, SourceState::Unavailable);
            assert_eq!(outcome.reason.as_deref(), Some("native permission denied"));
            assert_eq!(counters.coverage.source, crate::native::statements::SOURCE);
            assert_eq!(
                counters.coverage.error_code,
                Some(if instrumented { ErrorCode::PermissionDenied } else { ErrorCode::Unsupported })
            );
            assert!(counters.is_valid() && counters.context.is_none() && counters.observations.is_empty());
            let (_, missing, _) = coverage.finish();
            assert_eq!(missing[0].fix.as_deref(), Some(format!("GRANT SELECT ON {source} TO 'monitor'@'%';").as_str()));
        }
    }

    #[test]
    fn timeout_fence_and_invalid_data_outcomes_preserve_their_cause_instead_of_suggesting_a_grant() {
        let mut errors = [
            CollectionStop::TimedOut,
            CollectionStop::Cancelled,
            CollectionStop::CancellationUnconfirmed,
            CollectionStop::RowLimit,
            CollectionStop::InvalidPage,
        ]
        .into_iter()
        .map(CollectionStop::error)
        .collect::<Vec<_>>();
        errors.extend(
            [ErrorCode::ConnectionFailed, ErrorCode::AuthFailed, ErrorCode::QueryFailed, ErrorCode::Timeout]
                .into_iter()
                .map(|code| Error::new(code, "native source failed")),
        );
        errors.push(
            Error::new(ErrorCode::PermissionDenied, "typed terminal stop")
                .with_detail(culprit_core::ErrorDetail::CollectionStopped(CollectionStop::Cancelled)),
        );
        for original in errors {
            let original = original.with_hint("native recovery guidance");
            for instrumented in [false, true] {
                let error = statement_error(original.clone(), instrumented, "'monitor'@'%'");
                assert_eq!(
                    (error.code, error.detail, &error.message, &error.hint),
                    (original.code, original.detail, &original.message, &original.hint)
                );
                let mut coverage = Coverage::with(vec![Capability::PerformanceSchema], vec![]);
                let mut counters = culprit_core::telemetry::StatementCounterSnapshot::unavailable(
                    crate::native::statements::SOURCE,
                    ErrorCode::Unsupported,
                    None,
                );
                record_statement_failure(&mut coverage, &mut counters, original.clone(), instrumented, "'monitor'@'%'");
                let outcome =
                    coverage.outcomes().into_iter().find(|outcome| outcome.source == sources::STATEMENTS.name).unwrap();
                assert_eq!(outcome.state, SourceState::Unavailable);
                assert_eq!(
                    counters.coverage.error_code,
                    Some(if instrumented { original.code } else { ErrorCode::Unsupported })
                );
                let stopped = match &original.detail {
                    Some(culprit_core::ErrorDetail::CollectionStopped(stop)) if instrumented => Some(*stop),
                    _ => None,
                };
                assert_eq!(counters.coverage.stopped, stopped);
                assert!(counters.is_valid() && counters.context.is_none() && counters.observations.is_empty());
                let (_, missing, _) = coverage.finish();
                assert_eq!(missing[0].fix.as_deref(), Some("native recovery guidance"));
            }
        }
    }
}
