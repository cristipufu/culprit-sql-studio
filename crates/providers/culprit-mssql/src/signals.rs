//! Signals beyond time and reads: failed and cancelled runs, compile time and log volume per query (Query Store);
//! storage latency per file, logins, availability replicas; lock and latch waits per index; constraints SQL Server
//! doesn't trust. Reads use owned native sessions; a source failure becomes a note.

use std::collections::HashMap;

use culprit_core::telemetry::NativeQueryIdentity;
use culprit_core::{
    Coverage, Pressure, QueryStat, QueryStoreScope, Signal, SignalScope, SignalUnit, Snapshot, SnapshotOptions,
    WorkloadKind,
};

use crate::probe::Probe;
use crate::query_store::Reader;
use crate::query_store::costs::Compilation;
use crate::read::NativeRead;
use crate::sources;

#[cfg(test)]
#[path = "signals/compilation_tests.rs"]
mod compilation_tests;

/// Each original workload keeps its native query incarnation and runtime population. Borrowing participants
/// avoids a pooled workload and permits one independent native acquisition for every selected population.
pub(crate) struct QueryParticipant<'a> {
    pub query: &'a mut QueryStat,
    pub identity: Option<&'a NativeQueryIdentity>,
    pub kind: Option<WorkloadKind>,
    pub runtime_scope: QueryStoreScope,
}

/// Collect every signal into `snapshot`, each source read through `sources::read`: these add detail and lock nothing,
/// so a failure is a note.
pub(crate) async fn collect(
    client: &mut impl NativeRead,
    provider: &crate::MssqlProvider,
    probe: &Probe,
    options: &SnapshotOptions,
    participants: &mut [QueryParticipant<'_>],
    snapshot: &mut Snapshot,
) {
    let pool = provider.pool.clone();
    let mut coverage = Coverage::default();
    snapshot.operations.query_compilation = None;
    if participants.iter().any(|participant| participant.kind == Some(WorkloadKind::QueryStore)) {
        let compiles = collect_query_compilations(
            Reader::open(pool.clone()),
            options.window_in_minutes(),
            participants,
            &mut coverage,
        )
        .await;
        snapshot.operations.query_compilation = compiles.snapshot;
        for participant in
            participants.iter_mut().filter(|participant| participant.kind == Some(WorkloadKind::QueryStore))
        {
            culprit_core::signals::fill_row_bytes(std::slice::from_mut(participant.query));
        }
        snapshot.pressure.get_or_insert_with(Pressure::default).signals.extend(compiles.signals);
    }
    if probe.can_read_dmvs() {
        crate::index_operations::collect(pool.clone(), probe, snapshot).await;
    }
    attach_table_metadata(client, snapshot, &mut coverage).await;
    let pressure = snapshot.pressure.get_or_insert_with(Pressure::default);
    if probe.can_read_dmvs() {
        // These native hash selectors collect shared cache candidates once. They do not assign grants or requests
        // to a Query Store population, even when that population contains the same statement hash.
        let hashes = participant_hashes(participants);
        if let Some(grants) = crate::grants::collect_application(provider, probe, &mut coverage, &hashes).await {
            pressure.grants.push(grants);
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

fn participant_hashes(participants: &[QueryParticipant<'_>]) -> Vec<String> {
    participants
        .iter()
        .filter_map(|participant| participant.query.query_hash.clone())
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect()
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

#[derive(Default)]
struct CompilationRead {
    signals: Vec<Signal>,
    snapshot: Option<culprit_core::telemetry::CompilationSnapshot>,
}

/// Compilation lifetimes stay independent of runtime sampling and publish only after their own confirmed shutdown.
async fn collect_query_compilations<R: NativeRead>(
    mut reader: Reader<R>,
    window_minutes: u32,
    participants: &mut [QueryParticipant<'_>],
    coverage: &mut Coverage,
) -> CompilationRead {
    let observed_from = chrono::Utc::now().to_rfc3339();
    clear_compilations(participants);
    let ids: Vec<String> = participants
        .iter()
        .filter(|participant| participant.kind == Some(WorkloadKind::QueryStore))
        .map(|participant| participant.query.id.clone())
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect();
    let compilations = match reader.compilations(&ids).await {
        Err(error) if crate::read::stops_remaining_reads(&error) => {
            let _ = reader.close().await;
            coverage.source_failed(&sources::QUERY_COMPILATION, &error);
            return CompilationRead::default();
        }
        result => result,
    };
    let database = database_compiles(&mut reader).await;
    if reader.close().await == culprit_core::telemetry::SourceShutdown::Unconfirmed {
        let error = culprit_core::telemetry::CollectionStop::CancellationUnconfirmed.error();
        coverage.source_failed(&sources::QUERY_COMPILATION, &error);
        return CompilationRead::default();
    }
    let observed_until = chrono::Utc::now().to_rfc3339();
    let compilations = coverage.read(&sources::QUERY_COMPILATION, compilations);
    let captured = compilations.as_ref().map(|rows| compilation_snapshot(rows, observed_from, observed_until));
    let snapshot = captured.and_then(|result| coverage.read(&sources::QUERY_COMPILATION, result));
    let complete =
        compilations.is_none_or(|compilations| attach_compilations(participants, compilations, window_minutes));
    let signals = coverage.read(&sources::QUERY_COMPILATION, database).unwrap_or_default();
    if !complete {
        coverage.partial(
            &sources::QUERY_COMPILATION,
            culprit_core::message!("provider.mssql.compilation.identity_unverified"),
        );
    }
    CompilationRead { signals, snapshot }
}

fn compilation_snapshot(
    compilations: &[Compilation],
    observed_from: String,
    observed_until: String,
) -> culprit_core::Result<culprit_core::telemetry::CompilationSnapshot> {
    let snapshot = culprit_core::telemetry::CompilationSnapshot {
        source: sources::QUERY_COMPILATION.name.into(),
        observed_from,
        observed_until,
        observations: compilations.iter().map(Compilation::record).collect::<culprit_core::Result<_>>()?,
    };
    if !snapshot.is_valid() {
        return Err(crate::read::invalid_data());
    }
    Ok(snapshot)
}

/// Compilation history keeps its own lifetime even when runtime costs are unavailable.
fn clear_compilations(participants: &mut [QueryParticipant<'_>]) {
    for participant in participants.iter_mut().filter(|participant| participant.kind == Some(WorkloadKind::QueryStore))
    {
        let q = &mut participant.query;
        let mut costs = q.costs.take().unwrap_or_default();
        costs.plan_ms = None;
        costs.plans = None;
        costs.compile_history = None;
        q.costs = (!costs.is_empty()).then_some(costs);
    }
}

/// Compilation history keeps its own lifetime; only the original native definition can authorize its SQL link.
fn attach_compilations(
    participants: &mut [QueryParticipant<'_>],
    compilations: Vec<Compilation>,
    window_minutes: u32,
) -> bool {
    let compiles = compilations.into_iter().map(|row| (row.identity.query_id.clone(), row)).collect::<HashMap<_, _>>();
    let mut complete = true;
    for participant in participants.iter_mut().filter(|participant| participant.kind == Some(WorkloadKind::QueryStore))
    {
        let q = &mut participant.query;
        if let Some(c) = compiles.get(&q.id)
            && participant.identity == Some(&c.identity)
        {
            let mut costs = q.costs.take().unwrap_or_default();
            let within_window = c.within_window(window_minutes);
            costs.compile_history = Some(c.history(window_minutes));
            if within_window && participant.runtime_scope == QueryStoreScope::Local {
                costs.plan_ms = Some(c.total_ms());
                costs.plans = Some(c.count as f64);
            }
            q.costs = Some(costs);
        } else {
            complete = false;
        }
    }
    complete
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
pub(crate) fn connection_reuse_signals(logins: Option<f64>, resets: Option<f64>, shared: bool) -> Vec<Signal> {
    let whole = if shared { "server" } else { "other" };
    let mut out = Vec::new();
    if let Some((logins, resets)) = logins.zip(resets)
        && logins + resets >= 1_000.0
    {
        out.push(Signal {
            healthy: Some(if shared {
                culprit_core::t!("provider.mssql.signal.shared")
            } else {
                culprit_core::t!("provider.mssql.signal.pooled_reuse.healthy")
            }),
            ..Signal::new(
                culprit_core::signal_id::POOLED_REUSE,
                culprit_core::message!("provider.mssql.signal.pooled_reuse", whole = whole),
                resets / (logins + resets) * 100.0,
                SignalUnit::Percent,
                SignalScope::Cumulative,
            )
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        resources::test_support::{FakeRead, replace},
        value::{Rec, Val},
    };

    pub(super) fn query_without_costs() -> culprit_core::QueryStat {
        let snapshot: Snapshot = serde_json::from_str(include_str!("../../../../fixtures/mssql-demo.json")).unwrap();
        let mut query = snapshot.workload.unwrap().queries.remove(0);
        query.id = "3".into();
        query.costs = None;
        query
    }

    pub(super) fn database_compilation_row() -> Rec {
        Rec::of(&[
            ("compiles", Val::Float(5.0)),
            ("compile_ms", Val::Float(6000.0)),
            ("first_compile", Val::Text("2020-01-01T06:00:00Z".into())),
            ("last_compile", Val::Text("2020-01-01T06:01:00Z".into())),
        ])
    }

    pub(super) fn compilation_identities() -> HashMap<String, NativeQueryIdentity> {
        let identity =
            crate::query_store::decode::query_identity(&crate::query_store::tests::compilation_row()).unwrap();
        [(identity.query_id.clone(), identity)].into()
    }

    pub(super) fn unpartitioned_participants<'a>(
        queries: &'a mut [QueryStat],
        identities: &'a HashMap<String, NativeQueryIdentity>,
    ) -> Vec<QueryParticipant<'a>> {
        queries
            .iter_mut()
            .map(|query| {
                let identity = identities.get(&query.id);
                QueryParticipant {
                    query,
                    identity,
                    kind: Some(WorkloadKind::QueryStore),
                    runtime_scope: QueryStoreScope::Local,
                }
            })
            .collect()
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
            let signals = collect_query_compilations(
                Reader::new(io),
                60,
                &mut unpartitioned_participants(&mut queries, &compilation_identities()),
                &mut coverage,
            )
            .await;
            if shutdown == culprit_core::telemetry::SourceShutdown::Unconfirmed {
                assert!(queries[0].costs.is_none(), "unsettled native work cannot publish fresh per-query evidence");
                assert!(signals.signals.is_empty());
                assert!(signals.snapshot.is_none());
                assert!(
                    coverage.outcomes().iter().all(|outcome| outcome.state == culprit_core::SourceState::Unavailable)
                );
            } else {
                let costs = queries[0].costs.as_ref().unwrap();
                assert!(costs.failed.is_none() && costs.aborted.is_none());
                assert_eq!(costs.compile_history.as_ref().unwrap().memory.unwrap().last_kb, Some(8192.0));
                assert_eq!(signals.signals.len(), 2);
                let captured = signals.snapshot.as_ref().unwrap();
                assert!(captured.is_valid());
                assert_eq!(captured.observations.len(), 1);
            }
        }
    }

    #[test]
    fn only_compiles_wholly_in_the_selected_period_are_compared_with_its_runs() {
        let c = Compilation::decode(&replace(
            &crate::query_store::tests::compilation_row(),
            &[
                ("count_compiles", Val::Int(1500)),
                ("avg_compile_duration", Val::Float(20_000.0)),
                ("compiling_s", Val::Int(3600)),
                ("query_initial_compile_at", Val::Text("2020-01-01T00:00:00Z".into())),
                ("last_compile", Val::Text("2020-01-01T01:00:00Z".into())),
            ],
        ))
        .unwrap();
        assert!(c.within_window(60));
        assert_eq!(c.total_ms(), 30_000.0);
        assert!(!c.within_window(30));
        assert!(!Compilation { seconds_since_first: -1, ..c }.within_window(60));
    }

    // On Azure SQL Database the login counters are the server process's, shared by the pool: never graded.
    #[test]
    fn same_read_pool_reuse_is_independent_of_unknown_lifetime_and_shared_counters_are_not_graded() {
        let shared = connection_reuse_signals(Some(3_600_000.0), Some(10.0), true);
        assert_eq!(shared[0].key, culprit_core::signal_id::POOLED_REUSE);
        assert_eq!(shared[0].label, "Connections served from a pool (whole server)");
        assert_eq!(shared[0].status, culprit_core::SignalStatus::Info);
        let own = connection_reuse_signals(Some(3_600_000.0), Some(10.0), false);
        assert_eq!(own[0].status, culprit_core::SignalStatus::Info);
        assert!(shared.iter().chain(&own).all(|signal| signal.scope == SignalScope::Cumulative));
        assert!(shared.iter().chain(&own).all(|signal| signal.key != culprit_core::signal_id::NEW_CONNECTIONS));
        assert!(connection_reuse_signals(None, Some(10.0), false).is_empty());
        assert!(connection_reuse_signals(Some(1000.0), None, false).is_empty());
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
