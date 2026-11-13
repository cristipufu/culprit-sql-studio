//! Snapshot orchestration: collect independent sources concurrently and account for their coverage once.

use std::future::Future;
use std::time::Instant;

use culprit_core::{
    Capability, ColumnInfo, Coverage, Engine, ForeignKey, IndexStat, MissingCapability, MissingIndex, ResourceSample,
    Result, SNAPSHOT_FORMAT, Setting, Snapshot, SnapshotOptions, StatisticsInfo, TableStat, WaitStat, Workload,
};

use crate::{
    MssqlProvider, advice, operations, pressure,
    probe::{self, Probe},
    read::{NativeRead, ReadSession},
    resources, signals, sources, workload,
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
    missing_indexes: Vec<MissingIndex>,
    constraints: Vec<culprit_core::ConstraintIssue>,
    outcome: Outcome,
}

#[derive(Default)]
struct MiscPart {
    pressure: Option<culprit_core::Pressure>,
    query_families: Vec<culprit_core::QueryFamily>,
    plan_cache: Option<culprit_core::PlanCacheUse>,
    tuning: Option<culprit_core::TuningAdvice>,
    waits: Vec<WaitStat>,
    settings: Vec<Setting>,
    outcome: Outcome,
}

#[derive(Default)]
struct ResourcePart {
    samples: Vec<ResourceSample>,
    signals: Vec<culprit_core::Signal>,
    outcome: Outcome,
}
fn resource_part(result: Result<Vec<resources::Sample>>, include_pressure: bool, grant: &str) -> ResourcePart {
    let mut part = ResourcePart::default();
    let result = result.map_err(|error| {
        if error.code == culprit_core::ErrorCode::PermissionDenied { error.with_hint(grant.to_owned()) } else { error }
    });
    if include_pressure {
        part.outcome.read(&sources::RESOURCE_USE, result.as_ref().map(|_| ()).map_err(Clone::clone));
    }
    if let Some(samples) = part.outcome.read(&sources::RESOURCES, result) {
        part.outcome.completed(&sources::RESOURCES, samples.len());
        part.outcome.delivered(Capability::ResourceUsage);
        if include_pressure {
            part.signals = pressure::resource_signals(&resources::Summary::from_samples(&samples));
        }
        part.samples = samples.iter().map(resources::Sample::snapshot).collect();
    }
    part
}
async fn collect_resources(provider: &MssqlProvider, probe: &Probe) -> ResourcePart {
    if !probe.is_azure_sql_database() {
        return ResourcePart::default();
    }
    resource_part(resources::snapshot(provider.pool.clone()).await, probe.can_read_dmvs(), &probe.grant_hint())
}

/// What the probe already knows about Query Store: none here (an older server, a system database), or one that is off.
pub(crate) fn query_store_account(outcome: &mut Outcome, probe: &Probe) {
    if probe.query_store.is_none() {
        outcome.not_applicable(
            &sources::QUERY_STORE,
            culprit_core::message!("provider.mssql.capability.query_store_requires"),
        );
        outcome.lacking(
            Capability::QueryStore,
            culprit_core::message!("provider.mssql.capability.query_store_missing"),
            None,
        );
    }
    if let Some(qs) = &probe.query_store
        && !qs.readable()
    {
        outcome.recorded_message(
            &sources::QUERY_STORE,
            culprit_core::SourceState::Unavailable,
            qs.unavailable_message(),
        );
        outcome.missing_capability(MissingCapability::sql(
            Capability::QueryStore,
            qs.limited_history_message(),
            "ALTER DATABASE CURRENT SET QUERY_STORE = ON (OPERATION_MODE = READ_WRITE);",
        ));
    }
}

/// Query Store plans whose XML couldn't be read.
pub(crate) fn unparsed_plans(count: usize) -> culprit_core::i18n::Message {
    culprit_core::message!(
        "provider.mssql.note.unparsed_plans",
        n = count,
        count = culprit_core::i18n::MessageValue::Digits(count.to_string())
    )
}

/// This login can't read the plan cache either.
pub(crate) fn plan_cache_unreadable(outcome: &mut Outcome, probe: &Probe) {
    let reason = culprit_core::message!("provider.mssql.capability.plan_cache_unreadable");
    outcome.recorded_message(&sources::PLAN_CACHE, culprit_core::SourceState::Unavailable, reason.clone());
    outcome.missing_capability(MissingCapability::sql(Capability::PlanCache, reason, probe.grant_hint()));
}

/// Index usage counters this login can't read.
pub(crate) fn index_usage_unreadable(outcome: &mut Outcome, probe: &Probe) {
    outcome.missing_capability(MissingCapability::sql(
        Capability::IndexUsage,
        culprit_core::message!("provider.mssql.capability.index_usage_unreadable"),
        probe.grant_hint(),
    ));
}

/// What the server state views would have delivered, without the permission to read them.
pub(crate) fn server_state_unreadable(outcome: &mut Outcome, probe: &Probe) {
    for capability in [Capability::MissingIndexes, Capability::WaitStats] {
        outcome.missing_capability(MissingCapability::sql(
            capability,
            culprit_core::message!("provider.mssql.capability.no_server_state"),
            probe.grant_hint(),
        ));
    }
}

async fn collect_workload(provider: &MssqlProvider, probe: &Probe, options: &SnapshotOptions) -> WorkloadPart {
    let mut part = WorkloadPart::default();
    let dmv = probe.can_read_dmvs();
    query_store_account(&mut part.outcome, probe);

    if probe.query_store_readable() {
        let grant = || probe.grant_hint();
        let mut reader = crate::query_store::Reader::open(provider.pool.clone());
        let result = workload::from_query_store(&mut reader, options).await;
        let result = if reader.close().await == culprit_core::telemetry::SourceShutdown::Unconfirmed {
            Err(culprit_core::telemetry::CollectionStop::CancellationUnconfirmed.error())
        } else {
            result
        };
        let stop_followup = result.as_ref().err().is_some_and(crate::read::stops_remaining_reads);
        if let Some(result) = part.outcome.read(
            &sources::QUERY_STORE,
            result.map_err(|error| {
                if error.code == culprit_core::ErrorCode::PermissionDenied { error.with_hint(grant()) } else { error }
            }),
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
        if stop_followup {
            return part;
        }
    }

    if !dmv {
        plan_cache_unreadable(&mut part.outcome, probe);
        return part;
    }
    let mut client = ReadSession::application(provider.pool.clone());
    let plan_cache = workload::from_plan_cache(&mut client, options).await;
    let plan_cache = client.finish(plan_cache).await;
    if let Some(result) = part.outcome.read(
        &sources::PLAN_CACHE,
        plan_cache.map_err(|error| {
            if error.code == culprit_core::ErrorCode::PermissionDenied {
                error.with_hint(probe.grant_hint())
            } else {
                error
            }
        }),
    ) {
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
async fn usage_counted_since(client: &mut impl NativeRead, probe: &Probe) -> Option<String> {
    let r = crate::read::one(
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

async fn collect_schema(client: &mut impl NativeRead, probe: &Probe) -> SchemaPart {
    let mut part = SchemaPart::default();
    let dmv = probe.can_read_dmvs();
    let grant = || probe.grant_hint();
    let out = &mut part.outcome;
    let mut catalog_complete = probe.can_view_definition;
    if let Some(read) = sources::read(out, &sources::TABLES, crate::tables::application(client, dmv).await, grant) {
        out.completed(&sources::TABLES, read.tables.len());
        catalog_complete &= read.complete_scope;
        part.tables = read.tables;
        out.delivered(Capability::TableStats);
    }
    if let Some((indexes, with_usage)) =
        sources::read(out, &sources::INDEXES, crate::indexes::application(client, dmv).await, grant)
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
    part.columns = sources::read_rows(out, &sources::COLUMNS, crate::columns::application(client).await, grant)
        .unwrap_or_default();
    part.foreign_keys =
        sources::read_rows(out, &sources::FOREIGN_KEYS, crate::foreign_keys::application(client).await, grant)
            .unwrap_or_default();
    part.constraints = sources::read_rows(out, &sources::CONSTRAINTS, crate::constraints::read(client).await, grant)
        .unwrap_or_default();
    if let Some(read) = sources::read(out, &sources::STATISTICS, crate::statistics::application(client).await, grant) {
        out.completed(&sources::STATISTICS, read.statistics.len());
        if !read.properties_complete {
            out.partial(&sources::STATISTICS, culprit_core::message!("provider.mssql.statistics.partial_properties"));
        }
        part.statistics = read.statistics;
        out.delivered(Capability::Statistics);
    }
    if dmv {
        let missing = crate::missing_indexes::application(client).await;
        if let Some(read) = sources::read(out, &sources::MISSING_INDEXES, missing, grant) {
            out.completed(&sources::MISSING_INDEXES, read.indexes.len());
            if let Some(error) = read.query_link_failure {
                out.source_failed(&sources::MISSING_INDEXES, &error);
            }
            part.missing_indexes = read.indexes;
            out.delivered(Capability::MissingIndexes);
        }
    }
    match crate::catalog_visibility::complete(client).await {
        Ok(complete) => catalog_complete &= complete,
        Err(error) => out.sources_failed(sources::SCHEMA, &error),
    }
    if !catalog_complete {
        restrict_catalog_scope(out);
    }
    part
}

fn restrict_catalog_scope(outcome: &mut Outcome) {
    let collected = outcome.outcomes();
    let reason = culprit_core::message!("provider.mssql.catalog.partial_metadata");
    let mut restricted = false;
    for source in sources::SCHEMA {
        if collected.iter().any(|result| {
            result.source == source.name
                && matches!(result.state, culprit_core::SourceState::Collected | culprit_core::SourceState::Empty)
        }) {
            outcome.recorded_message(source, culprit_core::SourceState::Partial, reason.clone());
            restricted = true;
        }
    }
    if restricted {
        outcome.note(culprit_core::source_id::CATALOG, reason);
    }
}

async fn collect_misc(
    client: &mut impl NativeRead,
    provider: &MssqlProvider,
    probe: &Probe,
    options: &SnapshotOptions,
) -> MiscPart {
    let mut part = MiscPart::default();
    let dmv = probe.can_read_dmvs();
    let grant = || probe.grant_hint();
    if dmv {
        let (pressure, waits, read) = pressure::collect(provider.pool.clone(), probe).await;
        part.outcome.merge(read);
        part.waits = waits;
        part.pressure = Some(pressure);
    } else {
        server_state_unreadable(&mut part.outcome, probe);
    }
    let (shared_resources, shared_coverage) =
        crate::resource_counters::collect(provider.pool.clone(), probe, options.collect_shared_resources).await;
    part.outcome.merge(shared_coverage);
    if let Some(counters) = shared_resources {
        part.pressure.get_or_insert_with(Default::default).resource_counters = Some(counters);
    }
    let (request_memory, request_coverage) = crate::request_memory::collect(
        provider.pool.clone(),
        options.collect_request_memory,
        provider.endpoint_host.clone(),
        provider.endpoint_port,
    )
    .await;
    part.outcome.merge(request_coverage);
    if let Some(grants) = request_memory {
        part.pressure.get_or_insert_with(Default::default).request_memory = Some(grants);
    }
    if dmv {
        // The index advisor and tuning checks: each adds detail, and a failure is a note.
        let out = &mut part.outcome;
        let query_store = probe.query_store_readable();
        let mut reader = crate::query_store::Reader::open(provider.pool.clone());
        let families =
            advice::query_families(client, options.window_in_minutes(), query_store.then_some(&mut reader)).await;
        if let Some(read) = out.read(&sources::QUERY_FAMILIES, families) {
            out.completed(&sources::QUERY_FAMILIES, read.families.len());
            if let Some(error) = read.cache_failure {
                out.partial(&sources::QUERY_FAMILIES, error.message_source());
            }
            part.query_families = read.families;
        }
        part.plan_cache = crate::cache_summary::collect(provider.pool.clone(), probe, out).await;
        let tuning = crate::query_store::tuning::read(&mut reader).await;
        let shutdown = reader.close().await;
        part.tuning = tuning.finish(out, &probe.grant_hint(), shutdown, query_store);
    }
    let query_store_options = probe.query_store.as_ref().map(|qs| &qs.options);
    part.settings = crate::settings::collect(
        provider.pool.clone(),
        probe.is_azure_sql_database(),
        query_store_options,
        &mut part.outcome,
        &grant(),
    )
    .await;
    part
}

impl MssqlProvider {
    /// Runs `f` on its own pooled session so collector groups proceed concurrently.
    async fn on_session<T, F, Fut>(&self, f: F) -> Result<T>
    where
        F: FnOnce(ReadSession) -> Fut,
        Fut: Future<Output = (T, ReadSession)>,
    {
        let (value, mut reader) = f(ReadSession::application(self.pool.clone())).await;
        reader.finish(Ok(value)).await
    }
}

/// Independent source failures become coverage; unconfirmed native shutdown fails the scan.
pub(crate) async fn snapshot(
    provider: &MssqlProvider,
    connection: &str,
    options: &SnapshotOptions,
) -> Result<Snapshot> {
    let started = Instant::now();
    let taken_at = chrono::Utc::now();
    let probe = provider.pool.observe(|c| Box::pin(probe::run(c))).await?;
    let probe_ref = &probe;

    let (workload_part, schema_part, misc_part, resource_part) = tokio::join!(
        collect_workload(provider, probe_ref, options),
        provider.on_session(move |mut c| async move {
            let part = collect_schema(&mut c, probe_ref).await;
            (part, c)
        }),
        provider.on_session(move |mut c| async move {
            let part = collect_misc(&mut c, provider, probe_ref, options).await;
            (part, c)
        }),
        collect_resources(provider, probe_ref),
    );
    let schema_part = schema_part?;
    let mut misc_part = misc_part?;
    provider.pool.ensure_available()?;

    let mut outcome = Outcome::default();
    let (expected_have, expected_missing) = probe::expected_capabilities(&probe);
    for capability in [Capability::Showplan, Capability::Sessions] {
        if expected_have.contains(&capability) {
            outcome.delivered(capability);
        }
    }
    for m in expected_missing {
        if matches!(m.capability, Capability::Showplan | Capability::Sessions) {
            outcome.missing_capability(m);
        }
    }
    outcome.merge(workload_part.outcome);
    outcome.merge(schema_part.outcome);
    outcome.merge(misc_part.outcome);
    outcome.merge(resource_part.outcome);
    if !resource_part.signals.is_empty() {
        misc_part.pressure.get_or_insert_with(Default::default).signals.extend(resource_part.signals);
    }
    let sources = outcome.outcomes();
    let (have, missing, notes) = outcome.finish();

    let waits = if workload_part.waits.is_empty() { misc_part.waits } else { workload_part.waits };
    let tables = schema_part.tables;
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
        missing_indexes: schema_part.missing_indexes,
        statistics: schema_part.statistics,
        waits,
        resources: resource_part.samples,
        settings: misc_part.settings,
        pressure: misc_part.pressure,
        notes,
        options: Some(options.clone()),
        query_families: misc_part.query_families,
        plan_cache: misc_part.plan_cache,
        tuning: misc_part.tuning,
        deadlocks: None,
        constraints: schema_part.constraints,
        histograms: None,
        escalations_before: None,
        contention_before: None,
        evictions_before: None,
        sizes_before: None,
        operations: Default::default(),
    };
    let mut client = ReadSession::application(provider.pool.clone());
    signals::collect(&mut client, provider.pool.clone(), &probe, options, &mut snapshot).await;
    if client.close().await == culprit_core::telemetry::SourceShutdown::Unconfirmed {
        return Err(culprit_core::telemetry::CollectionStop::CancellationUnconfirmed.error());
    }
    operations::collect(provider.pool.clone(), &probe, &mut snapshot).await;
    provider.pool.ensure_available()?;
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
    fn limited_catalog_visibility_cannot_turn_empty_sources_into_proven_absence_or_hide_a_failure() {
        let mut coverage = culprit_core::Coverage::default();
        for source in crate::sources::SCHEMA {
            coverage.read(source, Ok(()));
            coverage.completed(source, 0);
        }
        coverage.source_failed(&crate::sources::INDEXES, &crate::catalog_visibility::unavailable());
        let failed_reason = coverage
            .outcomes()
            .into_iter()
            .find(|outcome| outcome.source == crate::sources::INDEXES.name)
            .unwrap()
            .reason;
        super::restrict_catalog_scope(&mut coverage);
        for outcome in coverage.outcomes() {
            assert_eq!(outcome.state, culprit_core::SourceState::Partial, "{}", outcome.source);
            if outcome.source == crate::sources::INDEXES.name {
                assert_eq!(outcome.reason, failed_reason, "limited visibility cannot replace a recorded failure");
            }
        }
    }

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
    #[tokio::test]
    async fn a_shared_resource_read_populates_both_snapshot_consumers_and_coverage() {
        use crate::resources::{
            self,
            test_support::{FakeRead, row},
        };
        let mut io = FakeRead::new(vec![Ok(vec![row("2020-01-01T06:00:15Z")])]);
        let samples = resources::retained(&mut io).await.map_err(crate::read::ReadError::into_error).unwrap();
        let part = super::resource_part(Ok(samples), true, "GRANT VIEW DATABASE STATE TO [monitor];");
        assert_eq!(part.samples.len(), 1);
        assert_eq!(part.samples[0].memory_pct, None);
        assert_eq!(part.signals.len(), 1);
        assert_eq!(part.signals[0].key, culprit_core::signal_id::DATA_IO);
        assert!(part.outcome.has(culprit_core::Capability::ResourceUsage));
        let outcomes = part.outcome.outcomes();
        assert_eq!(outcomes.len(), 2);
        assert!(outcomes.iter().all(|outcome| outcome.state == culprit_core::SourceState::Collected));
        assert_eq!(io.calls.len(), 1);
    }
    #[test]
    fn failed_resource_reads_lock_only_resource_capabilities_and_retain_the_exact_grant() {
        let grant = "GRANT VIEW DATABASE STATE TO [monitor];";
        let part = super::resource_part(
            Err(culprit_core::Error::new(culprit_core::ErrorCode::PermissionDenied, "fixture denied")),
            true,
            grant,
        );
        assert!(part.samples.is_empty());
        assert!(part.signals.is_empty());
        assert!(part.outcome.outcomes().iter().all(|outcome| outcome.state == culprit_core::SourceState::Unavailable));
        let (have, missing, notes) = part.outcome.finish();
        assert!(have.is_empty());
        assert_eq!(missing.len(), 1);
        assert_eq!(missing[0].capability, culprit_core::Capability::ResourceUsage);
        assert_eq!(missing[0].fix.as_deref(), Some(grant));
        assert_eq!(notes.len(), 2);
    }
}

#[cfg(test)]
mod native_tests;
