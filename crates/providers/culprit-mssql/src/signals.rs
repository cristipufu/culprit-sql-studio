//! Signals beyond time and reads: failed and cancelled runs, compile time and log volume per query (Query Store);
//! storage latency per file, logins, availability replicas; lock and latch waits per index; constraints SQL Server
//! doesn't trust. Reads use owned native sessions; a source failure becomes a note.

use std::collections::HashMap;

use culprit_core::{Coverage, Pressure, Signal, SignalScope, SignalUnit, Snapshot, SnapshotOptions};

use crate::probe::Probe;
use crate::query_store::Reader;
#[cfg(test)]
use crate::query_store::costs::Compilation;
use crate::read::NativeRead;
use crate::sources;

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
    let from_query_store =
        snapshot.workload.as_ref().is_some_and(|w| w.kind() == Some(culprit_core::WorkloadKind::QueryStore));
    if from_query_store {
        let mut reader = Reader::open(pool.clone());
        if let Some(workload) = snapshot.workload.as_mut() {
            let costs = attach_costs(&mut reader, options.window_in_minutes(), &mut workload.queries).await;
            coverage.read(&sources::QUERY_COSTS, costs);
            culprit_core::signals::fill_row_bytes(&mut workload.queries);
        }
        let compiles = database_compiles(&mut reader).await;
        let shutdown = reader.close().await;
        let compiles = if shutdown == culprit_core::telemetry::SourceShutdown::Unconfirmed {
            Err(culprit_core::telemetry::CollectionStop::CancellationUnconfirmed.error())
        } else {
            compiles
        };
        let compiles = coverage.read(&sources::QUERY_COMPILATION, compiles);
        snapshot.pressure.get_or_insert_with(Pressure::default).signals.extend(compiles.unwrap_or_default());
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
async fn database_compiles(reader: &mut Reader) -> culprit_core::Result<Vec<Signal>> {
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

/// Aborted (3) and exception (4) runs, their log bytes, and compile time from Query Store.
async fn attach_costs(
    reader: &mut Reader,
    window_minutes: u32,
    queries: &mut [culprit_core::QueryStat],
) -> culprit_core::Result<()> {
    let ids: Vec<String> = queries.iter().map(|query| query.id.clone()).collect();
    if ids.is_empty() {
        return Ok(());
    }
    let rows = reader.costs(window_minutes, &ids).await?;
    let total_log = reader.total_log_bytes(window_minutes).await?;
    let by_id = rows.into_iter().map(|row| (row.query_id.clone(), row)).collect::<HashMap<_, _>>();
    let compiles =
        reader.compilations(&ids).await?.into_iter().map(|row| (row.query_id.clone(), row)).collect::<HashMap<_, _>>();

    for q in queries.iter_mut() {
        let mut costs = q.costs.take().unwrap_or_default();
        if let Some(r) = by_id.get(&q.id) {
            let aborted = r.aborted as f64;
            costs.aborted = Some(aborted);
            costs.failed = Some(r.failed as f64);
            costs.aborted_mean_ms =
                (aborted > 0.0).then(|| crate::query_store::runtime::milliseconds(r.aborted_us) / aborted);
            costs.log_bytes = r.log_bytes;
            costs.log_share = match (costs.log_bytes, total_log) {
                (Some(own), Some(all)) if all > 0.0 => Some((own / all).min(1.0)),
                _ => None,
            };
        }
        let compiled = compiles.get(&q.id);
        costs.plan_ms = None;
        costs.plans = None;
        if let Some(c) = compiled {
            let within_window = c.within_window(window_minutes);
            costs.compile_history = Some(culprit_core::signals::CompileHistory {
                count: c.count as f64,
                total_ms: c.total_ms(),
                first_at: Some(c.first_at.clone()),
                last_at: Some(c.last_at.clone()),
                within_window,
            });
            if within_window {
                costs.plan_ms = Some(c.total_ms());
                costs.plans = Some(c.count as f64);
            }
        }
        q.costs = (!costs.is_empty()).then_some(costs);
    }
    Ok(())
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

    #[test]
    fn only_compiles_wholly_in_the_selected_period_are_compared_with_its_runs() {
        let c = Compilation {
            query_id: "1".into(),
            count: 1500,
            avg_duration_us: 20_000.0,
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
