use crate::{YugabyteProvider, catalog, probe, sources, workload};
use culprit_core::{Capability, Coverage, SNAPSHOT_FORMAT, Snapshot, SnapshotOptions, StatisticsInfo};
use culprit_postgres::wire::COLLECTOR;
use std::time::Instant;

pub(crate) async fn snapshot(
    provider: &YugabyteProvider,
    name: &str,
    options: &SnapshotOptions,
) -> culprit_core::Result<Snapshot> {
    let started = Instant::now();
    let session = provider.connector.connect_observed(COLLECTOR).await?;
    let probe = probe::read(&session.client).await?;
    let mut coverage = Coverage::default();
    let tables = coverage.read_rows(&sources::TABLES, catalog::tables(&session.client).await).unwrap_or_default();
    let indexes =
        coverage.read_rows(&sources::INDEXES, catalog::indexes(&session.client, &probe).await).unwrap_or_default();
    let columns =
        coverage.read_rows(&sources::COLUMNS, catalog::columns(&session.client, &probe).await).unwrap_or_default();
    let foreign_keys =
        coverage.read_rows(&sources::FOREIGN_KEYS, catalog::foreign_keys(&session.client).await).unwrap_or_default();
    let settings = coverage.read_rows(&sources::SETTINGS, catalog::settings(&session.client).await).unwrap_or_default();
    let constraints = catalog::constraints(&session.client).await;
    let truncated = constraints.as_ref().is_ok_and(|(_, truncated)| *truncated);
    let constraints = coverage.read_rows(&sources::CONSTRAINTS, constraints.map(|(rows, _)| rows)).unwrap_or_default();
    if truncated {
        coverage
            .partial(&sources::CONSTRAINTS, culprit_core::message!("provider.yugabyte.coverage.constraints_truncated"));
    }
    let workload = workload::read(&session, &probe, options.top_queries, &mut coverage).await;
    let mut snapshot = Snapshot {
        format: SNAPSHOT_FORMAT,
        id: probe.now.replace(['-', ':'], ""),
        connection: name.into(),
        environment: provider.spec.environment,
        taken_at: probe.now.clone(),
        duration_ms: 0.0,
        server: probe.server(),
        workload,
        tables,
        indexes,
        columns,
        foreign_keys,
        missing_indexes: vec![],
        statistics: vec![],
        waits: vec![],
        resources: vec![],
        settings,
        notes: vec![],
        pressure: None,
        options: Some(options.clone()),
        query_families: vec![],
        plan_cache: None,
        tuning: None,
        deadlocks: None,
        constraints,
        histograms: None,
        escalations_before: None,
        contention_before: None,
        evictions_before: None,
        sizes_before: None,
        sources: vec![],
        identity: None,
        operations: Default::default(),
    };
    catalog::details(&session.client, &mut snapshot, &mut coverage).await;
    crate::index_usage::read(&session, &probe, &mut snapshot, &mut coverage).await;
    snapshot.operations.node_resources = crate::resources::collect(&session, &mut coverage).await;
    snapshot.operations.activity_history = crate::ash::collect(&session, &probe, options, &mut coverage).await;
    snapshot.operations.termination_history =
        crate::terminations::collect(&session, &probe, options, &mut coverage).await;
    snapshot.statistics = snapshot
        .tables
        .iter()
        .map(|t| StatisticsInfo {
            schema: t.schema.clone(),
            table: t.name.clone(),
            name: t.name.clone(),
            last_updated: None,
            rows: t.rows,
            rows_sampled: None,
            modifications: None,
            auto_created: false,
        })
        .collect();
    if let Some(w) = &mut snapshot.workload {
        capture_plans(provider, &session, &probe, w, options.plans, &mut coverage).await;
    }
    coverage.note("DocDB measurements", culprit_core::message!("provider.yugabyte.coverage.docdb_measurements"));
    coverage.append_to(&mut snapshot);
    let mut identity = culprit_core::DatabaseIdentity::server(
        culprit_core::Engine::Yugabyte,
        &snapshot.server.database,
        &provider.connector.host,
        provider.connector.port,
        None,
    );
    identity.incarnation = Some(probe.oid.to_string());
    snapshot.bind_identity(identity);
    snapshot.duration_ms = started.elapsed().as_secs_f64() * 1000.0;
    Ok(snapshot)
}

pub(crate) async fn capture_plans(
    provider: &YugabyteProvider,
    session: &culprit_postgres::wire::ObservedSession,
    probe: &probe::Probe,
    workload: &mut culprit_core::Workload,
    requested: u32,
    coverage: &mut Coverage,
) {
    use culprit_postgres::wire::{elevated_login, own_ysql_text};
    use tokio::time::{Instant, timeout_at};
    if workload.queries.is_empty() {
        return;
    }
    // One budget for the phase, including ownership checks and connection setup, rather than 1,000 timeouts.
    let deadline = Instant::now() + COLLECTOR.statement_timeout;
    if timeout_at(deadline, crate::history::collect(session, probe, workload, coverage)).await.is_err() {
        coverage.partial(
            &sources::PLAN_HISTORY,
            culprit_core::message!("provider.yugabyte.coverage.plan_history_deadline"),
        );
        return;
    }
    if requested == 0 {
        return;
    }
    let mut retained = culprit_core::limits::ResultBudget::new(requested.min(1000) as usize * 2);
    if timeout_at(
        deadline,
        crate::history::capture(session, probe, workload, requested.min(1000) as usize, &mut retained, coverage),
    )
    .await
    .is_err()
    {
        coverage.partial(
            &sources::PLAN_HISTORY,
            culprit_core::message!("provider.yugabyte.coverage.plan_capture_deadline"),
        );
    }
    let client = &session.client;
    let elevated = match timeout_at(deadline, elevated_login(client)).await {
        Ok(Ok(value)) => Some(value),
        _ => {
            coverage.note("YSQL plans", culprit_core::message!("provider.yugabyte.coverage.plan_permissions"));
            None
        }
    };
    let mut failed = false;
    let mut foreign = false;
    for q in workload.queries.iter_mut().take(requested.min(1000) as usize) {
        if retained.truncated() {
            break;
        }
        let capture = async {
            let text = if elevated.ok_or_else(culprit_postgres::wire::foreign_statement)? {
                own_ysql_text(client, &q.id, probe.cluster)
                    .await?
                    .ok_or_else(culprit_postgres::wire::foreign_statement)?
            } else {
                q.text.clone()
            };
            provider
                .execution
                .explain_ysql(&text, culprit_core::ExplainMode::Estimated)
                .await
                .and_then(crate::plan::native)
        };
        match timeout_at(deadline, capture).await {
            Ok(Ok(plan)) => {
                let bytes = plan
                    .raw
                    .as_deref()
                    .map_or(0, str::len)
                    .saturating_add(plan.statement.as_deref().map_or(0, str::len));
                if !retained.retain(bytes) {
                    coverage.note("YSQL plans", culprit_core::message!("provider.yugabyte.coverage.plans_budget"));
                    break;
                }
                q.plan = Some(plan);
            }
            Ok(Err(error)) => {
                foreign |= error.code == culprit_core::ErrorCode::PermissionDenied;
                failed = true;
            }
            Err(_) => {
                coverage.note("YSQL plans", culprit_core::message!("provider.yugabyte.coverage.plans_deadline"));
                break;
            }
        }
    }
    if failed {
        coverage.note("YSQL plans", culprit_core::message!("provider.yugabyte.coverage.plans_failed"));
    }
    if foreign {
        coverage.note("YSQL plans", culprit_core::message!("provider.yugabyte.coverage.plans_foreign"));
    }
    if workload
        .queries
        .iter()
        .any(|q| q.plan.is_some() || q.plan_history.as_ref().is_some_and(|h| h.captured_plan.is_some()))
    {
        coverage.delivered(Capability::Showplan);
        coverage.delivered(Capability::QueryPlans);
    }
}
