//! Pressure signals and the "right now" wait sample: performance counters, wait totals, memory grants and the oldest open
//! transaction. Read-only and tagged; a read that fails leaves its signal out.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use culprit_core::{Coverage, Engine, Pressure, Signal, SignalScope, SignalUnit, WaitSample, WaitStat, format, t};

use crate::probe::Probe;
use crate::read::{NativeRead, ReadError, ReadSession};
use crate::sources;
use crate::waits::{self, WaitTotals};

async fn counters<R: NativeRead>(reader: &mut R) -> culprit_core::Result<HashMap<String, f64>> {
    crate::performance::read(reader, crate::performance::MAX_COUNTERS)
        .await
        .map(|rows| crate::performance::application(&rows))
}

fn uptime_hours(probe: &Probe) -> Option<f64> {
    let started = chrono::DateTime::parse_from_rfc3339(probe.started_at.as_deref()?).ok()?;
    let hours = (chrono::Utc::now() - started.with_timezone(&chrono::Utc)).num_seconds() as f64 / 3600.0;
    (hours > 0.0).then_some(hours)
}

/// What the server's performance counters say. On Azure SQL Database they describe the server process behind the
/// database, which an elastic pool shares among all its databases: shown for context, labelled as the whole server's,
/// and never graded, since another database's scans would turn this one red.
pub(crate) fn counter_signals(c: &HashMap<String, f64>, uptime: Option<f64>, azure: bool) -> Vec<Signal> {
    let shared = || Some(t!("provider.mssql.signal.shared"));
    let whole = if azure { "server" } else { "other" };
    let mut signals = Vec::new();
    let batches = c.get("Batch Requests/sec").copied().unwrap_or(0.0);
    let compilations = c.get("SQL Compilations/sec").copied();
    // SQL Compilations/sec counts recompiles too; they have other causes (changed statistics or schema, temporary
    // tables, OPTION (RECOMPILE)) and parameters don't remove them.
    let recompiles =
        c.get("SQL Re-Compilations/sec").copied().map(|value| compilations.map_or(value, |total| value.min(total)));
    let compiles = compilations.zip(recompiles).map(|(total, repeated)| total - repeated);
    if let Some(hours) = uptime.filter(|_| batches > 0.0) {
        let rate = Signal::new(
            culprit_core::signal_id::BATCH_REQUESTS,
            culprit_core::message!("provider.mssql.signal.batch_requests", whole = whole),
            batches / (hours * 3600.0),
            SignalUnit::PerSecond,
            SignalScope::SinceStartup,
        );
        signals.push(if azure { Signal { healthy: shared(), ..rate } } else { rate });
    }
    if batches > 0.0 {
        if let Some(compiles) = compiles {
            let ratio = Signal::new(
                culprit_core::signal_id::COMPILES_PER_BATCH,
                culprit_core::message!("provider.mssql.signal.plans_compiled", whole = whole),
                compiles / batches * 100.0,
                SignalUnit::Percent,
                SignalScope::SinceStartup,
            );
            // A handful of batches compiling says nothing about plan reuse; grade the ratio only under real load.
            let busy = uptime.is_none_or(|h| batches / (h * 3600.0) >= 1.0);
            signals.push(if azure {
                Signal { healthy: shared(), ..ratio }
            } else if busy {
                ratio.graded(&t!("provider.mssql.signal.plans_compiled.healthy"), 10.0, 25.0, true)
            } else {
                Signal { healthy: Some(t!("provider.mssql.signal.plans_compiled.too_few")), ..ratio }
            });
        }
        if let Some(recompiles) = recompiles {
            let recompiled = Signal::new(
                culprit_core::signal_id::RECOMPILES_PER_BATCH,
                culprit_core::message!("provider.mssql.signal.plans_recompiled", whole = whole),
                recompiles / batches * 100.0,
                SignalUnit::Percent,
                SignalScope::SinceStartup,
            );
            signals.push(if azure { Signal { healthy: shared(), ..recompiled } } else { recompiled });
        }
    }
    if let Some(ple) = c.get("Page life expectancy") {
        let life = Signal::new(
            culprit_core::signal_id::PAGE_LIFE,
            culprit_core::message!("provider.mssql.signal.page_life", whole = whole),
            *ple,
            SignalUnit::Seconds,
            SignalScope::Now,
        );
        signals.push(if azure {
            Signal { healthy: shared(), ..life }
        } else {
            life.graded(&t!("provider.mssql.signal.page_life.healthy"), 300.0, 60.0, false)
        });
    }
    signals
}

/// Azure SQL Database's own figures for this database over the last hour: storage I/O against its tier's limit,
/// plus memory in use, for context. Individual samples establish pressure; a full cache is normal.
pub(crate) fn resource_signals(summary: &crate::resources::Summary) -> Vec<Signal> {
    let mut signals = Vec::new();
    if let (Some(data_io), Some(peak)) = (summary.data_io, summary.data_io_peak) {
        signals.push(Signal {
            healthy: Some(t!(
                "provider.mssql.signal.data_io.healthy",
                peak = format::digits(std::format!("{peak:.1}"))
            )),
            ..Signal::new(
                culprit_core::signal_id::DATA_IO,
                culprit_core::message!("provider.mssql.signal.data_io"),
                data_io,
                SignalUnit::Percent,
                SignalScope::LastHour,
            )
        });
    }
    if let Some(memory) = summary.memory {
        signals.push(Signal {
            healthy: Some(t!("provider.mssql.signal.memory_use.healthy")),
            ..Signal::new(
                culprit_core::signal_id::MEMORY_USE,
                culprit_core::message!("provider.mssql.signal.memory_use"),
                memory,
                SignalUnit::Percent,
                SignalScope::LastHour,
            )
        });
    }
    signals
}

/// Signals, grants and the oldest open transaction. Each read is independent, through `sources::read`: a failure is a
/// note in the coverage returned.
pub(crate) async fn collect(
    pool: std::sync::Arc<crate::conn::Pool>,
    probe: &Probe,
) -> (Pressure, Vec<WaitStat>, Coverage) {
    let azure = probe.is_azure_sql_database();
    let uptime = uptime_hours(probe);
    let mut p = Pressure { uptime_hours: uptime, ..Pressure::default() };
    let mut coverage = Coverage::default();
    let (native_waits, native_counters) = history(pool.clone(), azure).await;
    let permission = |error: culprit_core::Error| {
        if error.code == culprit_core::ErrorCode::PermissionDenied {
            error.with_hint(probe.grant_hint())
        } else {
            error
        }
    };
    let native_waits = native_waits.map_err(permission);
    let native_counters = native_counters.map_err(permission);
    if let Some(counters) = coverage.read(&sources::LOGINS, native_counters.clone()) {
        p.signals.extend(crate::signals::login_signals(
            counters.get("Logins/sec").copied(),
            counters.get("Connection Reset/sec").copied(),
            uptime.map(|hours| hours * 3600.0),
            azure,
        ));
    }
    if let Some(counters) = coverage.read(&sources::PERFORMANCE_COUNTERS, native_counters) {
        coverage.completed(&sources::PERFORMANCE_COUNTERS, counters.len());
        p.signals.extend(counter_signals(&counters, uptime, azure));
    }
    let categories = if let Some(rows) = coverage.read_rows(&sources::WAITS, native_waits.clone()) {
        coverage.delivered(culprit_core::Capability::WaitStats);
        waits::categories(&waits::totals(&rows))
    } else {
        Vec::new()
    };
    if let Some(rows) = coverage.read_rows(&sources::WAIT_TOTALS, native_waits) {
        p.signals.extend(wait_total_signals(&waits::totals(&rows), azure, uptime));
    }
    p.signals.extend(crate::queues::application(pool.clone(), probe, &mut coverage).await);
    p.signals.extend(crate::compiler_gateways::collect(pool.clone(), probe, &mut coverage).await);
    if let Some(topology) = crate::processors::collect(pool.clone(), probe, &mut coverage).await {
        (p.cpus, p.numa_nodes) = topology.application();
    }
    if let Some(grants) = crate::grants::collect_application(
        pool.clone(),
        probe,
        &mut coverage,
        crate::grants::ApplicationSelection::Largest,
    )
    .await
    {
        p.grants = grants;
    }
    let version_store = crate::version_store::application(pool.clone(), probe, &mut coverage).await;
    if let Some(bytes) = version_store.tempdb_bytes {
        p.signals.push(version_store_signal(bytes));
    }
    p.signals.extend(version_store.persistent.as_ref().and_then(crate::version_store::persistent::signal));
    p.oldest_transaction =
        crate::open_transactions::collect(pool, probe, &mut coverage, version_store.tempdb_cleanup_context()).await;
    if let Some(transaction) = &p.oldest_transaction {
        p.signals.push(oldest_transaction_signal(transaction.seconds));
    }
    (p, categories, coverage)
}

/// Both consumers use one native wait snapshot; no second reading can silently describe a different instant.
async fn history(
    pool: std::sync::Arc<crate::conn::Pool>,
    azure: bool,
) -> (
    culprit_core::Result<Vec<culprit_core::telemetry::WaitCounterObservation>>,
    culprit_core::Result<HashMap<String, f64>>,
) {
    let mut reader = ReadSession::new(pool, culprit_core::telemetry::CollectionBudget::default());
    let waits = waits::read(&mut reader, azure, crate::waits::MAX_WAIT_TYPES).await.map_err(ReadError::into_error);
    let counters = counters(&mut reader).await;
    if reader.close().await == culprit_core::telemetry::SourceShutdown::Unconfirmed {
        let error = culprit_core::telemetry::CollectionStop::CancellationUnconfirmed.error();
        return (Err(error.clone()), Err(error));
    }
    (waits, counters)
}

/// Queries waiting for a memory grant at the scan's instant.
pub(crate) fn pending_grants_signal(pending: f64) -> Signal {
    Signal::new(
        culprit_core::signal_id::MEMORY_GRANTS_PENDING,
        culprit_core::message!("provider.mssql.signal.grants_pending"),
        pending,
        SignalUnit::Count,
        SignalScope::Now,
    )
    .graded(&t!("provider.mssql.signal.grants_pending.healthy"), 1.0, 5.0, true)
}

/// What the wait totals since the start say: the scheduling share, and the waits for workers and memory grants.
pub(crate) fn wait_total_signals(totals: &WaitTotals, azure: bool, uptime: Option<f64>) -> Vec<Signal> {
    let mut out = Vec::new();
    let (wait, signal) = totals.values().fold((0.0, 0.0), |(w, s), (tw, ts, _)| (w + tw, s + ts));
    if wait > 0.0 {
        out.push(scheduling_share(signal, wait, SignalScope::SinceStartup));
    }
    let threadpool = totals.get("THREADPOOL").map(|t| t.0).unwrap_or(0.0);
    // Totals since the start: one bad hour days ago and a steady trickle average to the same rate, so checks
    // say how much and since when instead of an hourly rate.
    out.push(Signal::new(
        culprit_core::signal_id::THREADPOOL_WAIT_MS,
        culprit_core::message!(
            "provider.mssql.signal.threadpool_wait_ms",
            whole = if azure { "other" } else { "instance" }
        ),
        threadpool,
        SignalUnit::Milliseconds,
        SignalScope::SinceStartup,
    ));
    // Memory grants alone; the Memory category also holds allocation waits (CMEMTHREAD) and compile memory.
    let grants = totals.get("RESOURCE_SEMAPHORE").map(|t| t.0).unwrap_or(0.0);
    out.push(Signal::new(
        culprit_core::signal_id::GRANT_WAIT_MS,
        culprit_core::message!("provider.mssql.signal.grant_wait_ms", whole = if azure { "other" } else { "instance" }),
        grants,
        SignalUnit::Milliseconds,
        SignalScope::SinceStartup,
    ));
    if let Some(hours) = uptime {
        out.push(
            Signal::new(
                culprit_core::signal_id::THREADPOOL_WAITS,
                culprit_core::message!(
                    "provider.mssql.signal.threadpool_waits",
                    per = if hours >= 1.0 { "hour" } else { "other" }
                ),
                // Under an hour of uptime, report the total rather than extrapolate an hourly rate.
                threadpool / hours.max(1.0),
                SignalUnit::Milliseconds,
                SignalScope::SinceStartup,
            )
            .graded(&t!("provider.mssql.signal.threadpool_waits.healthy"), 1_000.0, 60_000.0, true),
        );
    }
    out
}

/// Requests in this database waiting for a worker at the scan's instant.
pub(crate) fn threadpool_now_signal(waiting: f64) -> Signal {
    Signal::new(
        culprit_core::signal_id::THREADPOOL_WAITING,
        culprit_core::message!("provider.mssql.signal.threadpool_waiting"),
        waiting,
        SignalUnit::Count,
        SignalScope::Now,
    )
    .graded(&t!("provider.mssql.signal.threadpool_waiting.healthy"), 1.0, f64::INFINITY, true)
}

pub(crate) fn compilation_waiting_signal(waiting: f64, minimum_wait_ms: u32) -> Signal {
    Signal {
        measurement_scope: Some(culprit_core::MeasurementScope::Database),
        ..Signal::new(
            culprit_core::signal_id::COMPILATION_WAITING,
            culprit_core::message!(
                "provider.mssql.signal.compilation_waiting",
                minimum = format::duration(f64::from(minimum_wait_ms))
            ),
            waiting,
            SignalUnit::Count,
            SignalScope::Now,
        )
        .graded(
            &t!(
                "provider.mssql.signal.compilation_waiting.context",
                minimum = format::duration(f64::from(minimum_wait_ms))
            ),
            1.0,
            f64::INFINITY,
            true,
        )
    }
}

/// tempdb's version store, which open snapshot transactions keep from shrinking.
pub(crate) fn version_store_signal(bytes: f64) -> Signal {
    Signal::new(
        culprit_core::signal_id::VERSION_STORE,
        culprit_core::message!("provider.mssql.version_store"),
        bytes,
        SignalUnit::Bytes,
        SignalScope::Now,
    )
    .graded(
        &t!("provider.mssql.signal.version_store.healthy"),
        1024.0 * 1024.0 * 1024.0,
        10.0 * 1024.0 * 1024.0 * 1024.0,
        true,
    )
}

/// How long the oldest open transaction has been open.
pub(crate) fn oldest_transaction_signal(seconds: f64) -> Signal {
    Signal::new(
        culprit_core::signal_id::OLDEST_TRANSACTION,
        culprit_core::message!("provider.mssql.signal.oldest_transaction"),
        seconds,
        SignalUnit::Seconds,
        SignalScope::Now,
    )
    .graded(&t!("provider.mssql.signal.oldest_transaction.healthy"), 600.0, 1800.0, true)
}

/// Two readings of wait totals and counters `seconds` apart; the difference is what happened in between.
pub(crate) async fn sample<R: NativeRead>(
    reader: &mut R,
    azure: bool,
    seconds: u32,
) -> culprit_core::Result<WaitSample> {
    let started = Instant::now();
    let taken_at = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let first = waits::read(reader, azure, crate::waits::MAX_WAIT_TYPES).await.map_err(ReadError::into_error)?;
    let counters_before = crate::performance::read(reader, crate::performance::MAX_COUNTERS).await?;
    tokio::time::sleep(Duration::from_secs(u64::from(seconds))).await;
    let second = waits::read(reader, azure, crate::waits::MAX_WAIT_TYPES).await.map_err(ReadError::into_error)?;
    let counters_after = crate::performance::read(reader, crate::performance::MAX_COUNTERS).await?;
    let elapsed = started.elapsed().as_secs_f64().max(1.0);

    let deltas = waits::deltas(&first, &second)?;
    let (wait_total, signal_total) =
        deltas.values().fold((0.0, 0.0), |(wait, signal), (ms, signal_ms, _)| (wait + ms, signal + signal_ms));
    let categories = waits::categories(&deltas);

    let rates = crate::performance::sampled_rates(&counters_before, &counters_after, elapsed);
    let rate = |name: &str| rates.get(name).copied();
    let signals = sampled_signals(rate, signal_total, wait_total, azure);
    let view = if azure { "sys.dm_db_wait_stats" } else { "sys.dm_os_wait_stats" };
    Ok(WaitSample {
        engine: Engine::Mssql,
        taken_at,
        seconds: elapsed,
        method: sample_method(view, seconds),
        total_ms: wait_total,
        categories,
        signals,
        active_sessions: None,
        notes: sample_notes(azure),
    })
}

/// How a sample was taken: two readings of `view`, `seconds` apart.
pub(crate) fn sample_method(view: &str, seconds: u32) -> String {
    t!("provider.mssql.sample.method", view = view, seconds = format::digits(seconds.to_string()))
}

/// What a sample's reader should know: on SQL Server the wait counters are the whole instance's.
pub(crate) fn sample_notes(azure: bool) -> Vec<String> {
    if azure { Vec::new() } else { vec![t!("provider.mssql.sample.instance_wide")] }
}

/// The counters' rates over a sample (`rate` of a counter, per second), and the scheduling share of its waits.
pub(crate) fn sampled_signals(
    rate: impl Fn(&str) -> Option<f64>,
    signal_total: f64,
    wait_total: f64,
    azure: bool,
) -> Vec<Signal> {
    let mut signals = Vec::new();
    if let Some(batches) = rate("Batch Requests/sec") {
        signals.push(Signal::new(
            "batch-requests",
            culprit_core::message!("provider.mssql.signal.batch_requests", whole = "other"),
            batches,
            SignalUnit::PerSecond,
            SignalScope::Sampled,
        ));
        // Recompiles are counted in compilations too; plan reuse is about the first compile.
        if let Some(compiles) = rate("SQL Compilations/sec")
            .zip(rate("SQL Re-Compilations/sec"))
            .map(|(total, repeated)| (total - repeated).max(0.0))
            .filter(|_| batches > 0.0)
        {
            signals.push(Signal::new(
                "compiles-per-batch",
                culprit_core::message!("provider.mssql.signal.plans_compiled", whole = "instance"),
                compiles / batches * 100.0,
                SignalUnit::Percent,
                SignalScope::Sampled,
            ));
        }
    }
    if let Some(recompiles) = rate("SQL Re-Compilations/sec") {
        signals.push(Signal::new(
            "recompiles",
            culprit_core::message!("provider.mssql.signal.recompiles"),
            recompiles,
            SignalUnit::PerSecond,
            SignalScope::Sampled,
        ));
    }
    if wait_total > 0.0 {
        signals.push(scheduling_share(signal_total, wait_total, SignalScope::Sampled));
    }
    if azure {
        signals.retain(|s| !matches!(s.key.as_str(), "batch-requests" | "compiles-per-batch" | "recompiles"));
    }
    signals
}

/// A wait composition, never CPU utilization or a CPU-pressure verdict, even in a short live sample.
fn scheduling_share(signal: f64, wait: f64, scope: SignalScope) -> Signal {
    Signal {
        healthy: Some(t!("provider.mssql.signal.scheduling_share.healthy")),
        ..Signal::new(
            culprit_core::signal_id::SIGNAL_WAIT,
            culprit_core::message!("provider.mssql.signal.scheduling_share"),
            signal / wait * 100.0,
            SignalUnit::Percent,
            scope,
        )
    }
}

#[cfg(test)]
mod tests {
    use culprit_core::SignalStatus;

    use super::*;

    fn counters(ple: f64) -> HashMap<String, f64> {
        HashMap::from([
            ("Batch Requests/sec".to_string(), 7_200_000.0),
            ("SQL Compilations/sec".to_string(), 3_600_000.0),
            ("SQL Re-Compilations/sec".to_string(), 0.0),
            ("Page life expectancy".to_string(), ple),
        ])
    }

    fn signal<'a>(signals: &'a [Signal], key: &str) -> &'a Signal {
        signals.iter().find(|s| s.key == key).expect(key)
    }

    // On a server of its own, a short page life is this server's, and graded.
    #[test]
    fn a_short_page_life_is_graded_on_a_server_of_its_own() {
        let signals = counter_signals(&counters(44.0), Some(100.0), false);
        let life = signal(&signals, "page-life");
        assert_eq!((life.label.as_str(), life.status), ("Page life expectancy", SignalStatus::Bad));
        assert!(life.healthy.as_deref().unwrap().starts_with("how long a page stays in memory once read"));
        assert_eq!(signal(&signals, "compiles-per-batch").status, SignalStatus::Bad);
    }

    // On Azure SQL Database the counters are the server process's, shared by the elastic pool: context, never graded.
    #[test]
    fn azure_counters_are_the_whole_servers_and_never_graded() {
        let signals = counter_signals(&counters(44.0), Some(100.0), true);
        for key in ["page-life", "compiles-per-batch", "batch-requests"] {
            let s = signal(&signals, key);
            assert_eq!(s.status, SignalStatus::Info, "{key}");
            assert!(s.label.ends_with("(whole server)"), "{}", s.label);
            assert!(s.healthy.as_deref().unwrap().contains("every database on the server"), "{key}");
        }
    }

    // SQL Compilations/sec counts recompiles too: plan reuse is judged on first compiles only.
    #[test]
    fn recompiles_are_not_counted_as_new_plans() {
        let mut c = counters(400.0);
        c.insert("SQL Re-Compilations/sec".into(), 1_800_000.0);
        let signals = counter_signals(&c, Some(100.0), false);
        assert_eq!(signal(&signals, "compiles-per-batch").value, 25.0);
        assert_eq!(signal(&signals, "recompiles-per-batch").value, 25.0);
    }
    #[test]
    fn unknown_recompile_counts_cannot_be_substituted_with_zero_for_compile_ratios() {
        let mut counts = counters(400.0);
        counts.remove("SQL Re-Compilations/sec");
        let signals = counter_signals(&counts, Some(100.0), false);
        assert!(
            !signals.iter().any(|signal| matches!(signal.key.as_str(), "compiles-per-batch" | "recompiles-per-batch"))
        );
        let signals = sampled_signals(|name| counts.get(name).copied(), 0.0, 0.0, false);
        assert!(!signals.iter().any(|signal| signal.key == "compiles-per-batch"));
    }

    #[test]
    fn aggregate_resource_counters_need_individual_samples_before_a_pressure_verdict() {
        let calm = resource_signals(&crate::resources::Summary {
            data_io: Some(0.0),
            data_io_peak: Some(3.0),
            memory: Some(0.4),
        });
        assert_eq!(signal(&calm, "data-io").status, SignalStatus::Info);
        assert!(signal(&calm, "data-io").healthy.as_deref().unwrap().contains("peak 3.0%"));
        assert_eq!(signal(&calm, "memory-use").status, SignalStatus::Info);
        assert_eq!(
            signal(
                &resource_signals(&crate::resources::Summary {
                    data_io: Some(85.0),
                    data_io_peak: Some(100.0),
                    memory: Some(90.0)
                }),
                "data-io"
            )
            .status,
            SignalStatus::Info
        );
    }

    #[test]
    fn scheduling_share_alone_never_claims_cpu_pressure() {
        for scope in [SignalScope::SinceStartup, SignalScope::Sampled] {
            assert_eq!(scheduling_share(99.0, 100.0, scope).status, SignalStatus::Info);
        }
    }
    #[tokio::test]
    async fn a_failed_counter_read_stops_sampling_without_waiting_or_sending_more_queries() {
        use crate::{read::ReadError, resources::test_support::FakeRead};
        use culprit_core::telemetry::CollectionStop;
        for reason in [CollectionStop::TimedOut, CollectionStop::Cancelled, CollectionStop::CancellationUnconfirmed] {
            let mut reader = FakeRead::new(vec![
                Ok(vec![crate::waits::tests::row("WRITELOG", 10)]),
                Err(ReadError::Stopped(reason)),
            ]);
            let result = tokio::time::timeout(Duration::from_millis(50), sample(&mut reader, true, 60)).await.unwrap();
            assert_eq!(CollectionStop::from_error(&result.err().unwrap()), reason);
            assert_eq!(reader.calls.len(), 2);
        }
    }
    #[tokio::test]
    async fn a_failed_second_counter_read_cannot_publish_a_complete_wait_sample() {
        use crate::{read::ReadError, resources::test_support::FakeRead};
        use culprit_core::telemetry::CollectionStop;
        let mut reader = FakeRead::new(vec![
            Ok(vec![crate::waits::tests::row("WRITELOG", 10)]),
            Ok(vec![crate::performance::tests::row()]),
            Ok(vec![crate::waits::tests::row("WRITELOG", 20)]),
            Err(ReadError::Stopped(CollectionStop::TimedOut)),
        ]);
        let error = sample(&mut reader, true, 0).await.err().unwrap();
        assert_eq!(CollectionStop::from_error(&error), CollectionStop::TimedOut);
        assert_eq!(reader.calls.len(), 4);
    }
}
