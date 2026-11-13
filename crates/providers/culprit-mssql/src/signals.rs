//! Signals beyond time and reads: failed and cancelled runs, compile time and log volume per query (Query Store);
//! storage latency per file, logins, availability replicas; lock and latch waits per index; constraints SQL Server
//! doesn't trust. Reads use owned native sessions; a source failure becomes a note.

use std::collections::HashMap;

use culprit_core::{Coverage, Pressure, Signal, SignalScope, SignalUnit, Snapshot, SnapshotOptions};

use crate::probe::Probe;
use crate::query_store::Reader;
use crate::query_store::costs::Compilation;
use crate::read::NativeRead;
use crate::sources;

#[cfg(test)]
#[path = "signals/runtime_tests.rs"]
mod runtime_tests;

/// Collect every signal into `snapshot`, each source read through `sources::read`: these add detail and lock nothing,
/// so a failure is a note.
pub(crate) async fn collect(
    client: &mut impl NativeRead,
    pool: std::sync::Arc<crate::conn::Pool>,
    probe: &Probe,
    options: &SnapshotOptions,
    snapshot: &mut Snapshot,
) {
    let mut coverage = Coverage::default();
    if let Some(workload) =
        snapshot.workload.as_mut().filter(|workload| workload.kind() == Some(culprit_core::WorkloadKind::QueryStore))
    {
        let compiles = collect_query_compilations(
            Reader::open(pool.clone()),
            options.window_in_minutes(),
            &mut workload.queries,
            &mut coverage,
        )
        .await;
        culprit_core::signals::fill_row_bytes(&mut workload.queries);
        snapshot.pressure.get_or_insert_with(Pressure::default).signals.extend(compiles);
    }
    if probe.can_read_dmvs() {
        crate::index_operations::collect(pool.clone(), probe, snapshot).await;
    }
    attach_table_metadata(client, snapshot, &mut coverage).await;
    let pressure = snapshot.pressure.get_or_insert_with(Pressure::default);
    if probe.can_read_dmvs() {
        let hashes: Vec<String> = snapshot
            .workload
            .as_ref()
            .into_iter()
            .flat_map(|w| &w.queries)
            .filter_map(|q| q.query_hash.clone())
            .collect();
        if !hashes.is_empty()
            && let Some(grants) = crate::grants::collect_application(
                pool.clone(),
                probe,
                &mut coverage,
                crate::grants::ApplicationSelection::Hashes(&hashes),
            )
            .await
        {
            for grant in grants {
                pressure.grants.retain(|g| g.query_hash != grant.query_hash);
                pressure.grants.push(grant);
            }
        }
        let azure = probe.is_azure_sql_database();
        let files = files(pool.clone(), azure).await.map_err(|error| {
            if error.code == culprit_core::ErrorCode::PermissionDenied {
                error.with_hint(probe.grant_hint())
            } else {
                error
            }
        });
        if let Some(files) = coverage.read_rows(&sources::FILE_LATENCY, files) {
            pressure.files = files;
        }
    }
    crate::replication::collect(pool, probe, snapshot).await;
    coverage.append_to(snapshot);
}

// ── per query ──────────────────────────────────────────────────────────────────────────────────────

/// An exact database-scoped count, not a ratio against the instance's batches or an estimate for a runtime window.
async fn database_compiles(reader: &mut Reader<impl NativeRead>) -> culprit_core::Result<Vec<Signal>> {
    let rows = reader.read_rows(&crate::query_store::costs::database_compilation_sql()).await?;
    let Some(r) = rows.first() else { return Ok(Vec::new()) };
    let (Some(compiles), Some(ms)) = (r.f64("compiles"), r.f64("compile_ms")) else { return Ok(Vec::new()) };
    Ok(compile_signals(compiles, ms, &r.string("first_compile"), &r.string("last_compile")))
}

/// Compiles Query Store retains for this database's user queries, and their elapsed time, first compile to `last`.
pub(crate) fn compile_signals(compiles: f64, ms: f64, first: &str, last: &str) -> Vec<Signal> {
    let context = culprit_core::t!("provider.mssql.signal.compiles.context", first = first, last = last);
    vec![
        Signal {
            healthy: Some(context.clone()),
            ..Signal::new(
                culprit_core::signal_id::DATABASE_COMPILES,
                culprit_core::message!("provider.mssql.signal.compiles_recorded"),
                compiles,
                SignalUnit::Count,
                SignalScope::QueryHistory,
            )
        },
        Signal {
            healthy: Some(context),
            ..Signal::new(
                culprit_core::signal_id::DATABASE_COMPILE_TIME,
                culprit_core::message!("provider.mssql.signal.compile_time"),
                ms,
                SignalUnit::Milliseconds,
                SignalScope::QueryHistory,
            )
        },
    ]
}

/// Compilation lifetimes stay independent of runtime sampling and publish only after their own confirmed shutdown.
async fn collect_query_compilations<R: NativeRead>(
    mut reader: Reader<R>,
    window_minutes: u32,
    queries: &mut [culprit_core::QueryStat],
    coverage: &mut Coverage,
) -> Vec<Signal> {
    let ids: Vec<String> = queries.iter().map(|query| query.id.clone()).collect();
    let compilations = match reader.compilations(&ids).await {
        Err(error) if crate::read::stops_remaining_reads(&error) => {
            let _ = reader.close().await;
            coverage.source_failed(&sources::QUERY_COMPILATION, &error);
            return vec![];
        }
        result => result,
    };
    let database = database_compiles(&mut reader).await;
    if reader.close().await == culprit_core::telemetry::SourceShutdown::Unconfirmed {
        let error = culprit_core::telemetry::CollectionStop::CancellationUnconfirmed.error();
        coverage.source_failed(&sources::QUERY_COMPILATION, &error);
        return vec![];
    }
    if let Some(compilations) = coverage.read(&sources::QUERY_COMPILATION, compilations) {
        attach_compilations(queries, compilations, window_minutes);
    }
    coverage.read(&sources::QUERY_COMPILATION, database).unwrap_or_default()
}

/// Runtime ratios stay in their original native owner and interval population. A later counter value is not the
/// earlier query's execution denominator, and a recycled query ID cannot borrow its SQL or costs.
pub(crate) async fn attach_runtime_costs<R: NativeRead>(
    reader: &mut Reader<R>,
    window_minutes: u32,
    queries: &mut [culprit_core::QueryStat],
    definitions: &HashMap<String, crate::query_store::definitions::Definition>,
    coverage: &mut Coverage,
) -> culprit_core::Result<()> {
    let ids = queries.iter().map(|query| query.id.clone()).collect::<Vec<_>>();
    let captured = match reader.costs(window_minutes, &ids).await {
        Ok(rows) => rows,
        Err(error) if crate::read::stops_remaining_reads(&error) => return Err(error),
        Err(error) => {
            coverage.source_failed(&sources::QUERY_COSTS, &error);
            return Ok(());
        }
    };
    coverage.completed(&sources::QUERY_COSTS, captured.len());
    let by_id = captured.into_iter().map(|row| (row.query_id.clone(), row)).collect::<HashMap<_, _>>();
    let mut changed_counts = false;
    const MAX_EXACT_COUNT: u64 = 1 << 53;
    for query in queries {
        let Some(row) = by_id.get(&query.id) else {
            return Err(reader.source_changed());
        };
        let Some(expected) = definitions.get(&query.id).and_then(|definition| definition.query.as_ref()) else {
            changed_counts = true;
            continue;
        };
        if &row.identity != expected || (row.executions as f64) < query.executions {
            return Err(reader.source_changed());
        }
        if row.executions > MAX_EXACT_COUNT || row.executions as f64 != query.executions {
            changed_counts = true;
            continue;
        }
        let mut costs = query.costs.take().unwrap_or_default();
        costs.aborted = Some(row.aborted as f64);
        costs.failed = Some(row.failed as f64);
        costs.aborted_mean_ms =
            (row.aborted > 0).then(|| crate::query_store::runtime::milliseconds(row.aborted_us) / row.aborted as f64);
        costs.log_bytes = row.log_bytes;
        // Both aggregates share one native statement and frozen population; selected groups are bounded by IDs.
        costs.log_share = match (row.log_bytes, row.total_log_bytes) {
            (Some(own), Some(all)) if all > 0.0 => Some((own / all).min(1.0)),
            _ => None,
        };
        query.costs = (!costs.is_empty()).then_some(costs);
    }
    if changed_counts {
        coverage.partial(
            &sources::QUERY_COSTS,
            culprit_core::telemetry::CollectionStop::SourceChanged.error().message_source(),
        );
    }
    Ok(())
}

/// Compilation history keeps its own lifetime even when runtime costs are unavailable.
fn attach_compilations(queries: &mut [culprit_core::QueryStat], compilations: Vec<Compilation>, window_minutes: u32) {
    let compiles = compilations.into_iter().map(|row| (row.query_id.clone(), row)).collect::<HashMap<_, _>>();
    for q in queries {
        let mut costs = q.costs.take().unwrap_or_default();
        let compiled = compiles.get(&q.id);
        costs.plan_ms = None;
        costs.plans = None;
        if let Some(c) = compiled {
            let within_window = c.within_window(window_minutes);
            costs.compile_history = Some(c.history(window_minutes));
            if within_window {
                costs.plan_ms = Some(c.total_ms());
                costs.plans = Some(c.count as f64);
            }
        }
        q.costs = (!costs.is_empty()).then_some(costs);
    }
}

/// Foreign keys and check constraints loaded or re-enabled WITH NOCHECK, or switched off. NOT FOR REPLICATION foreign
/// keys are always untrusted and checking them can't change that, so they're left out.
/// Active user triggers and partition range bounds per table (best-effort catalog reads).
async fn attach_table_metadata(client: &mut impl NativeRead, snapshot: &mut Snapshot, coverage: &mut Coverage) {
    if snapshot.tables.is_empty() {
        return;
    }
    let positions: HashMap<_, _> = snapshot
        .tables
        .iter()
        .enumerate()
        .map(|(index, table)| ((table.schema.clone(), table.name.clone()), index))
        .collect();
    let result = crate::triggers::application(client).await;
    if let Err(error) = &result {
        coverage.partial(&sources::TABLES, error.message_source());
    }
    let Some(triggers) = coverage.read_rows(&sources::TRIGGERS, result) else { return };
    let mut incomplete = false;
    for trigger in triggers {
        incomplete |= !trigger.event_metadata_complete();
        let events = crate::triggers::event_codes(&trigger);
        if let Some(index) = positions.get(&(trigger.parent_schema, trigger.parent_name)) {
            snapshot.tables[*index]
                .storage
                .get_or_insert_with(culprit_core::TableStorage::default)
                .options
                .push(format!("trigger={}:{events}", trigger.name));
        } else {
            incomplete = true;
        }
    }
    if incomplete {
        let error = crate::catalog_visibility::unavailable();
        coverage.partial(&sources::TRIGGERS, error.message_source());
        coverage.partial(&sources::TABLES, error.message_source());
    }
    let result = crate::partitions::application(client).await;
    if let Err(error) = &result {
        coverage.partial(&sources::TABLES, error.message_source());
    }
    let Some(boundaries) = coverage.read_rows(&sources::PARTITIONS, result) else { return };
    let mut incomplete = false;
    for boundary in boundaries {
        let Some(option) = crate::partitions::application_bound(&boundary) else { continue };
        let table = &boundary.definition;
        if let Some(index) = positions.get(&(table.schema.clone(), table.table_name.clone())) {
            snapshot.tables[*index]
                .storage
                .get_or_insert_with(culprit_core::TableStorage::default)
                .options
                .push(option);
        } else {
            incomplete = true;
        }
    }
    if incomplete {
        let error = crate::catalog_visibility::unavailable();
        coverage.partial(&sources::PARTITIONS, error.message_source());
        coverage.partial(&sources::TABLES, error.message_source());
    }
}

// ── per server ─────────────────────────────────────────────────────────────────────────────────────

async fn files(
    pool: std::sync::Arc<crate::conn::Pool>,
    azure: bool,
) -> culprit_core::Result<Vec<culprit_core::FileIo>> {
    let mut reader = crate::read::ReadSession::new(pool, culprit_core::telemetry::CollectionBudget::default());
    let selection = if azure {
        crate::storage_io::Selection::Database
    } else {
        crate::storage_io::Selection::DatabaseAndSharedTempdb
    };
    let rows = crate::storage_io::read(&mut reader, selection, 10_000).await;
    if reader.close().await == culprit_core::telemetry::SourceShutdown::Unconfirmed {
        return Err(culprit_core::telemetry::CollectionStop::CancellationUnconfirmed.error());
    }
    rows.map(|rows| crate::storage_io::display(&rows))
}

/// The login counters count every database on the instance. On Azure SQL Database that instance is the server
/// process behind the database, shared by its elastic pool: shown for context, labelled as the whole server's and
/// never graded, since another database's logins would turn this one red.
pub(crate) fn login_signals(
    logins: Option<f64>,
    resets: Option<f64>,
    uptime_s: Option<f64>,
    shared: bool,
) -> Vec<Signal> {
    let whole = if shared { "server" } else { "other" };
    let grade = |signal: Signal, healthy: String, watch: f64, bad: f64, higher_is_worse: bool| {
        if shared {
            Signal { healthy: Some(culprit_core::t!("provider.mssql.signal.shared")), ..signal }
        } else {
            signal.graded(&healthy, watch, bad, higher_is_worse)
        }
    };
    let mut out = Vec::new();
    if let (Some(logins), Some(seconds)) = (logins, uptime_s) {
        out.push(grade(
            Signal::new(
                culprit_core::signal_id::NEW_CONNECTIONS,
                culprit_core::message!("provider.mssql.signal.new_connections", whole = whole),
                logins / seconds,
                SignalUnit::PerSecond,
                SignalScope::SinceStartup,
            ),
            culprit_core::t!("provider.mssql.signal.new_connections.healthy"),
            5.0,
            25.0,
            true,
        ));
        if let Some(resets) = resets
            && logins + resets >= 1_000.0
        {
            out.push(grade(
                Signal::new(
                    culprit_core::signal_id::POOLED_REUSE,
                    culprit_core::message!("provider.mssql.signal.pooled_reuse", whole = whole),
                    resets / (logins + resets) * 100.0,
                    SignalUnit::Percent,
                    SignalScope::SinceStartup,
                ),
                culprit_core::t!("provider.mssql.signal.pooled_reuse.healthy"),
                90.0,
                50.0,
                false,
            ));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        resources::test_support::FakeRead,
        value::{Rec, Val},
    };

    fn query_without_costs() -> culprit_core::QueryStat {
        let snapshot: Snapshot = serde_json::from_str(include_str!("../../../../fixtures/mssql-demo.json")).unwrap();
        let mut query = snapshot.workload.unwrap().queries.remove(0);
        query.id = "3".into();
        query.costs = None;
        query
    }

    fn database_compilation_row() -> Rec {
        Rec::of(&[
            ("compiles", Val::Float(5.0)),
            ("compile_ms", Val::Float(6000.0)),
            ("first_compile", Val::Text("2020-01-01T06:00:00Z".into())),
            ("last_compile", Val::Text("2020-01-01T06:01:00Z".into())),
        ])
    }

    #[tokio::test]
    async fn query_compilation_is_independent_and_never_survives_unconfirmed_native_shutdown() {
        for shutdown in
            [culprit_core::telemetry::SourceShutdown::Quiescent, culprit_core::telemetry::SourceShutdown::Unconfirmed]
        {
            let mut io = FakeRead::new(vec![
                Ok(vec![crate::query_store::tests::compilation_row()]),
                Ok(vec![database_compilation_row()]),
            ]);
            io.shutdown = shutdown;
            let mut queries = vec![query_without_costs()];
            let mut coverage = Coverage::default();
            let signals = collect_query_compilations(Reader::new(io), 60, &mut queries, &mut coverage).await;
            if shutdown == culprit_core::telemetry::SourceShutdown::Unconfirmed {
                assert!(queries[0].costs.is_none(), "unsettled native work cannot publish fresh per-query evidence");
                assert!(signals.is_empty());
                assert!(
                    coverage.outcomes().iter().all(|outcome| outcome.state == culprit_core::SourceState::Unavailable)
                );
            } else {
                let costs = queries[0].costs.as_ref().unwrap();
                assert!(costs.failed.is_none() && costs.aborted.is_none());
                assert_eq!(costs.compile_history.as_ref().unwrap().memory.unwrap().last_kb, Some(8192.0));
                assert_eq!(signals.len(), 2);
            }
        }
    }

    #[test]
    fn only_compiles_wholly_in_the_selected_period_are_compared_with_its_runs() {
        let c = Compilation {
            query_id: "1".into(),
            count: 1500,
            avg_duration_us: 20_000.0,
            memory: None,
            seconds_since_first: 3600,
            first_at: "2020-01-01T00:00:00Z".into(),
            last_at: "2020-01-01T01:00:00Z".into(),
        };
        assert!(c.within_window(60));
        assert_eq!(c.total_ms(), 30_000.0);
        assert!(!c.within_window(30));
        assert!(!Compilation { seconds_since_first: -1, ..c }.within_window(60));
    }

    // On Azure SQL Database the login counters are the server process's, shared by the pool: never graded.
    #[test]
    fn azure_logins_are_the_whole_servers_and_not_graded() {
        let shared = login_signals(Some(3_600_000.0), Some(10.0), Some(3_600.0), true);
        assert_eq!(shared[0].label, "New connections (whole server)");
        assert_eq!(shared[0].status, culprit_core::SignalStatus::Info);
        let own = login_signals(Some(3_600_000.0), Some(10.0), Some(3_600.0), false);
        assert_eq!(own[0].status, culprit_core::SignalStatus::Bad);
    }
    #[tokio::test]
    async fn failed_table_detail_reads_preserve_tables_and_report_partial_coverage() {
        use crate::{read::ReadError, resources::test_support::FakeRead};
        for failed_read in [1, 2] {
            let mut snapshot: Snapshot =
                serde_json::from_str(include_str!("../../../../fixtures/mssql-demo.json")).unwrap();
            let table_count = snapshot.tables.len();
            let mut replies = Vec::new();
            if failed_read == 2 {
                replies.push(Ok(vec![]));
            }
            replies.push(Err(ReadError::Stopped(culprit_core::telemetry::CollectionStop::TimedOut)));
            let mut reader = FakeRead::new(replies);
            let mut coverage = Coverage::default();
            coverage.completed(&sources::TABLES, table_count);
            attach_table_metadata(&mut reader, &mut snapshot, &mut coverage).await;
            assert_eq!(snapshot.tables.len(), table_count);
            assert_eq!(reader.calls.len(), failed_read);
            let outcomes = coverage.outcomes();
            let tables = outcomes.iter().find(|outcome| outcome.source == sources::TABLES.name).unwrap();
            assert_eq!(tables.state, culprit_core::SourceState::Partial);
            assert!(tables.reason.is_some());
        }
    }
    #[tokio::test]
    async fn trigger_metadata_uses_exact_native_names_and_preserves_colons() {
        use crate::resources::test_support::FakeRead;
        let mut snapshot: Snapshot =
            serde_json::from_str(include_str!("../../../../fixtures/mssql-demo.json")).unwrap();
        let mut upper = snapshot.tables[0].clone();
        upper.schema = "dbo".into();
        upper.name = "Items".into();
        upper.storage = None;
        let mut lower = upper.clone();
        lower.name = "items".into();
        snapshot.tables = vec![upper, lower];
        let mut reader = FakeRead::new(vec![Ok(vec![crate::triggers::tests::row()]), Ok(vec![])]);
        let mut coverage = Coverage::default();
        coverage.completed(&sources::TABLES, 2);
        attach_table_metadata(&mut reader, &mut snapshot, &mut coverage).await;
        assert!(snapshot.tables[0].storage.is_none());
        assert_eq!(snapshot.tables[1].storage.as_ref().unwrap().triggers(), [("tenant:audit", "IU")]);
        assert_eq!(reader.calls.len(), 2);
        assert_eq!(
            coverage.outcomes().iter().find(|source| source.source == sources::TRIGGERS.name).unwrap().state,
            culprit_core::SourceState::Collected
        );
    }
}
