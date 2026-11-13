//! Pressure signals, waits from statement timing, and the "right now" sample. PostgreSQL keeps no wait history, so
//! history comes from per-statement I/O timing, and "right now" samples sessions every second.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use culprit_core::{
    CleanupHolder, Coverage, Engine, Pressure, Result, Signal, SignalScope, SignalUnit, WaitSample, WaitStat, t,
};
use tokio_postgres::Client;

use crate::probe::Probe;
use crate::rows::{query, query_one};
use crate::sources;

/// pg_stat_statements I/O time columns per version (milliseconds; zero unless track_io_timing is on).
pub fn statements_io_column(version_num: u32) -> &'static str {
    if version_num >= 170_000 {
        "(shared_blk_read_time + shared_blk_write_time + local_blk_read_time + local_blk_write_time + temp_blk_read_time + temp_blk_write_time)"
    } else if version_num >= 150_000 {
        "(blk_read_time + blk_write_time + temp_blk_read_time + temp_blk_write_time)"
    } else {
        "(blk_read_time + blk_write_time)"
    }
}

/// Database time by category from statement timing: disk I/O where it is timed, the rest as "Other". WAL write and
/// sync time (pg_stat_wal, pg_stat_io) isn't in it: that is the whole server's, spent mostly by the WAL writer and by
/// commits after a statement's timing ends, so it is no share of statement time. The scan shows it with the files'
/// latency instead.
pub async fn history_waits(
    _client: &Client,
    _probe: &Probe,
    statements_ms: f64,
    statements_io_ms: f64,
) -> Vec<WaitStat> {
    breakdown(statements_ms, statements_io_ms)
}

/// Statement time split into disk I/O and the rest. Nothing else PostgreSQL times is part of statement time per
/// category: CPU, locks and untimed waits stay together, never guessed apart.
fn breakdown(statements_ms: f64, statements_io_ms: f64) -> Vec<WaitStat> {
    if statements_io_ms <= 0.0 || statements_io_ms > statements_ms {
        return Vec::new();
    }
    let other = (statements_ms - statements_io_ms).max(0.0);
    let mut out = vec![WaitStat {
        category: "IO".into(),
        wait_type: Some(t!("provider.postgres.wait.data_files")),
        wait_ms: statements_io_ms,
        tasks: None,
    }];
    if other > 0.0 {
        out.push(WaitStat {
            category: "Other".into(),
            wait_type: Some(t!("provider.postgres.wait.other")),
            wait_ms: other,
            tasks: None,
        });
    }
    out.sort_by(|a, b| b.wait_ms.total_cmp(&a.wait_ms));
    out
}

/// The pressure counters, each source read through `Coverage::read` (`crate::sources`).
pub async fn collect(client: &Client, probe: &Probe) -> (Pressure, Coverage) {
    let mut p = Pressure::default();
    let mut coverage = Coverage::default();
    // How long the counters have counted. PostgreSQL keeps statistics across a clean restart, so the server's start
    // time says nothing about it. A reset (of the database's counters or of one table's) stamps the database's
    // stats_reset. Until then it is empty: the counters run from when PostgreSQL last discarded its statistics (a
    // crash, a restore, the first start), which also stamps the shared counters' stats_reset (pg_stat_bgwriter,
    // pg_stat_archiver) with that moment. A later reset of one of those moves only its own stamp, so the earlier of the
    // two is the closer one: still never older than the counters (no "unused" claim stretches past them), and a rate
    // isn't inflated by a span cut short.
    let database = query_one(
        client,
        "SELECT blks_hit, blks_read, temp_bytes, xact_commit, xact_rollback, deadlocks,
                extract(epoch FROM now() - coalesce(stats_reset,
                    (SELECT least(b.stats_reset, a.stats_reset) FROM pg_stat_bgwriter b, pg_stat_archiver a))) / 3600 AS hours
           FROM pg_stat_database WHERE datname = current_database()",
    )
    .await;
    if let Some(Some(r)) = coverage.read(&sources::DATABASE_STATS, database) {
        let hours = r.number("hours").filter(|h| *h > 0.0);
        p.uptime_hours = hours;
        let (hit, read) = (r.number("blks_hit").unwrap_or(0.0), r.number("blks_read").unwrap_or(0.0));
        if hit + read > 0.0 {
            p.signals.push(
                Signal::new(
                    culprit_core::signal_id::CACHE_HIT,
                    // shared_buffers only: a read the operating system's cache serves counts as a miss here.
                    culprit_core::message!("provider.postgres.signal.cache_hit"),
                    hit / (hit + read) * 100.0,
                    SignalUnit::Percent,
                    SignalScope::SinceStartup,
                )
                .graded(&t!("provider.postgres.signal.cache_hit_healthy"), 99.0, 95.0, false),
            );
        }
        let temp = r.number("temp_bytes").unwrap_or(0.0);
        match hours {
            // Over an hour of known counting: a rate, graded.
            Some(h) if h >= 1.0 => p.signals.push(
                Signal::new(
                    culprit_core::signal_id::TEMP_BYTES,
                    culprit_core::message!("provider.postgres.signal.temp_per_hour"),
                    temp / h,
                    SignalUnit::Bytes,
                    SignalScope::SinceStartup,
                )
                .graded(
                    &t!("provider.postgres.signal.temp_per_hour_healthy"),
                    100.0 * 1024.0 * 1024.0,
                    1024.0 * 1024.0 * 1024.0,
                    true,
                ),
            ),
            // Under an hour, or since an unknown time: the total, which no rate threshold applies to.
            Some(_) => p.signals.push(Signal::new(
                culprit_core::signal_id::TEMP_BYTES,
                culprit_core::message!("provider.postgres.signal.temp_since_reset"),
                temp,
                SignalUnit::Bytes,
                SignalScope::SinceStartup,
            )),
            None => p.signals.push(Signal::new(
                culprit_core::signal_id::TEMP_BYTES,
                culprit_core::message!("provider.postgres.signal.temp_since_began"),
                temp,
                SignalUnit::Bytes,
                SignalScope::SinceStartup,
            )),
        }
        if let Some(h) = hours {
            p.signals.push(Signal::new(
                "commits",
                culprit_core::message!("provider.postgres.signal.commits"),
                r.number("xact_commit").unwrap_or(0.0) / (h * 3600.0),
                SignalUnit::PerSecond,
                SignalScope::SinceStartup,
            ));
        }
        let (commits, rollbacks) = (r.number("xact_commit").unwrap_or(0.0), r.number("xact_rollback").unwrap_or(0.0));
        if commits + rollbacks > 0.0 {
            p.signals.push(
                Signal::new(
                    "rollbacks",
                    culprit_core::message!("provider.postgres.signal.rollbacks"),
                    rollbacks / (commits + rollbacks) * 100.0,
                    SignalUnit::Percent,
                    SignalScope::SinceStartup,
                )
                .graded(&t!("provider.postgres.signal.rollbacks_healthy"), 5.0, 20.0, true),
            );
        }
        p.signals.push(
            Signal::new(
                "deadlocks",
                culprit_core::message!("provider.postgres.signal.deadlocks"),
                r.number("deadlocks").unwrap_or(0.0),
                SignalUnit::Count,
                SignalScope::SinceStartup,
            )
            .graded(&t!("provider.postgres.signal.deadlocks_healthy"), 1.0, 10.0, true),
        );
    }
    let checkpoints_sql = if probe.version_num >= 170_000 {
        "SELECT num_timed AS timed, num_requested AS requested FROM pg_stat_checkpointer"
    } else {
        "SELECT checkpoints_timed AS timed, checkpoints_req AS requested FROM pg_stat_bgwriter"
    };
    if let Some(Some(r)) = coverage.read(&sources::CHECKPOINTS, query_one(client, checkpoints_sql).await) {
        let (timed, requested) = (r.number("timed").unwrap_or(0.0), r.number("requested").unwrap_or(0.0));
        if timed + requested > 0.0 {
            p.signals.push(Signal::new(
                culprit_core::signal_id::CHECKPOINTS,
                culprit_core::message!("provider.postgres.signal.checkpoints"),
                timed + requested,
                SignalUnit::Count,
                SignalScope::SinceStartup,
            ));
            p.signals.push(
                Signal::new(
                    culprit_core::signal_id::CHECKPOINTS_REQUESTED,
                    culprit_core::message!("provider.postgres.signal.checkpoints_requested"),
                    requested / (timed + requested) * 100.0,
                    SignalUnit::Percent,
                    SignalScope::SinceStartup,
                )
                .graded(
                    &t!("provider.postgres.signal.checkpoints_requested_healthy"),
                    10.0,
                    30.0,
                    true,
                ),
            );
        }
    }
    if probe.version_num >= 140_000
        && let Ok(Some(r)) = query_one(client, "SELECT wal_buffers_full FROM pg_stat_wal").await
        && let Some(full) = r.number("wal_buffers_full").filter(|n| *n > 0.0)
    {
        p.signals.push(Signal::new(
            "wal-buffers-full",
            culprit_core::message!("provider.postgres.signal.wal_buffers_full"),
            full,
            SignalUnit::Count,
            SignalScope::SinceStartup,
        ));
    }
    if let Ok(Some(r)) = query_one(
        client,
        "SELECT count(*) FILTER (WHERE backend_type = 'client backend' AND application_name IS DISTINCT FROM 'culprit') AS used,
                current_setting('max_connections')::float8 - current_setting('superuser_reserved_connections')::float8 AS max
           FROM pg_stat_activity",
    )
    .await
        && let (Some(used), Some(max)) = (r.number("used"), r.number("max").filter(|m| *m > 0.0))
    {
        p.signals.push(
            Signal::new(
                culprit_core::signal_id::CONNECTIONS,
                culprit_core::message!("provider.postgres.signal.connections"),
                used / max * 100.0,
                SignalUnit::Percent,
                SignalScope::Now,
            )
            .graded(&t!("provider.postgres.signal.connections_healthy"), 80.0, 90.0, true),
        );
    }
    let retention = cleanup_holders(client, probe, &mut coverage).await;
    project_cleanup(&mut p, retention);
    (p, coverage)
}

fn project_cleanup(p: &mut Pressure, retention: culprit_core::telemetry::RetentionSnapshot) {
    use culprit_core::telemetry::CleanupHorizon;
    let data = retention.oldest_observed(CleanupHorizon::Data);
    let catalog = retention.oldest_observed(CleanupHorizon::Catalog);
    let selected = data
        .first()
        .map(|holder| (*holder, CleanupHorizon::Data, data.len()))
        .or_else(|| catalog.first().map(|holder| (*holder, CleanupHorizon::Catalog, catalog.len())));
    if let Some((holder, horizon, tied)) = selected {
        // Display-only unknown ages never become measured zero seconds or a graded global oldest.
        if holder.holder != CleanupHolder::ReplicationSlot
            && retention.is_horizon_complete(horizon)
            && tied == 1
            && holder.scope == culprit_core::MeasurementScope::Database
            && let Some(seconds) = holder.age_seconds
        {
            p.signals.push(
                Signal::new(
                    culprit_core::signal_id::OLDEST_TRANSACTION,
                    culprit_core::message!("provider.postgres.signal.oldest_transaction"),
                    seconds,
                    SignalUnit::Seconds,
                    SignalScope::Now,
                )
                .graded(
                    &t!("provider.postgres.signal.oldest_transaction_healthy"),
                    600.0,
                    1800.0,
                    true,
                ),
            );
        }
        // The old duration card cannot express an unknown session age. The typed retainer stays available.
        if holder.holder == CleanupHolder::ReplicationSlot || holder.age_seconds.is_some() {
            p.oldest_transaction = Some(holder.display(horizon));
        }
    }
    p.retention = Some(retention);
}

/// Sessions of this database (and physical walsenders, which carry a standby's feedback for every database) whose
/// snapshot or transaction id holds VACUUM's horizon back. A session idle in a read-committed transaction has neither,
/// and a session in another database doesn't hold this one's tables back, so neither is listed. Autovacuum's own
/// workers are ignored by VACUUM.
async fn cleanup_holders(
    client: &Client,
    probe: &Probe,
    coverage: &mut Coverage,
) -> culprit_core::telemetry::RetentionSnapshot {
    use crate::native::cleanup::{self, Kind};
    use culprit_core::{SourceState, telemetry::*};
    let mut reader = client;
    let mut holders = vec![];
    let mut native_sources = vec![];
    for (kind, source) in [
        (Kind::Session, &sources::OPEN_TRANSACTIONS),
        (Kind::Slot, &sources::CLEANUP_SLOTS),
        (Kind::Prepared, &sources::CLEANUP_PREPARED),
    ] {
        let remaining = MAX_RETENTION_HOLDERS.saturating_sub(holders.len());
        let result = if !probe.privileged {
            Err(culprit_core::Error::new(
                culprit_core::ErrorCode::PermissionDenied,
                t!("provider.postgres.capability.waits.hidden"),
            ))
        } else if remaining == 0 {
            Err(CollectionStop::RowLimit.error())
        } else {
            cleanup::read(&mut reader, kind, probe.version_num, remaining).await
        };
        let state = if let Some(reading) = coverage.read(source, result) {
            let state = cleanup_coverage(coverage, source, kind.source(), probe.standby, &reading);
            holders.extend(reading.holders);
            state
        } else {
            SourceState::Unavailable
        };
        native_sources.push(RetentionSourceCoverage { source: kind.source().into(), state });
    }
    RetentionSnapshot { coverage: cleanup::coverage(probe.standby), holders, sources: native_sources }
}

fn cleanup_coverage(
    coverage: &mut Coverage,
    source: &culprit_core::Source,
    native_source: &str,
    standby: bool,
    reading: &crate::native::cleanup::Reading,
) -> culprit_core::SourceState {
    use culprit_core::{SourceState, telemetry::CollectionStop};
    let scope_complete = crate::native::cleanup::scope_complete(native_source, standby);
    coverage.completed(source, reading.holders.len());
    if reading.truncated {
        coverage.partial(source, CollectionStop::RowLimit.error().message_source());
    }
    if !scope_complete {
        coverage.partial(source, culprit_core::message!("provider.postgres.cleanup.recovery_horizon"));
    }
    if reading.truncated || !scope_complete {
        SourceState::Partial
    } else if reading.holders.is_empty() {
        SourceState::Empty
    } else {
        SourceState::Collected
    }
}

/// The shared category for a PostgreSQL wait event. A session with no wait event is running on CPU.
pub fn category(event_type: Option<&str>, event: Option<&str>) -> &'static str {
    let event = event.unwrap_or("");
    match event_type {
        None => "CPU",
        Some("IO") if event.starts_with("Wal") || event.starts_with("WAL") => "Log",
        Some("IO") => "IO",
        Some("LWLock") if event.starts_with("WAL") || event.starts_with("Wal") => "Log",
        Some("LWLock") | Some("BufferPin") => "Latch",
        Some("Lock") => "Lock",
        Some("Client") => "Network",
        Some("IPC")
            if event.starts_with("BgWorker")
                || event.starts_with("ExecuteGather")
                || event.starts_with("Parallel")
                || event.starts_with("Hash")
                || event.starts_with("MessageQueue") =>
        {
            "Parallelism"
        }
        _ => "Other",
    }
}

/// Sessions sampled every second for `seconds`: each observation of a busy session counts as one second of its time.
pub async fn sample(client: &Client, seconds: u32) -> Result<WaitSample> {
    let started = Instant::now();
    let taken_at = query_one(client, &format!("SELECT {} AS now", crate::rows::iso("now()")))
        .await?
        .and_then(|r| r.text("now"))
        .unwrap_or_default();
    let commits = |r: Option<crate::rows::Row>| r.and_then(|r| r.number("commits"));
    let commit_sql =
        "SELECT xact_commit + xact_rollback AS commits FROM pg_stat_database WHERE datname = current_database()";
    let before = commits(query_one(client, commit_sql).await?);
    let mut observed: BTreeMap<(String, String), f64> = BTreeMap::new();
    let mut samples = 0.0;
    let mut busy = 0.0;
    // N seconds need N + 1 samples, one at each end.
    let ticks = seconds.max(1) + 1;
    for tick in 0..ticks {
        let rows = query(
            client,
            "SELECT wait_event_type AS type, wait_event AS event, count(*) AS n
               FROM pg_stat_activity
              WHERE state = 'active' AND backend_type = 'client backend' AND pid <> pg_backend_pid()
                AND datname = current_database() AND application_name IS DISTINCT FROM 'culprit'
              GROUP BY 1, 2",
        )
        .await?;
        samples += 1.0;
        for r in rows {
            let n = r.number("n").unwrap_or(0.0);
            busy += n;
            let key = (r.text("type").unwrap_or_default(), r.text("event").unwrap_or_else(|| "Running".into()));
            *observed.entry(key).or_default() += n;
        }
        if tick + 1 < ticks {
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    }
    let after = commits(query_one(client, commit_sql).await?);
    let elapsed = started.elapsed().as_secs_f64().max(1.0);
    // Each observation stands for one second of one session's time.
    let mut by_category: BTreeMap<&'static str, (f64, String, f64)> = BTreeMap::new();
    for ((event_type, event), n) in &observed {
        let key = category(Some(event_type.as_str()).filter(|t| !t.is_empty()), Some(event.as_str()));
        let entry = by_category.entry(key).or_default();
        entry.0 += n;
        if *n > entry.2 {
            entry.1 = if event_type.is_empty() {
                t!("provider.postgres.wait.running")
            } else {
                format!("{event_type}: {event}")
            };
            entry.2 = *n;
        }
    }
    let mut categories: Vec<WaitStat> = by_category
        .into_iter()
        .map(|(category, (n, top, _))| WaitStat {
            category: category.into(),
            wait_type: Some(top),
            wait_ms: n / samples * elapsed * 1000.0,
            tasks: Some(n),
        })
        .collect();
    categories.sort_by(|a, b| b.wait_ms.total_cmp(&a.wait_ms));
    let mut signals = Vec::new();
    if let (Some(a), Some(b)) = (before, after) {
        signals.push(Signal::new(
            "commits",
            culprit_core::message!("provider.postgres.signal.transactions"),
            ((b - a) / elapsed).max(0.0),
            SignalUnit::PerSecond,
            SignalScope::Sampled,
        ));
    }
    let mut notes = Vec::new();
    if busy == 0.0 {
        notes.push(t!("provider.postgres.sample.idle"));
    }
    Ok(WaitSample {
        engine: Engine::Postgres,
        taken_at,
        seconds: elapsed,
        method: t!("provider.postgres.sample.method", seconds = seconds.to_string()),
        total_ms: busy / samples * elapsed * 1000.0,
        categories,
        signals,
        active_sessions: Some(if samples > 0.0 { busy / samples } else { 0.0 }),
        notes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn events_fold_into_shared_categories() {
        assert_eq!(category(None, None), "CPU");
        assert_eq!(category(Some("IO"), Some("DataFileRead")), "IO");
        assert_eq!(category(Some("IO"), Some("WalSync")), "Log");
        assert_eq!(category(Some("LWLock"), Some("WALWrite")), "Log");
        assert_eq!(category(Some("LWLock"), Some("BufferMapping")), "Latch");
        assert_eq!(category(Some("Lock"), Some("transactionid")), "Lock");
        assert_eq!(category(Some("Client"), Some("ClientWrite")), "Network");
        assert_eq!(category(Some("IPC"), Some("ExecuteGather")), "Parallelism");
        assert_eq!(category(Some("Timeout"), Some("PgSleep")), "Other");
    }

    #[test]
    fn io_columns_follow_the_version() {
        assert!(statements_io_column(170_000).contains("shared_blk_read_time"));
        assert!(statements_io_column(150_000).contains("temp_blk_read_time"));
        assert_eq!(statements_io_column(130_000), "(blk_read_time + blk_write_time)");
    }

    fn category_ms(waits: &[WaitStat], category: &str) -> f64 {
        waits.iter().filter(|w| w.category == category).map(|w| w.wait_ms).sum()
    }

    // The aptabase reading: pg_stat_wal's write and sync time (4,143 s) stood for 99.98% of statement time, because it
    // was capped at the time statements didn't spend on I/O. It is the server's WAL writer and commits, outside
    // statement time: never a share of it.
    #[test]
    fn wal_time_is_no_share_of_statement_time() {
        let waits = breakdown(4_143_706.0, 750.6);
        assert_eq!(category_ms(&waits, "Log"), 0.0);
        assert_eq!(category_ms(&waits, "IO"), 750.6);
        assert!((category_ms(&waits, "Other") - 4_142_955.4).abs() < 0.01);
        assert!(waits.iter().all(|w| !w.wait_type.as_deref().unwrap_or_default().contains("WAL")));
        // Nothing timed per category: no breakdown, rather than all of it as "Other".
        assert!(breakdown(100_000.0, 0.0).is_empty(), "nothing timed, nothing to break down");
        assert!(breakdown(100.0, 150.0).is_empty(), "overlapping I/O time is not a partition of execution time");
    }

    // What the statement-time breakdown calls its parts, word for word.
    #[test]
    fn breakdown_in_english() {
        let waits = breakdown(100.0, 40.0);
        let named: Vec<(&str, &str)> =
            waits.iter().map(|w| (w.category.as_str(), w.wait_type.as_deref().unwrap())).collect();
        assert_eq!(named, [("Other", "CPU, locks and untimed waits"), ("IO", "data file reads and writes")]);
    }

    #[test]
    fn display_only_unknown_or_incomplete_retention_ages_never_become_graded_zeroes() {
        use culprit_core::{MeasurementScope, SourceState, TransactionActivity, telemetry::*};
        let holder = RetentionObservation {
            source: "pg_stat_activity".into(),
            identity: NativeIdentity::new("session-42"),
            observed_at: "2026-11-13T08:00:00Z".into(),
            scope: MeasurementScope::Database,
            holder: CleanupHolder::Session,
            name: "42".into(),
            horizons: vec![RetentionHorizon {
                protects: vec![CleanupHorizon::Data, CleanupHorizon::Catalog, CleanupHorizon::SharedCatalog],
                native_name: "backend_xmin".into(),
                native_value: "100".into(),
                transactions_behind: Some("10".into()),
            }],
            session: Some(NativeActivityIdentity { id: "42".into(), generation: "2026-11-13T07:00:00Z".into() }),
            unresolved_session_id: None,
            transaction: None,
            query: None,
            client: None,
            activity: TransactionActivity::Reported,
            native_state: None,
            age_seconds: None,
            database: Some("application".into()),
            code_ownership: PayloadUnavailable::NotCollected,
        };
        let original = RetentionSnapshot {
            coverage: RetentionCoverage {
                native_sources: vec!["pg_stat_activity".into()],
                qualified_horizons: vec![CleanupHorizon::Data, CleanupHorizon::Catalog],
            },
            holders: vec![holder],
            sources: vec![RetentionSourceCoverage { source: "pg_stat_activity".into(), state: SourceState::Collected }],
        };
        let mut pressure = Pressure::default();
        project_cleanup(&mut pressure, original.clone());
        assert!(pressure.oldest_transaction.is_none(), "the duration display cannot express an unknown native age");
        assert_eq!(pressure.retention.as_ref().unwrap().holders.len(), 1, "typed retainer context remains available");
        assert!(pressure.signals.is_empty(), "display seconds=0 is not a native age measurement");
        let mut measured = original.clone();
        measured.holders[0].age_seconds = Some(7200.0);
        let mut pressure = Pressure::default();
        project_cleanup(&mut pressure, measured.clone());
        assert_eq!(pressure.signals[0].value, 7200.0);
        for state in [SourceState::Partial, SourceState::Unavailable] {
            let mut partial = measured.clone();
            partial.sources[0].state = state;
            let mut pressure = Pressure::default();
            project_cleanup(&mut pressure, partial);
            assert!(pressure.signals.is_empty(), "{state:?} cannot establish a graded oldest");
        }
        let mut tied = measured.clone();
        let mut second = tied.holders[0].clone();
        second.identity = NativeIdentity::new("session-43");
        second.name = "43".into();
        tied.holders.push(second);
        let mut pressure = Pressure::default();
        project_cleanup(&mut pressure, tied);
        assert!(pressure.signals.is_empty(), "ties cannot select an arbitrary measured oldest age");
        measured.holders[0].scope = MeasurementScope::Server;
        let mut pressure = Pressure::default();
        project_cleanup(&mut pressure, measured);
        assert!(pressure.signals.is_empty(), "shared feedback cannot become database-specific pressure");
    }

    #[test]
    fn recovery_coverage_retains_native_rows_and_explains_the_missing_cluster_boundary() {
        use culprit_core::{SourceState, telemetry::*};
        let row = crate::rows::Row::from_fields([
            ("native_source".into(), Some("pg_stat_activity".into())),
            (
                "native_values".into(),
                Some(
                    serde_json::json!({
                        "native_id":"42","name":"42","observed_at":"2026-11-13T08:00:00.000001Z",
                        "native_scope":"database","database_oid":"16384","backend_start":"2026-11-13T06:00:00.000001Z",
                        "data_xmin":"100","data_xmin_age":"5000000","seconds":"7200"
                    })
                    .to_string(),
                ),
            ),
        ]);
        let reading = crate::native::cleanup::Reading {
            holders: vec![crate::native::cleanup::decode(&row).unwrap()],
            truncated: false,
        };
        let mut coverage = Coverage::default();
        let state = cleanup_coverage(&mut coverage, &sources::OPEN_TRANSACTIONS, "pg_stat_activity", true, &reading);
        assert_eq!(state, SourceState::Partial);
        let outcome =
            coverage.outcomes().into_iter().find(|source| source.source == sources::OPEN_TRANSACTIONS.name).unwrap();
        assert!(outcome.reason_message.is_some());
        assert!(outcome.reason.unwrap().contains("complete cleanup boundary is unknown"));
        let mut pressure = Pressure::default();
        project_cleanup(
            &mut pressure,
            RetentionSnapshot {
                coverage: RetentionCoverage {
                    native_sources: vec!["pg_stat_activity".into()],
                    qualified_horizons: vec![],
                },
                holders: reading.holders,
                sources: vec![RetentionSourceCoverage { source: "pg_stat_activity".into(), state }],
            },
        );
        assert_eq!(pressure.retention.as_ref().unwrap().holders.len(), 1);
        assert!(!pressure.retention.as_ref().unwrap().is_horizon_complete(CleanupHorizon::Data));
        assert!(pressure.signals.is_empty());
    }
}
