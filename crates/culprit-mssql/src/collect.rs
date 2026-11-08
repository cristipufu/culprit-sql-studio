//! Snapshot orchestration: collect independent sources concurrently and account for their coverage once.

use std::collections::HashMap;
use std::future::Future;
use std::time::Instant;

use culprit_core::{
    Capability, ColumnInfo, Coverage, Engine, ForeignKey, IndexStat, MissingIndex, ResourceSample, Result,
    SNAPSHOT_FORMAT, Setting, Snapshot, SnapshotOptions, StatisticsInfo, TableStat, WaitStat, Workload,
};

use crate::{
    MssqlProvider, advice,
    conn::{self, MsClient},
    operations, pressure,
    probe::{self, Probe},
    schema, signals, sources, workload,
};

/// What a part of the scan's sources could read: the shared account every provider keeps (`culprit_core::Coverage`).
type Outcome = Coverage;

#[derive(Default)]
struct WorkloadPart {
    workload: Option<Workload>,
    waits: Vec<WaitStat>,
    outcome: Outcome,
}

#[derive(Default)]
struct SchemaPart {
    tables: Vec<TableStat>,
    indexes: Vec<IndexStat>,
    columns: Vec<ColumnInfo>,
    foreign_keys: Vec<ForeignKey>,
    statistics: Vec<StatisticsInfo>,
    outcome: Outcome,
}

#[derive(Default)]
struct MiscPart {
    pressure: Option<culprit_core::Pressure>,
    missing_indexes: Vec<MissingIndex>,
    forwarded: HashMap<(String, String), f64>,
    query_families: Vec<culprit_core::QueryFamily>,
    plan_cache: Option<culprit_core::PlanCacheUse>,
    tuning: Option<culprit_core::TuningAdvice>,
    waits: Vec<WaitStat>,
    resources: Vec<ResourceSample>,
    settings: Vec<Setting>,
    outcome: Outcome,
}

/// What the probe already knows about Query Store: none here (an older server, a system database), or one that is off.
pub(crate) fn query_store_account(outcome: &mut Outcome, probe: &Probe) {
    if probe.query_store.is_none() {
        outcome.not_applicable(
            &sources::QUERY_STORE,
            "Query Store requires SQL Server 2016 or later and a user database.",
        );
        outcome.lacking(
            Capability::QueryStore,
            "Query Store is not available here; it needs SQL Server 2016 or later and a user database.",
            None,
        );
    }
    if let Some(qs) = &probe.query_store
        && !qs.readable()
    {
        outcome.recorded(
            &sources::QUERY_STORE,
            culprit_core::SourceState::Unavailable,
            Some(format!("Query Store is {} on this database.", qs.actual)),
        );
        outcome.lacking(
            Capability::QueryStore,
            format!(
                "Query Store is {} on this database, so workload history is limited to what is still in the plan cache.",
                qs.actual.to_ascii_lowercase().replace('_', " ")
            ),
            Some("ALTER DATABASE CURRENT SET QUERY_STORE = ON (OPERATION_MODE = READ_WRITE);".into()),
        );
    }
}

/// Query Store plans whose XML couldn't be read.
pub(crate) fn unparsed_plans(count: usize) -> String {
    format!("{count} Query Store plans could not be parsed")
}

/// This login can't read the plan cache either.
pub(crate) fn plan_cache_unreadable(outcome: &mut Outcome, probe: &Probe) {
    outcome.recorded(
        &sources::PLAN_CACHE,
        culprit_core::SourceState::Unavailable,
        Some("This login cannot read plan cache statistics.".into()),
    );
    outcome.lacking(Capability::PlanCache, "This login cannot read plan cache statistics.", Some(probe.grant_hint()));
}

/// Index usage counters this login can't read.
pub(crate) fn index_usage_unreadable(outcome: &mut Outcome, probe: &Probe) {
    outcome.lacking(
        Capability::IndexUsage,
        "Index usage counters are not readable with this login, so unused-index checks are off.",
        Some(probe.grant_hint()),
    );
}

/// What the server state views would have delivered, without the permission to read them.
pub(crate) fn server_state_unreadable(outcome: &mut Outcome, probe: &Probe) {
    for capability in [Capability::MissingIndexes, Capability::WaitStats] {
        outcome.lacking(
            capability,
            "This login lacks VIEW DATABASE STATE / VIEW SERVER STATE.",
            Some(probe.grant_hint()),
        );
    }
}

async fn collect_workload(client: &mut MsClient, probe: &Probe, options: &SnapshotOptions) -> WorkloadPart {
    let mut part = WorkloadPart::default();
    let dmv = probe.can_read_dmvs();
    query_store_account(&mut part.outcome, probe);

    if probe.query_store_readable() {
        let grant = || probe.grant_hint();
        if let Some(result) = sources::read(
            &mut part.outcome,
            &sources::QUERY_STORE,
            workload::from_query_store(client, options).await,
            grant,
        ) {
            part.outcome.merge(result.coverage);
            part.outcome.completed(&sources::QUERY_STORE, result.workload.queries.len());
            part.outcome.delivered(Capability::QueryStore);
            if result.plans_parsed > 0 {
                part.outcome.delivered(Capability::QueryPlans);
            }
            if result.plan_errors > 0 {
                part.outcome.note("plans", unparsed_plans(result.plan_errors));
            }
            if !result.waits.is_empty() {
                part.outcome.delivered(Capability::WaitStats);
            }
            part.waits = result.waits;
            part.workload = Some(result.workload);
            return part;
        }
    }

    if !dmv {
        plan_cache_unreadable(&mut part.outcome, probe);
        return part;
    }
    let plan_cache = workload::from_plan_cache(client, options).await;
    if let Some(result) = sources::read(&mut part.outcome, &sources::PLAN_CACHE, plan_cache, || probe.grant_hint()) {
        part.outcome.completed(&sources::PLAN_CACHE, result.workload.queries.len());
        part.outcome.delivered(Capability::PlanCache);
        if result.plans_parsed > 0 {
            part.outcome.delivered(Capability::QueryPlans);
        }
        part.workload = Some(result.workload);
    }
    part
}

/// Returns `Some(created)` only when the database creation timestamp is strictly newer than the server's start time
/// (or when the server start time is unknown).
fn created_since_server_start(started_at: Option<&str>, created: String) -> Option<String> {
    let epoch = |t: &str| chrono::DateTime::parse_from_rfc3339(t).ok().map(|d| d.timestamp());
    match started_at.and_then(epoch) {
        Some(started) if epoch(&created).is_some_and(|c| c <= started) => None,
        _ => Some(created),
    }
}

/// The latest this database's index usage counters may have started, when later than the server's start (the age every
/// counter has otherwise): a database restored or created since then starts its counters with it (sys.databases'
/// create_date), and one with AUTO_CLOSE on starts them again each time it opens, so as far as anyone knows, now.
async fn usage_counted_since(client: &mut MsClient, probe: &Probe) -> Option<String> {
    let r = conn::query_one(
        client,
        "SELECT is_auto_close_on AS auto_close, CONVERT(varchar(19), DATEADD(minute, DATEDIFF(minute, GETDATE(), GETUTCDATE()), create_date), 126) + 'Z' AS created FROM sys.databases WHERE database_id = DB_ID()",
    )
    .await
    .ok()?;
    if r.bool("auto_close") {
        return Some(chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string());
    }
    let created = r.str("created")?;
    created_since_server_start(probe.started_at.as_deref(), created)
}

async fn collect_schema(client: &mut MsClient, probe: &Probe) -> SchemaPart {
    let mut part = SchemaPart::default();
    let dmv = probe.can_read_dmvs();
    let grant = || probe.grant_hint();
    let out = &mut part.outcome;
    if let Some(tables) = sources::read_rows(out, &sources::TABLES, schema::tables(client, dmv).await, grant) {
        part.tables = tables;
        out.delivered(Capability::TableStats);
    }
    if let Some((indexes, with_usage)) =
        sources::read(out, &sources::INDEXES, schema::indexes(client, dmv).await, grant)
    {
        out.completed(&sources::INDEXES, indexes.len());
        part.indexes = indexes;
        if with_usage {
            // Index usage is counted per database, from when it came online: no older than the server's start.
            let since = usage_counted_since(client, probe).await;
            part.indexes.iter_mut().for_each(|i| i.counters_since = since.clone());
            out.delivered(Capability::IndexUsage);
        } else {
            index_usage_unreadable(out, probe);
        }
    }
    part.columns = sources::read_rows(out, &sources::COLUMNS, schema::columns(client).await, grant).unwrap_or_default();
    part.foreign_keys =
        sources::read_rows(out, &sources::FOREIGN_KEYS, schema::foreign_keys(client).await, grant).unwrap_or_default();
    if let Some(stats) = sources::read_rows(out, &sources::STATISTICS, schema::statistics(client).await, grant) {
        part.statistics = stats;
        out.delivered(Capability::Statistics);
    }
    part
}

async fn collect_misc(client: &mut MsClient, probe: &Probe, options: &SnapshotOptions) -> MiscPart {
    let mut part = MiscPart::default();
    let dmv = probe.can_read_dmvs();
    let grant = || probe.grant_hint();
    if dmv {
        let missing = schema::missing_indexes(client).await;
        if let Some(missing) = sources::read_rows(&mut part.outcome, &sources::MISSING_INDEXES, missing, grant) {
            part.missing_indexes = missing;
            part.outcome.delivered(Capability::MissingIndexes);
        }
        let waits = schema::waits(client, probe.is_azure_sql_database()).await;
        if let Some(waits) = sources::read_rows(&mut part.outcome, &sources::WAITS, waits, grant) {
            part.waits = waits;
            part.outcome.delivered(Capability::WaitStats);
        }
        let (pressure, read) = pressure::collect(client, probe).await;
        part.outcome.merge(read);
        part.pressure = Some(pressure);
    } else {
        server_state_unreadable(&mut part.outcome, probe);
    }
    if probe.is_azure_sql_database() {
        let resources = schema::resources(client).await;
        if let Some(resources) = sources::read_rows(&mut part.outcome, &sources::RESOURCES, resources, grant) {
            part.resources = resources;
            part.outcome.delivered(Capability::ResourceUsage);
        }
    }
    if dmv {
        // The index advisor and tuning checks: each adds detail, and a failure is a note.
        let out = &mut part.outcome;
        part.forwarded =
            sources::read(out, &sources::FORWARDED_FETCHES, advice::forwarded_fetches(client).await, grant)
                .unwrap_or_default();
        let query_store = probe.query_store_readable();
        let families = advice::query_families(client, options.window_in_minutes(), query_store).await;
        part.query_families = sources::read_rows(out, &sources::QUERY_FAMILIES, families, grant).unwrap_or_default();
        part.plan_cache =
            sources::read(out, &sources::PLAN_CACHE_USE, advice::plan_cache(client).await, grant).flatten();
        part.tuning = sources::read(out, &sources::TUNING, advice::tuning(client, query_store).await, grant);
    }
    let query_store_options = probe.query_store.as_ref().map(|qs| &qs.options);
    let settings = schema::settings(client, probe.is_azure_sql_database(), query_store_options).await;
    part.settings = sources::read_rows(&mut part.outcome, &sources::SETTINGS, settings, grant).unwrap_or_default();
    part
}

impl MssqlProvider {
    /// Runs `f` on its own pooled session so collector groups proceed concurrently.
    async fn on_session<T: Default, F, Fut>(&self, f: F) -> (T, Option<culprit_core::Error>)
    where
        F: FnOnce(MsClient) -> Fut,
        Fut: Future<Output = (T, MsClient)>,
    {
        match self.pool.get().await {
            Ok(client) => {
                let (value, client) = f(client).await;
                self.pool.put(client);
                (value, None)
            }
            Err(err) => (T::default(), Some(err)),
        }
    }
}

/// Collect a full SQL Server snapshot. Each collector that fails becomes a note; only an unreachable server is an error.
pub(crate) async fn snapshot(
    provider: &MssqlProvider,
    connection: &str,
    options: &SnapshotOptions,
) -> Result<Snapshot> {
    let started = Instant::now();
    let taken_at = chrono::Utc::now();
    let probe = provider.pool.with(|c| Box::pin(probe::run(c))).await?;
    let probe_ref = &probe;

    let ((workload_part, workload_error), (schema_part, schema_error), (misc_part, misc_error)) = tokio::join!(
        provider.on_session(move |mut c| async move {
            let part = collect_workload(&mut c, probe_ref, options).await;
            (part, c)
        }),
        provider.on_session(move |mut c| async move {
            let part = collect_schema(&mut c, probe_ref).await;
            (part, c)
        }),
        provider.on_session(move |mut c| async move {
            let part = collect_misc(&mut c, probe_ref, options).await;
            (part, c)
        }),
    );

    let mut outcome = Outcome::default();
    let (expected_have, expected_missing) = probe::expected_capabilities(&probe);
    for capability in [Capability::Showplan, Capability::Sessions] {
        if expected_have.contains(&capability) {
            outcome.delivered(capability);
        }
    }
    for m in expected_missing {
        if matches!(m.capability, Capability::Showplan | Capability::Sessions) {
            outcome.lacking(m.capability, m.reason, m.fix);
        }
    }
    outcome.merge(workload_part.outcome);
    if let Some(error) = workload_error {
        outcome.sources_failed(sources::WORKLOAD, &error);
    }
    outcome.merge(schema_part.outcome);
    outcome.merge(misc_part.outcome);
    if let Some(error) = schema_error {
        outcome.sources_failed(sources::SCHEMA, &error);
    }
    if let Some(error) = misc_error {
        outcome.sources_failed(sources::MISC, &error);
    }
    let sources = outcome.outcomes();
    let (have, missing, notes) = outcome.finish();

    let waits = if workload_part.waits.is_empty() { misc_part.waits } else { workload_part.waits };
    let mut tables = schema_part.tables;
    for t in &mut tables {
        t.forwarded_fetches = misc_part.forwarded.get(&(t.schema.to_lowercase(), t.name.to_lowercase())).copied();
    }
    let mut snapshot = Snapshot {
        identity: None,
        sources,
        format: SNAPSHOT_FORMAT,
        id: taken_at.format("%Y%m%dT%H%M%SZ").to_string(),
        connection: connection.to_string(),
        environment: provider.environment,
        taken_at: taken_at.format("%Y-%m-%dT%H:%M:%SZ").to_string(),
        duration_ms: started.elapsed().as_secs_f64() * 1_000.0,
        server: probe::server_info(&probe, have, missing),
        workload: workload_part.workload,
        tables,
        indexes: schema_part.indexes,
        columns: schema_part.columns,
        foreign_keys: schema_part.foreign_keys,
        missing_indexes: misc_part.missing_indexes,
        statistics: schema_part.statistics,
        waits,
        resources: misc_part.resources,
        settings: misc_part.settings,
        pressure: misc_part.pressure,
        notes,
        options: Some(options.clone()),
        query_families: misc_part.query_families,
        plan_cache: misc_part.plan_cache,
        tuning: misc_part.tuning,
        deadlocks: None,
        constraints: Vec::new(),
        histograms: None,
        escalations_before: None,
        contention_before: None,
        evictions_before: None,
        sizes_before: None,
        operations: Default::default(),
    };
    match provider.pool.get().await {
        Ok(mut client) => {
            signals::collect(&mut client, &probe, options, &mut snapshot).await;
            operations::collect(&mut client, &probe, &mut snapshot).await;
            provider.pool.put(client);
        }
        Err(error) => {
            let mut coverage = Coverage::default();
            coverage.sources_failed(sources::SIGNAL_READS, &error);
            coverage.append_to(&mut snapshot);
        }
    }
    snapshot.bind_identity(culprit_core::DatabaseIdentity::server(
        Engine::Mssql,
        &snapshot.server.database,
        &provider.endpoint_host,
        provider.endpoint_port,
        None,
    ));
    snapshot.duration_ms = started.elapsed().as_secs_f64() * 1_000.0;
    Ok(snapshot)
}

#[cfg(test)]
mod tests {
    use super::created_since_server_start;

    #[test]
    fn created_since_server_start_only_reports_databases_created_after_server_boot() {
        let server_boot = Some("2026-03-10T12:00:00Z");
        // Created before server boot -> counters reset at server boot, so None.
        assert_eq!(created_since_server_start(server_boot, "2026-01-01T00:00:00Z".into()), None);
        // Created at the exact server boot second -> None.
        assert_eq!(created_since_server_start(server_boot, "2026-03-10T12:00:00Z".into()), None);
        // Restored/created after server boot -> returns database creation timestamp.
        assert_eq!(
            created_since_server_start(server_boot, "2026-03-12T08:30:00Z".into()),
            Some("2026-03-12T08:30:00Z".into()),
        );
        // Server boot unknown -> returns database creation timestamp conservatively.
        assert_eq!(
            created_since_server_start(None, "2026-01-01T00:00:00Z".into()),
            Some("2026-01-01T00:00:00Z".into())
        );
    }
}
