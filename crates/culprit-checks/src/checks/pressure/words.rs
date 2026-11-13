//! What each pressure finding says, word for word, in every shape it takes: its messages come from locales/en.json,
//! and moving them there must not change a word of English.

use super::*;
use crate::waits::snapshot_with;
use culprit_core::{GrantStat, OpenTransaction, ResourceSample, Setting, Signal, SignalScope, WaitStat};

/// Every word a finding shows, a part a line, with what it concludes beside: what these tests pin.
pub(crate) fn words(f: &Finding) -> String {
    let mut out = vec![
        format!(
            "{} · {:?} · {:?} · {:?} · score {:.2}",
            f.id, f.severity, f.confidence, f.actionability, f.impact.score
        ),
        format!("title: {}", f.title),
        format!("summary: {}", f.summary),
        format!("impact: {}", f.impact.label),
    ];
    for m in &f.metrics {
        out.push(match &m.text {
            Some(text) => format!("metric: {}: {text}", m.label),
            None => format!("metric: {}", m.label),
        });
    }
    if let Some(fix) = &f.fix {
        out.push(format!("fix: {}", fix.summary));
        if let Some(sql) = &fix.sql {
            out.push(format!("sql: {sql}"));
        }
        if let Some(cost) = &fix.cost {
            out.push(format!("cost: {cost}"));
        }
    }
    out.extend(f.verify.iter().map(|v| format!("verify: {v}")));
    out.extend(f.caveats.iter().map(|c| format!("caveat: {c}")));
    out.join("\n")
}

/// Every finding `check` makes of `s`, said.
pub(crate) fn said(check: &dyn Check, s: &Snapshot) -> String {
    let findings = check.run(s);
    assert!(!findings.is_empty(), "{} found nothing", check.info().id);
    findings.iter().map(words).collect::<Vec<_>>().join("\n--\n")
}

/// Each `(scenario, what it says)` against what it said before its words moved to the catalog.
pub(crate) fn pin(scenarios: Vec<(&str, String)>, expected: &[(&str, &str)]) {
    assert_eq!(scenarios.len(), expected.len(), "a scenario without its words");
    for ((name, text), (pinned, words)) in scenarios.iter().zip(expected) {
        assert_eq!(name, pinned);
        assert_eq!(text, words, "{name}");
    }
}

fn with_signals(engine: Engine, signals: Vec<Signal>) -> Snapshot {
    let mut s = snapshot_with(engine);
    s.pressure = Some(Pressure { uptime_hours: Some(72.0), signals, ..Pressure::default() });
    s
}

fn sig(key: &str, value: f64) -> Signal {
    Signal::new(key, key, value, SignalUnit::Count, SignalScope::SinceStartup)
}

fn set(s: &mut Snapshot, name: &str, value: &str) {
    s.settings.retain(|x| x.name != name);
    s.settings.push(Setting { name: name.into(), value: value.into(), default_value: None, scope: "server".into() });
}

fn uptime(mut s: Snapshot, hours: f64) -> Snapshot {
    s.pressure.as_mut().unwrap().uptime_hours = Some(hours);
    s
}

fn azure(mut s: Snapshot) -> Snapshot {
    s.server.set_platform(Some(culprit_core::Platform::AzureSqlDatabase));
    s
}

/// A workload window of the last `hours`, ending where it ends now.
fn last_hours(s: &mut Snapshot, hours: f64) {
    let w = s.workload.as_mut().unwrap();
    (w.windowed, w.window_hours) = (true, hours);
    let end = w.window_end.as_deref().and_then(format::epoch_seconds).unwrap();
    w.window_start = Some(format::iso_from_epoch(end - hours * 3600.0));
}

/// The first query, made one that stopped running a day and more before the window ended.
fn stopped(s: &mut Snapshot) {
    last_hours(s, 48.0);
    let end = s.workload.as_ref().unwrap().window_end.as_deref().and_then(format::epoch_seconds).unwrap();
    let q = &mut s.workload.as_mut().unwrap().queries[0];
    q.first_seen = Some(format::iso_from_epoch(end - 47.0 * 3600.0));
    q.last_seen = Some(format::iso_from_epoch(end - 30.0 * 3600.0));
}

#[test]
fn every_pressure_check_describes_itself() {
    let said: Vec<String> = CHECKS
        .iter()
        .map(|c| {
            let info = c.info();
            format!("{}: {}: {}", info.id, info.title, info.description)
        })
        .collect();
    pin(vec![("infos", said.join("\n"))], INFOS);
}

#[test]
fn checkpoint_pressure_words() {
    let signals = || vec![sig("checkpoints-requested", 62.0), sig("checkpoints", 50.0)];
    let mut gb = with_signals(Engine::Postgres, signals());
    set(&mut gb, "max_wal_size", "1GB");
    let mut mb = with_signals(Engine::Postgres, signals());
    set(&mut mb, "max_wal_size", "5000MB");
    let none = with_signals(Engine::Postgres, vec![sig("checkpoints-requested", 45.0), sig("checkpoints", 200.0)]);
    pin(
        vec![
            ("1gb", said(&CheckpointPressureCheck, &gb)),
            ("5000mb", said(&CheckpointPressureCheck, &mb)),
            ("unknown", said(&CheckpointPressureCheck, &none)),
        ],
        CHECKPOINT_PRESSURE,
    );
}

#[test]
fn compile_pressure_words() {
    let compiles = |recompiles: bool| {
        let mut signals = vec![sig("compiles-per-batch", 31.0), sig("batch-requests", 120.0)];
        if recompiles {
            signals.push(sig("recompiles-per-batch", 4.0));
        }
        let mut s = with_signals(Engine::Mssql, signals);
        s.query_families = vec![culprit_core::QueryFamily {
            key: "0xAB".into(),
            sample: "SELECT * FROM orders WHERE id = 42".into(),
            variants: 900.0,
            executions: 900.0,
            total_time_ms: 9_000.0,
            cached_bytes: None,
            query_ids: Vec::new(),
        }];
        s
    };
    pin(
        vec![
            ("days", said(&CompilePressureCheck, &compiles(true))),
            ("hours", said(&CompilePressureCheck, &uptime(compiles(false), 30.0))),
            ("minutes", said(&CompilePressureCheck, &uptime(compiles(false), 1.2))),
        ],
        COMPILE_PRESSURE,
    );
}

fn samples(pick: impl Fn(usize) -> f64, dim: usize) -> Vec<ResourceSample> {
    (0..240)
        .map(|i| {
            let mut v = [30.0, 10.0, 10.0, 5.0];
            v[dim] = pick(i);
            ResourceSample {
                at: format::iso_from_epoch(format::epoch_seconds("2026-10-11T10:00:15Z").unwrap() + i as f64 * 15.0),
                cpu_pct: Some(v[0]),
                data_io_pct: Some(v[1]),
                log_write_pct: Some(v[2]),
                memory_pct: Some(80.0),
                workers_pct: Some(v[3]),
            }
        })
        .collect()
}

#[test]
fn tier_limit_words() {
    let at = |dim: usize| {
        let mut s = snapshot_with(Engine::Mssql);
        s.resources = samples(|i| if i % 5 < 2 { 99.0 } else { 40.0 }, dim);
        said(&TierLimitCheck, &s)
    };
    pin(vec![("cpu", at(0)), ("data-io", at(1)), ("log-write", at(2)), ("workers", at(3))], TIER_LIMIT);
}

fn grant(hash: &str) -> GrantStat {
    GrantStat {
        query_hash: hash.into(),
        executions: 40.0,
        avg_granted_kb: 2.0 * 1024.0 * 1024.0,
        avg_used_kb: 40.0 * 1024.0,
        max_granted_kb: 3.0 * 1024.0 * 1024.0,
        last_granted_kb: None,
        last_used_kb: None,
    }
}

#[test]
fn memory_grant_words() {
    let mut running = with_signals(Engine::Mssql, vec![sig("memory-grants-pending", 6.0)]);
    let q = &mut running.workload.as_mut().unwrap().queries[0];
    q.waits =
        vec![WaitStat { category: "Memory".into(), wait_type: None, wait_ms: q.total_time_ms * 0.1, tasks: None }];
    let hash = q.query_hash.clone().unwrap();
    running.pressure.as_mut().unwrap().grants = vec![grant(&hash)];
    let mut gone = with_signals(Engine::Mssql, vec![]);
    stopped(&mut gone);
    gone.pressure.as_mut().unwrap().grants = vec![grant(&hash)];
    let mut unknown = with_signals(Engine::Mssql, vec![]);
    unknown.pressure.as_mut().unwrap().grants = vec![grant("0xFFFF")];
    pin(
        vec![
            ("running", said(&MemoryGrantCheck, &running)),
            ("stopped", said(&MemoryGrantCheck, &gone)),
            ("unknown", said(&MemoryGrantCheck, &unknown)),
        ],
        MEMORY_GRANT,
    );
}

#[test]
fn network_wait_words() {
    let network = |rows: Option<f64>, stop: bool| {
        let mut s = snapshot_with(Engine::Mssql);
        if stop {
            stopped(&mut s);
        }
        let q = &mut s.workload.as_mut().unwrap().queries[0];
        q.waits = vec![WaitStat {
            category: "Network".into(),
            wait_type: Some("Network IO".into()),
            wait_ms: q.total_time_ms * 0.6,
            tasks: None,
        }];
        q.rows = rows.map(|r| q.executions * r);
        said(&NetworkWaitCheck, &s)
    };
    pin(
        vec![
            ("rows", network(Some(80_000.0), false)),
            ("few-rows", network(Some(20.0), false)),
            ("no-rows", network(None, false)),
            ("stopped-rows", network(Some(80_000.0), true)),
            ("stopped", network(None, true)),
        ],
        NETWORK_WAIT,
    );
}

#[test]
fn row_lock_waits_words() {
    let hourly = |n: f64| Signal { unit: SignalUnit::PerHour, ..sig("row-lock-waits", n) };
    let mut here = with_signals(
        Engine::Mysql,
        vec![hourly(340.0), sig("row-lock-avg", 1200.0), sig("row-lock-time", 72.0 * 340.0 * 1200.0)],
    );
    set(&mut here, "transaction_isolation", "SERIALIZABLE");
    last_hours(&mut here, 10.0);
    let q = &mut here.workload.as_mut().unwrap().queries[0];
    q.waits = vec![WaitStat { category: "Lock".into(), wait_type: None, wait_ms: q.total_time_ms * 0.8, tasks: None }];
    let server = uptime(
        with_signals(
            Engine::Mysql,
            vec![sig("row-lock-waits", 90.0), sig("row-lock-avg", 1500.0), sig("row-lock-time", 135_000.0)],
        ),
        1.2,
    );
    pin(vec![("here", said(&RowLockWaitsCheck, &here)), ("server", said(&RowLockWaitsCheck, &server))], ROW_LOCK_WAITS);
}

/// The demo's waits with `ms` of memory waits in place of its own.
fn memory_waits(mut s: Snapshot, ms: f64) -> Snapshot {
    s.waits.retain(|w| w.category != "Memory");
    s.waits.push(WaitStat { category: "Memory".into(), wait_type: Some("Memory".into()), wait_ms: ms, tasks: None });
    s
}

#[test]
fn memory_queue_words() {
    let pending = |n: f64| with_signals(Engine::Mssql, vec![sig("memory-grants-pending", n)]);
    let grants = azure(with_signals(Engine::Mssql, vec![sig("grant-wait-ms", 72.0 * 30.0 * MINUTE_MS)]));
    // Query Store's window: the waits are this database's.
    let mut windowed = snapshot_with(Engine::Mssql);
    windowed.workload.as_mut().unwrap().queries[0].waits =
        vec![WaitStat { category: "Memory".into(), wait_type: None, wait_ms: 1_000.0, tasks: None }];
    let hours = windowed.workload.as_ref().unwrap().window_hours;
    let windowed = memory_waits(windowed, 20.0 * MINUTE_MS * hours * 4.0);
    // The instance's totals since it started, with queries queueing now.
    let shared = memory_waits(pending(2.0), 72.0 * 30.0 * MINUTE_MS * 10.0);
    // On Azure SQL Database the totals since the start are the database's.
    let mut database = memory_waits(azure(with_signals(Engine::Mssql, vec![])), 72.0 * 30.0 * MINUTE_MS * 10.0);
    database.pressure.as_mut().unwrap().signals.clear();
    pin(
        vec![
            ("one-pending", said(&MemoryQueueCheck, &pending(1.0))),
            ("pending", said(&MemoryQueueCheck, &pending(6.0))),
            ("grant-history", said(&MemoryQueueCheck, &grants)),
            ("windowed", said(&MemoryQueueCheck, &windowed)),
            ("shared", said(&MemoryQueueCheck, &shared)),
            ("database", said(&MemoryQueueCheck, &database)),
        ],
        MEMORY_QUEUE,
    );
}

#[test]
fn worker_pressure_words() {
    let threadpool = |signals: Vec<Signal>| with_signals(Engine::Mssql, signals);
    let history = || sig("threadpool-wait-ms", 72.0 * 120_000.0);
    let peak = |used: f64, pct: f64, ago: Option<f64>| {
        let mut signals = vec![sig("connections", used), sig("max-used-connections", pct)];
        signals.extend(ago.map(|a| sig("max-used-connections-ago", a)));
        with_signals(Engine::Mysql, signals)
    };
    pin(
        vec![
            ("waiting", said(&WorkerPressureCheck, &threadpool(vec![sig("threadpool-waiting", 2.0)]))),
            ("one-waiting", said(&WorkerPressureCheck, &threadpool(vec![sig("threadpool-waiting", 1.0)]))),
            ("history", said(&WorkerPressureCheck, &azure(threadpool(vec![history()])))),
            (
                "history-and-queue",
                said(&WorkerPressureCheck, &azure(threadpool(vec![history(), sig("threadpool-waiting", 6.0)]))),
            ),
            ("postgres", said(&WorkerPressureCheck, &with_signals(Engine::Postgres, vec![sig("connections", 92.0)]))),
            ("mysql-now", said(&WorkerPressureCheck, &peak(90.0, 100.0, None))),
            ("lowered", said(&WorkerPressureCheck, &peak(20.0, 104.0, Some(9.0 * 86_400.0)))),
            ("peak", said(&WorkerPressureCheck, &peak(20.0, 100.0, None))),
        ],
        WORKER_PRESSURE,
    );
}

fn holding(engine: Engine, t: OpenTransaction) -> Snapshot {
    let mut s = with_signals(engine, vec![]);
    s.pressure.as_mut().unwrap().oldest_transaction = Some(t);
    s
}

fn session(id: &str, seconds: f64) -> OpenTransaction {
    OpenTransaction {
        session: id.into(),
        seconds,
        program: Some("billing-worker".into()),
        activity: TransactionActivity::IdleInTransaction,
        holder: CleanupHolder::Session,
        ..OpenTransaction::default()
    }
}

#[test]
fn held_cleanup_words() {
    let gb = 1024.0 * 1024.0 * 1024.0;
    let slot = |catalog_only: bool, activity: TransactionActivity| OpenTransaction {
        session: "standby_1".into(),
        activity,
        holder: CleanupHolder::ReplicationSlot,
        xid_age: Some(150_000_000.0),
        catalog_only,
        ..OpenTransaction::default()
    };
    let prepared = |program: Option<&str>, behind: Option<f64>| OpenTransaction {
        session: "tx-17".into(),
        seconds: 7200.0,
        activity: TransactionActivity::Prepared,
        holder: CleanupHolder::PreparedTransaction,
        program: program.map(str::to_string),
        xid_age: behind,
        ..OpenTransaction::default()
    };
    let mut history = with_signals(Engine::Mysql, vec![sig("history-length", 2_100_000.0)]);
    history.pressure.as_mut().unwrap().oldest_transaction =
        Some(OpenTransaction { database: Some("billing".into()), ..session("104", 2.0 * 3600.0) });
    let mut sqlite: Snapshot = serde_json::from_str(include_str!("../../../../../fixtures/sqlite-demo.json")).unwrap();
    sqlite.pressure = Some(Pressure {
        oldest_transaction: Some(OpenTransaction { program: None, ..session("1", 3600.0) }),
        ..Pressure::default()
    });
    pin(
        vec![
            (
                "mssql-version-store",
                said(
                    &HeldCleanupCheck,
                    &holding(
                        Engine::Mssql,
                        OpenTransaction { held_bytes: Some(3.0 * gb), ..session("92", 3.0 * 3600.0) },
                    ),
                ),
            ),
            (
                "mssql-big",
                said(
                    &HeldCleanupCheck,
                    &holding(Engine::Mssql, OpenTransaction { held_bytes: Some(1.5 * gb), ..session("93", 600.0) }),
                ),
            ),
            (
                "mssql-wrote",
                said(
                    &HeldCleanupCheck,
                    &holding(
                        Engine::Mssql,
                        OpenTransaction {
                            wrote: Some(true),
                            activity: TransactionActivity::Running,
                            ..session("71", 3600.0)
                        },
                    ),
                ),
            ),
            (
                "mssql-snapshot-writer",
                said(
                    &HeldCleanupCheck,
                    &holding(
                        Engine::Mssql,
                        OpenTransaction {
                            wrote: Some(true),
                            activity: TransactionActivity::Snapshot,
                            ..session("72", 3600.0)
                        },
                    ),
                ),
            ),
            (
                "mssql-open",
                said(
                    &HeldCleanupCheck,
                    &holding(
                        Engine::Mssql,
                        OpenTransaction { program: None, state: Some("sleeping".into()), ..session("73", 2400.0) },
                    ),
                ),
            ),
            (
                "pg-slot",
                said(&HeldCleanupCheck, &holding(Engine::Postgres, slot(false, TransactionActivity::SlotInactive))),
            ),
            (
                "pg-active-slot",
                said(&HeldCleanupCheck, &holding(Engine::Postgres, slot(false, TransactionActivity::SlotActive))),
            ),
            (
                "pg-catalog-slot",
                said(&HeldCleanupCheck, &holding(Engine::Postgres, slot(true, TransactionActivity::SlotActive))),
            ),
            (
                "pg-prepared",
                said(&HeldCleanupCheck, &holding(Engine::Postgres, prepared(Some("tm-coordinator"), Some(40_000.0)))),
            ),
            ("pg-prepared-bare", said(&HeldCleanupCheck, &holding(Engine::Postgres, prepared(None, None)))),
            (
                "pg-idle",
                said(
                    &HeldCleanupCheck,
                    &holding(
                        Engine::Postgres,
                        OpenTransaction { xid_age: Some(25_000.0), ..session("4242", 5.0 * 3600.0) },
                    ),
                ),
            ),
            (
                "pg-active",
                said(
                    &HeldCleanupCheck,
                    &holding(
                        Engine::Postgres,
                        OpenTransaction {
                            program: None,
                            activity: TransactionActivity::Reported,
                            state: Some("active".into()),
                            ..session("4243", 2400.0)
                        },
                    ),
                ),
            ),
            ("mysql-history", said(&HeldCleanupCheck, &history)),
            (
                "mysql",
                said(
                    &HeldCleanupCheck,
                    &holding(
                        Engine::Mysql,
                        OpenTransaction { program: None, database: Some("shop".into()), ..session("105", 2400.0) },
                    ),
                ),
            ),
            ("sqlite", said(&HeldCleanupCheck, &sqlite)),
        ],
        HELD_CLEANUP,
    );
}

// What each scenario said before its words moved to locales/en.json, which they must still say.
const INFOS: &[(&str, &str)] = &[(
    "infos",
    r#"memory-grant: Queries that reserve far more memory than they use: Statements whose memory grant (reserved before they run, for sorts and hashes) is many times what they use, so other queries queue for memory.
memory-queue: Queries waiting in line for memory: Waiting for memory takes a real amount of time (a minute an hour or more), or queries in this database were queueing for a memory grant when the scan ran.
compile-pressure: Instance compilation context: Shared compilation counters provide context for this database's literal query families; measured Query Store costs establish its compilation impact.
compile-queue: Compiler admission: Requests in this database delayed by compiler admission, with recent per-query compile-memory evidence where available.
worker-pressure: Running out of workers or connections: SQL Server sessions waiting for a worker thread (THREADPOOL), or PostgreSQL/MySQL connections close to max_connections.
held-cleanup: A long open transaction holds cleanup back: The transaction that holds this database's cleanup back: one with uncommitted changes or row versions here (SQL Server), the oldest snapshot, replication slot or prepared transaction behind VACUUM's horizon (PostgreSQL), or the oldest read view behind InnoDB's purge (MySQL).
network-wait: Results the application reads slowly: Queries that spend much of their time waiting for the application to take results (ASYNC_NETWORK_IO), usually because they return large result sets.
tier-limit: Resource use near the cloud tier's limits: Azure SQL Database resources (CPU, data IO, log writes, workers) persistently near their limits in the available selected-period samples.
checkpoint-pressure: Requested checkpoints: The server's requested checkpoints, with the possible causes and the evidence needed before changing WAL settings.
row-lock-waits: Transactions waiting for row locks: InnoDB row-lock waits add up to real time (a minute an hour or more across the server), from long transactions or updates that scan instead of using an index."#,
)];
const CHECKPOINT_PRESSURE: &[(&str, &str)] = &[
    (
        "1gb",
        r#"checkpoint-pressure:max_wal_size · Low · Medium · OptionalImprovement · score 14.20
title: 62% of the server's recorded checkpoints were requested rather than timed
summary: 31 of 50 checkpoints across the whole server were requested rather than timed since the checkpoint counters were reset. Requests can come from WAL reaching max_wal_size (1GB), a manual CHECKPOINT, a base backup or CREATE DATABASE. This counter does not distinguish their causes or measure write latency, so it does not establish that WAL is too small or that this database has write pressure.
impact: 62% requested
metric: Requested checkpoints
metric: Checkpoints
fix: Inspect the checkpoint log (log_checkpoints) during normal load before changing anything. If it confirms frequent checkpoints caused by WAL filling max_wal_size, check free disk space and consider giving WAL more room: Set max_wal_size to 8GB and reload the configuration (settings marked as needing a restart take effect after one). Backups and manual checkpoints need no WAL-size fix.
cost: WAL takes more disk, and crash recovery can take a little longer.
verify: Check checkpoint reasons and write latency during comparable load. After a confirmed WAL-size change, WAL-triggered checkpoints should be less frequent; the requested-checkpoint ratio alone is not a health target."#,
    ),
    (
        "5000mb",
        r#"checkpoint-pressure:max_wal_size · Low · Medium · OptionalImprovement · score 14.20
title: 62% of the server's recorded checkpoints were requested rather than timed
summary: 31 of 50 checkpoints across the whole server were requested rather than timed since the checkpoint counters were reset. Requests can come from WAL reaching max_wal_size (5000MB), a manual CHECKPOINT, a base backup or CREATE DATABASE. This counter does not distinguish their causes or measure write latency, so it does not establish that WAL is too small or that this database has write pressure.
impact: 62% requested
metric: Requested checkpoints
metric: Checkpoints
fix: Inspect the checkpoint log (log_checkpoints) during normal load before changing anything. If it confirms frequent checkpoints caused by WAL filling max_wal_size, check free disk space and consider giving WAL more room: Set max_wal_size to 10000MB and reload the configuration (settings marked as needing a restart take effect after one). Backups and manual checkpoints need no WAL-size fix.
cost: WAL takes more disk, and crash recovery can take a little longer.
verify: Check checkpoint reasons and write latency during comparable load. After a confirmed WAL-size change, WAL-triggered checkpoints should be less frequent; the requested-checkpoint ratio alone is not a health target."#,
    ),
    (
        "unknown",
        r#"checkpoint-pressure:max_wal_size · Low · Medium · OptionalImprovement · score 12.50
title: 45% of the server's recorded checkpoints were requested rather than timed
summary: 90 of 200 checkpoints across the whole server were requested rather than timed since the checkpoint counters were reset. Requests can come from WAL reaching max_wal_size, a manual CHECKPOINT, a base backup or CREATE DATABASE. This counter does not distinguish their causes or measure write latency, so it does not establish that WAL is too small or that this database has write pressure.
impact: 45% requested
metric: Requested checkpoints
metric: Checkpoints
fix: Inspect the checkpoint log (log_checkpoints) during normal load before changing anything. If it confirms frequent checkpoints caused by WAL filling max_wal_size, check free disk space and consider giving WAL more room: Set max_wal_size to 8GB and reload the configuration (settings marked as needing a restart take effect after one). Backups and manual checkpoints need no WAL-size fix.
cost: WAL takes more disk, and crash recovery can take a little longer.
verify: Check checkpoint reasons and write latency during comparable load. After a confirmed WAL-size change, WAL-triggered checkpoints should be less frequent; the requested-checkpoint ratio alone is not a health target."#,
    ),
];
const COMPILE_PRESSURE: &[(&str, &str)] = &[
    (
        "days",
        r#"compile-pressure:server · Low · Medium · OptionalImprovement · score 14.20
title: The SQL Server instance recorded compilation for 31% of batches (about 2.2K a minute)
summary: Since SQL Server started 3 days ago, the instance averaged 120/s batches across all its databases and compiled a new plan for 31% of them, recompiles not counted. This ratio does not identify this database's compiles or measure its CPU pressure. This database has statements with different literal values; its own Query Store compile costs can show whether they warrant work.
impact: 31% of batches compile
metric: Compiles per 100 batches
metric: Compiles per minute
metric: Recompiles per 100 batches
fix: Inspect this database's Query Store compile counts and durations and its query-family findings. If those measurements show costly repeated compilation, parameterize the affected statements. Do not change database-wide parameterization from the instance's ratio alone.
verify: Compare this database's measured compilation costs under comparable load before and after the change. The instance-wide ratio alone cannot verify a fix in this database."#,
    ),
    (
        "hours",
        r#"compile-pressure:server · Low · Medium · OptionalImprovement · score 14.20
title: The SQL Server instance recorded compilation for 31% of batches (about 2.2K a minute)
summary: Since SQL Server started 30 hours ago, the instance averaged 120/s batches across all its databases and compiled a new plan for 31% of them, recompiles not counted. This ratio does not identify this database's compiles or measure its CPU pressure. This database has statements with different literal values; its own Query Store compile costs can show whether they warrant work.
impact: 31% of batches compile
metric: Compiles per 100 batches
metric: Compiles per minute
fix: Inspect this database's Query Store compile counts and durations and its query-family findings. If those measurements show costly repeated compilation, parameterize the affected statements. Do not change database-wide parameterization from the instance's ratio alone.
verify: Compare this database's measured compilation costs under comparable load before and after the change. The instance-wide ratio alone cannot verify a fix in this database."#,
    ),
    (
        "minutes",
        r#"compile-pressure:server · Low · Medium · OptionalImprovement · score 14.20
title: The SQL Server instance recorded compilation for 31% of batches (about 2.2K a minute)
summary: Since SQL Server started 72 minutes ago, the instance averaged 120/s batches across all its databases and compiled a new plan for 31% of them, recompiles not counted. This ratio does not identify this database's compiles or measure its CPU pressure. This database has statements with different literal values; its own Query Store compile costs can show whether they warrant work.
impact: 31% of batches compile
metric: Compiles per 100 batches
metric: Compiles per minute
fix: Inspect this database's Query Store compile counts and durations and its query-family findings. If those measurements show costly repeated compilation, parameterize the affected statements. Do not change database-wide parameterization from the instance's ratio alone.
verify: Compare this database's measured compilation costs under comparable load before and after the change. The instance-wide ratio alone cannot verify a fix in this database."#,
    ),
];
const TIER_LIMIT: &[(&str, &str)] = &[
    (
        "cpu",
        r#"tier-limit:cpu · High · High · OptionalImprovement · score 57.00
title: CPU reached 95% of the tier's limit in 40% of samples over the last 60 minutes
summary: Azure caps each tier's cpu. In the last 60 minutes it was at 95% or more of that cap in 40% of the 15-second samples (peak 99%). This is measured resource use, not a share of waits: queries can queue at the cap. At the cap, Azure makes requests wait rather than fail, so it shows up as slowness. Samples near the cap alone do not prove throttling or failed requests occurred.
impact: near limit 40%
metric: Samples at or above 95%
metric: Peak
fix: Fix the queries that use the most CPU first (Where time goes → CPU lists them); then consider more vCores.
verify: Re-scan: cpu should stay under 95% for most of the time (the Health strip on Dashboard shows it)."#,
    ),
    (
        "data-io",
        r#"tier-limit:data-io · High · High · OptionalImprovement · score 57.00
title: Data reads reached 95% of the tier's limit in 40% of samples over the last 60 minutes
summary: Azure caps each tier's data reads. In the last 60 minutes it was at 95% or more of that cap in 40% of the 15-second samples (peak 99%). This is measured resource use, not a share of waits: reads can be throttled at the cap. At the cap, Azure makes requests wait rather than fail, so it shows up as slowness. Samples near the cap alone do not prove throttling or failed requests occurred.
impact: near limit 40%
metric: Samples at or above 95%
metric: Peak
fix: Cut scans with the missing-index findings and check memory; then consider a tier with more IO.
verify: Re-scan: data reads should stay under 95% for most of the time (the Health strip on Dashboard shows it)."#,
    ),
    (
        "log-write",
        r#"tier-limit:log-write · High · High · OptionalImprovement · score 57.00
title: Log writes reached 95% of the tier's limit in 40% of samples over the last 60 minutes
summary: Azure caps each tier's log writes. In the last 60 minutes it was at 95% or more of that cap in 40% of the 15-second samples (peak 99%). This is measured resource use, not a share of waits: writes can be throttled at the cap. At the cap, Azure makes requests wait rather than fail, so it shows up as slowness. Samples near the cap alone do not prove throttling or failed requests occurred.
impact: near limit 40%
metric: Samples at or above 95%
metric: Peak
fix: Batch small writes into fewer transactions and avoid row-by-row changes; then consider a tier with more log throughput.
verify: Re-scan: log writes should stay under 95% for most of the time (the Health strip on Dashboard shows it)."#,
    ),
    (
        "workers",
        r#"tier-limit:workers · High · High · OptionalImprovement · score 57.00
title: Worker threads reached 95% of the tier's limit in 40% of samples over the last 60 minutes
summary: Azure caps each tier's worker threads. In the last 60 minutes it was at 95% or more of that cap in 40% of the 15-second samples (peak 99%). This is measured resource use, not a share of waits: new requests can be refused at the cap (error 10928). At the cap, Azure refuses new requests with error 10928 (the request limit for the database has been reached) until workers free up, so the application sees errors, not only slowness. Samples near the cap alone do not prove throttling or failed requests occurred.
impact: near limit 40%
metric: Samples at or above 95%
metric: Peak
fix: Look for blocking chains and heavy parallelism (they hold many workers); then consider a bigger tier.
verify: Re-scan: worker threads should stay under 95% for most of the time (the Health strip on Dashboard shows it)."#,
    ),
];
const MEMORY_GRANT: &[(&str, &str)] = &[
    (
        "running",
        r#"memory-grant:7 · High · High · OptionalImprovement · score 45.00
title: A read from customers reserves 2.0 GB of memory per run and uses 40.0 MB
summary: Before each run SQL Server sets aside 2.0 GB to sort and hash, sized by how many rows it expects; runs use 40.0 MB (2%). While a run holds memory it doesn't use, other queries can queue for it (RESOURCE_SEMAPHORE). It ran 40 times since its plan was cached; the largest grant was 3.0 GB.
impact: 2.0 GB reserved, 40.0 MB used
metric: Reserved per run
metric: Used per run
metric: Runs
fix: Fix the row estimate that sizes the grant: refresh statistics on the tables it reads and look in its plan for estimates far above actual rows. On SQL Server 2019+ (compatibility level 150+) memory grant feedback shrinks repeated grants by itself. As a stopgap, cap the grant on this statement.
sql: -- Stopgap, appended to the statement: limit its grant to 5% of query memory
OPTION (MAX_GRANT_PERCENT = 5);
verify: Re-scan: memory reserved per run should be close to memory used."#,
    ),
    (
        "stopped",
        r#"memory-grant:7 · Medium · High · OptionalImprovement · score 22.00
title: A read from customers reserved 2.0 GB of memory per run and used 40.0 MB
summary: Before each run SQL Server sets aside 2.0 GB to sort and hash, sized by how many rows it expects; runs use 40.0 MB (2%). While a run holds memory it doesn't use, other queries can queue for it (RESOURCE_SEMAPHORE). It ran 40 times since its plan was cached; the largest grant was 3.0 GB.
impact: 2.0 GB reserved, 40.0 MB used
metric: Reserved per run
metric: Used per run
metric: Runs
fix: Fix the row estimate that sizes the grant: refresh statistics on the tables it reads and look in its plan for estimates far above actual rows. On SQL Server 2019+ (compatibility level 150+) memory grant feedback shrinks repeated grants by itself. As a stopgap, cap the grant on this statement.
sql: -- Stopgap, appended to the statement: limit its grant to 5% of query memory
OPTION (MAX_GRANT_PERCENT = 5);
verify: Re-scan: memory reserved per run should be close to memory used."#,
    ),
    (
        "unknown",
        r#"memory-grant:0xFFFF · Medium · High · OptionalImprovement · score 22.00
title: A query reserves 2.0 GB of memory per run and uses 40.0 MB
summary: Before each run SQL Server sets aside 2.0 GB to sort and hash, sized by how many rows it expects; runs use 40.0 MB (2%). While a run holds memory it doesn't use, other queries can queue for it (RESOURCE_SEMAPHORE). It ran 40 times since its plan was cached; the largest grant was 3.0 GB.
impact: 2.0 GB reserved, 40.0 MB used
metric: Reserved per run
metric: Used per run
metric: Runs
fix: Fix the row estimate that sizes the grant: refresh statistics on the tables it reads and look in its plan for estimates far above actual rows. On SQL Server 2019+ (compatibility level 150+) memory grant feedback shrinks repeated grants by itself. As a stopgap, cap the grant on this statement.
sql: -- Stopgap, appended to the statement: limit its grant to 5% of query memory
OPTION (MAX_GRANT_PERCENT = 5);
verify: Re-scan: memory reserved per run should be close to memory used."#,
    ),
];
const NETWORK_WAIT: &[(&str, &str)] = &[
    (
        "rows",
        r#"network-wait:7 · Medium · High · OptionalImprovement · score 57.13
title: A read from customers waits on the application 60% of its time while sending 80K rows per call
summary: SQL Server had its results ready but the application took them slowly (ASYNC_NETWORK_IO), so each run holds its locks and memory longer. That is 60% of this query's time. Usually the result set is bigger than needed, or the application does work for each row before reading the next.
impact: 33% of DB time
metric: Waiting on the app
metric: Rows per call
fix: Return fewer rows (filter, or page with OFFSET/FETCH or keyset pagination), or read them faster: stream the results and do per-row work after the read, not while the result set is open.
verify: Re-scan: this query's Sending results (network) share should drop."#,
    ),
    (
        "few-rows",
        r#"network-wait:7 · Medium · High · OptionalImprovement · score 57.13
title: A read from customers waits on the application 60% of its time
summary: SQL Server had its results ready but the application took them slowly (ASYNC_NETWORK_IO), so each run holds its locks and memory longer. That is 60% of this query's time. Usually the result set is bigger than needed, or the application does work for each row before reading the next.
impact: 33% of DB time
metric: Waiting on the app
metric: Rows per call
fix: Return fewer rows (filter, or page with OFFSET/FETCH or keyset pagination), or read them faster: stream the results and do per-row work after the read, not while the result set is open.
verify: Re-scan: this query's Sending results (network) share should drop."#,
    ),
    (
        "no-rows",
        r#"network-wait:7 · Medium · High · OptionalImprovement · score 57.13
title: A read from customers waits on the application 60% of its time
summary: SQL Server had its results ready but the application took them slowly (ASYNC_NETWORK_IO), so each run holds its locks and memory longer. That is 60% of this query's time. Usually the result set is bigger than needed, or the application does work for each row before reading the next.
impact: 33% of DB time
metric: Waiting on the app
fix: Return fewer rows (filter, or page with OFFSET/FETCH or keyset pagination), or read them faster: stream the results and do per-row work after the read, not while the result set is open.
verify: Re-scan: this query's Sending results (network) share should drop."#,
    ),
    (
        "stopped-rows",
        r#"network-wait:7 · Low · High · OptionalImprovement · score 57.13
title: A read from customers waited on the application 60% of its time while sending 80K rows per call
summary: SQL Server had its results ready but the application took them slowly (ASYNC_NETWORK_IO), so each run holds its locks and memory longer. That is 60% of this query's time. Usually the result set is bigger than needed, or the application does work for each row before reading the next.
impact: 33% of DB time
metric: Waiting on the app
metric: Rows per call
fix: Return fewer rows (filter, or page with OFFSET/FETCH or keyset pagination), or read them faster: stream the results and do per-row work after the read, not while the result set is open.
verify: Re-scan: this query's Sending results (network) share should drop."#,
    ),
    (
        "stopped",
        r#"network-wait:7 · Low · High · OptionalImprovement · score 57.13
title: A read from customers waited on the application 60% of its time
summary: SQL Server had its results ready but the application took them slowly (ASYNC_NETWORK_IO), so each run holds its locks and memory longer. That is 60% of this query's time. Usually the result set is bigger than needed, or the application does work for each row before reading the next.
impact: 33% of DB time
metric: Waiting on the app
fix: Return fewer rows (filter, or page with OFFSET/FETCH or keyset pagination), or read them faster: stream the results and do per-row work after the read, not while the result set is open.
verify: Re-scan: this query's Sending results (network) share should drop."#,
    ),
];
const ROW_LOCK_WAITS: &[(&str, &str)] = &[
    (
        "here",
        r#"row-lock-waits:server · Medium · Medium · OptionalImprovement · score 22.00
title: Transactions on this server waited 8 h 10 min for row locks since MySQL started 3 days ago, 1.2 s each on average
summary: Since MySQL started 3 days ago, InnoDB made transactions across the whole server (it counts every database together) wait for row locks 24K times, 8 h 10 min in all and about 1.2 s each: about 7 min an hour on average, against 999% of statement time an hour in this database; the counters don't say when. In this database, a read from orders waited longest on locks (27.6 s in total). Lock waits grow with long transactions, and with UPDATE or DELETE statements whose filter isn't indexed: they lock every row they scan.
impact: 7 min/h lock waits
metric: Lock waits since start
metric: Time waiting since start
metric: Average wait
fix: Keep transactions short (no user input or slow calls inside them), make UPDATE and DELETE filter on indexed columns, and touch tables in the same order everywhere. Live shows who blocks whom right now.
verify: Sample Where time goes → Right now during normal load: it counts row-lock waits over the sample, which should be rare and short.
caveat: MySQL counts row-lock waits for the whole server, so other databases' waits are in these numbers."#,
    ),
    (
        "server",
        r#"row-lock-waits:server · Low · Medium · OptionalImprovement · score 8.00
title: Transactions on this server waited 2 min 15 s for row locks since MySQL started 72 minutes ago, 1.5 s each on average
summary: Since MySQL started 72 minutes ago, InnoDB made transactions across the whole server (it counts every database together) wait for row locks 90 times, 2 min 15 s in all and about 1.5 s each: about 2 min an hour on average; the counters don't say when. Lock waits grow with long transactions, and with UPDATE or DELETE statements whose filter isn't indexed: they lock every row they scan.
impact: 2 min/h lock waits
metric: Lock waits since start
metric: Time waiting since start
metric: Average wait
fix: Keep transactions short (no user input or slow calls inside them), make UPDATE and DELETE filter on indexed columns, and touch tables in the same order everywhere. Live shows who blocks whom right now.
verify: Sample Where time goes → Right now during normal load: it counts row-lock waits over the sample, which should be rare and short.
caveat: MySQL counts row-lock waits for the whole server, so other databases' waits are in these numbers."#,
    ),
];
const MEMORY_QUEUE: &[(&str, &str)] = &[
    (
        "one-pending",
        r#"memory-queue:server · Low · Low · OptionalImprovement · score 8.00
title: 1 query in this database was waiting for a memory grant when the scan ran
summary: When the scan ran, 1 query was waiting for a grant before it could start. A query needs its memory grant to sort or hash before it starts; when the pool is spoken for, the rest wait. The usual cause is a few queries that reserve far more than they use.
impact: 1 waiting
metric: Waiting now
fix: Shrink the biggest over-sized grants first (see the memory-grant findings), then look at plans that sort or hash large scans. Raising max server memory, or the tier, treats the symptom.
verify: Re-scan, or sample Right now in Where time goes: memory waits should fall under a minute an hour and nothing should be waiting for a grant.
caveat: Seen once, when the scan ran, with nothing on record before it: sample Where time goes → Right now a few times before acting on it."#,
    ),
    (
        "pending",
        r#"memory-queue:server · Medium · Low · OptionalImprovement · score 22.00
title: 6 queries in this database were waiting for a memory grant when the scan ran
summary: When the scan ran, 6 queries were waiting for a grant before they could start. A query needs its memory grant to sort or hash before it starts; when the pool is spoken for, the rest wait. The usual cause is a few queries that reserve far more than they use.
impact: 6 waiting
metric: Waiting now
fix: Shrink the biggest over-sized grants first (see the memory-grant findings), then look at plans that sort or hash large scans. Raising max server memory, or the tier, treats the symptom.
verify: Re-scan, or sample Right now in Where time goes: memory waits should fall under a minute an hour and nothing should be waiting for a grant.
caveat: Seen once, when the scan ran, with nothing on record before it: sample Where time goes → Right now a few times before acting on it."#,
    ),
    (
        "grant-history",
        r#"memory-queue:server · Medium · Medium · OptionalImprovement · score 22.00
title: Queries waited 36 h for memory grants since SQL Server started 3 days ago
summary: Since SQL Server started 3 days ago, this database's queries waited 36 h for memory grants (RESOURCE_SEMAPHORE), about 30 min an hour on average; when isn't recorded. A query needs its memory grant to sort or hash before it starts; when the pool is spoken for, the rest wait. The usual cause is a few queries that reserve far more than they use.
impact: 36 h grant waits
metric: Grant waits since start
fix: Shrink the biggest over-sized grants first (see the memory-grant findings), then look at plans that sort or hash large scans. Raising max server memory, or the tier, treats the symptom.
verify: Re-scan, or sample Right now in Where time goes: memory waits should fall under a minute an hour and nothing should be waiting for a grant."#,
    ),
    (
        "windowed",
        r#"memory-queue:server · High · High · OptionalImprovement · score 84.82
title: Waiting for memory takes 100% of all waiting (about 1 h 20 min an hour)
summary: Waits for memory are 100% of all waiting in the last 5 hours, about 1 h 20 min an hour. They include memory grants and memory allocations inside the server; the totals don't say which. Grant waits (RESOURCE_SEMAPHORE) mean queries queue for memory to sort or hash; allocation waits (CMEMTHREAD) mean many sessions contend for one of the server's memory objects, often while compiling.
impact: 100% of waits
metric: Share of waits
metric: Memory waits per hour
fix: Sample Where time goes → Right now while it happens to see which wait it is. For grant waits (RESOURCE_SEMAPHORE), shrink the biggest over-sized grants (the memory-grant findings); for allocation waits (CMEMTHREAD), parameterize one-off statements so fewer compile, and keep parallelism in check.
verify: Re-scan, or sample Right now in Where time goes: memory waits should fall under a minute an hour and nothing should be waiting for a grant."#,
    ),
    (
        "shared",
        r#"memory-queue:server · Medium · High · OptionalImprovement · score 62.00
title: 2 queries in this database were waiting for a memory grant when the scan ran
summary: Waits for memory are 100% of all waiting in the time since SQL Server started 3 days ago, about 5 h an hour, across every database on the instance. They include memory grants and memory allocations inside the server; the totals don't say which. When the scan ran, 2 queries were waiting for a grant before they could start. A query needs its memory grant to sort or hash before it starts; when the pool is spoken for, the rest wait. The usual cause is a few queries that reserve far more than they use.
impact: 100% of waits
metric: Share of waits
metric: Memory waits per hour
metric: Waiting now
fix: Shrink the biggest over-sized grants first (see the memory-grant findings), then look at plans that sort or hash large scans. Raising max server memory, or the tier, treats the symptom.
verify: Re-scan, or sample Right now in Where time goes: memory waits should fall under a minute an hour and nothing should be waiting for a grant."#,
    ),
    (
        "database",
        r#"memory-queue:server · Medium · Medium · OptionalImprovement · score 62.00
title: Waiting for memory takes 100% of all waiting (about 5 h an hour)
summary: Waits for memory are 100% of all waiting in the time since SQL Server started 3 days ago, about 5 h an hour. They include memory grants and memory allocations inside the server; the totals don't say which. Grant waits (RESOURCE_SEMAPHORE) mean queries queue for memory to sort or hash; allocation waits (CMEMTHREAD) mean many sessions contend for one of the server's memory objects, often while compiling.
impact: 100% of waits
metric: Share of waits
metric: Memory waits per hour
fix: Sample Where time goes → Right now while it happens to see which wait it is. For grant waits (RESOURCE_SEMAPHORE), shrink the biggest over-sized grants (the memory-grant findings); for allocation waits (CMEMTHREAD), parameterize one-off statements so fewer compile, and keep parallelism in check.
verify: Re-scan, or sample Right now in Where time goes: memory waits should fall under a minute an hour and nothing should be waiting for a grant."#,
    ),
];
const WORKER_PRESSURE: &[(&str, &str)] = &[
    (
        "waiting",
        r#"worker-pressure:server · Low · Low · OptionalImprovement · score 8.00
title: 2 requests were waiting for a worker thread when the scan ran (THREADPOOL)
summary: When the scan ran, 2 requests were in this database waiting for one. A request that can't get a worker can't even begin, so the app sees timeouts while the server looks idle. It is almost always a symptom: a blocking chain holding many workers, or many parallel queries each taking several.
impact: THREADPOOL waits
metric: Waiting now
fix: Find the head blocker in Live and end the transaction behind the chain; then lower parallelism so each query takes fewer workers (cost threshold for parallelism, MAXDOP). Raising max worker threads only hides it.
verify: Re-scan: THREADPOOL waits should stop growing and nothing should wait for a worker.
caveat: Seen once, when the scan ran, with nothing on record before it: sample Where time goes → Right now a few times before acting on it."#,
    ),
    (
        "one-waiting",
        r#"worker-pressure:server · Low · Low · OptionalImprovement · score 8.00
title: 1 request was waiting for a worker thread when the scan ran (THREADPOOL)
summary: When the scan ran, 1 request was in this database waiting for one. A request that can't get a worker can't even begin, so the app sees timeouts while the server looks idle. It is almost always a symptom: a blocking chain holding many workers, or many parallel queries each taking several.
impact: THREADPOOL waits
metric: Waiting now
fix: Find the head blocker in Live and end the transaction behind the chain; then lower parallelism so each query takes fewer workers (cost threshold for parallelism, MAXDOP). Raising max worker threads only hides it.
verify: Re-scan: THREADPOOL waits should stop growing and nothing should wait for a worker.
caveat: Seen once, when the scan ran, with nothing on record before it: sample Where time goes → Right now a few times before acting on it."#,
    ),
    (
        "history",
        r#"worker-pressure:server · Medium · Medium · OptionalImprovement · score 22.00
title: Requests waited 2 h 24 min for a worker thread since SQL Server started 3 days ago (THREADPOOL)
summary: Since SQL Server started 3 days ago, requests on this instance waited 2 h 24 min in total for a worker thread, about 2 min an hour on average; the counters don't say when. A request that can't get a worker can't even begin, so the app sees timeouts while the server looks idle. It is almost always a symptom: a blocking chain holding many workers, or many parallel queries each taking several.
impact: THREADPOOL waits
metric: Waited since start
fix: Find the head blocker in Live and end the transaction behind the chain; then lower parallelism so each query takes fewer workers (cost threshold for parallelism, MAXDOP). Raising max worker threads only hides it.
verify: Re-scan: THREADPOOL waits should stop growing and nothing should wait for a worker."#,
    ),
    (
        "history-and-queue",
        r#"worker-pressure:server · High · High · OptionalImprovement · score 45.00
title: 6 requests were waiting for a worker thread when the scan ran (THREADPOOL)
summary: Since SQL Server started 3 days ago, requests on this instance waited 2 h 24 min in total for a worker thread, about 2 min an hour on average; the counters don't say when. When the scan ran, 6 requests were in this database waiting for one. A request that can't get a worker can't even begin, so the app sees timeouts while the server looks idle. It is almost always a symptom: a blocking chain holding many workers, or many parallel queries each taking several.
impact: THREADPOOL waits
metric: Waited since start
metric: Waiting now
fix: Find the head blocker in Live and end the transaction behind the chain; then lower parallelism so each query takes fewer workers (cost threshold for parallelism, MAXDOP). Raising max worker threads only hides it.
verify: Re-scan: THREADPOOL waits should stop growing and nothing should wait for a worker."#,
    ),
    (
        "postgres",
        r#"worker-pressure:server · Low · High · OptionalImprovement · score 8.00
title: The whole server's connections were at 92% of max_connections when scanned
summary: This count covers every database on the server, not only this database's application. When every connection slot is taken, new connections are refused and the application errors out. Idle connections still hold a backend process each. Each connection costs memory, so the fix is fewer, busier connections rather than a higher limit.
impact: 92% of connections
metric: Connections in use
fix: Put a connection pooler in front (PgBouncer, or the application's pool with a smaller maximum) and close idle sessions. Raise max_connections only with memory to spare (it needs a restart).
verify: Re-scan: connections in use should stay under 80% of max_connections."#,
    ),
    (
        "mysql-now",
        r#"worker-pressure:server · Low · High · OptionalImprovement · score 8.00
title: The whole server's connections were at 90% of max_connections when scanned
summary: This count covers every database on the server, not only this database's application. When every connection slot is taken, new connections are refused and the application errors out. Idle connections still hold a thread and buffers each. Each connection costs memory, so the fix is fewer, busier connections rather than a higher limit.
impact: 90% of connections
metric: Connections in use
fix: Size the application's connection pools to what the server can run (a few per core), close idle sessions, and use ProxySQL or the cloud proxy if many clients connect. Raise max_connections only with memory to spare.
verify: Re-scan: connections in use should stay under 80% of max_connections."#,
    ),
    (
        "lowered",
        r#"worker-pressure:server · Low · High · OptionalImprovement · score 8.00
title: MySQL reached its connection limit once, 9.0 days ago
summary: This count covers every database on the server, not only this database's application. When every connection slot is taken, new connections are refused and the application errors out. At its busiest it had 104% of today's max_connections open: the limit was lowered after that. Idle connections still hold a thread and buffers each. Each connection costs memory, so the fix is fewer, busier connections rather than a higher limit.
impact: 20% of connections
metric: Connections in use
fix: Size the application's connection pools to what the server can run (a few per core), close idle sessions, and use ProxySQL or the cloud proxy if many clients connect. Raise max_connections only with memory to spare.
verify: Re-scan: connections in use should stay under 80% of max_connections."#,
    ),
    (
        "peak",
        r#"worker-pressure:server · Low · High · OptionalImprovement · score 8.00
title: MySQL reached its connection limit once, since MySQL started 3 days ago
summary: This count covers every database on the server, not only this database's application. When every connection slot is taken, new connections are refused and the application errors out. That was the busiest moment since MySQL started; it isn't near the limit now (20% in use). Idle connections still hold a thread and buffers each. Each connection costs memory, so the fix is fewer, busier connections rather than a higher limit.
impact: 20% of connections
metric: Connections in use
fix: Size the application's connection pools to what the server can run (a few per core), close idle sessions, and use ProxySQL or the cloud proxy if many clients connect. Raise max_connections only with memory to spare.
verify: Re-scan: connections in use should stay under 80% of max_connections."#,
    ),
];
const HELD_CLEANUP: &[(&str, &str)] = &[
    (
        "mssql-version-store",
        r#"held-cleanup:92 · High · High · OptionalImprovement · score 45.00
title: Session 92 has kept a transaction open for 3 h; this database's version store holds 3.0 GB
summary: This transaction uses row versions. The database's version-store size does not measure bytes retained by this transaction. It comes from billing-worker. Keep transactions short.
impact: open 3 h
metric: Open for
metric: Version store
fix: Find out what the application does in that session and make it commit or roll back promptly; transactions should never wait on a person or a slow call. After checking it is safe, a DBA can end the session, which rolls its transaction back.
sql: -- Ends the session and rolls back its open transaction
KILL 92;
cost: Ending the session discards that transaction's uncommitted changes.
verify: Re-scan: the oldest open transaction should be minutes old, not hours."#,
    ),
    (
        "mssql-big",
        r#"held-cleanup:93 · Medium · High · OptionalImprovement · score 22.00
title: Session 93 has kept a transaction open for 10 min; this database's version store holds 1.5 GB
summary: This transaction uses row versions. The database's version-store size does not measure bytes retained by this transaction. It comes from billing-worker. Keep transactions short.
impact: open 10 min
metric: Open for
metric: Version store
fix: Find out what the application does in that session and make it commit or roll back promptly; transactions should never wait on a person or a slow call. After checking it is safe, a DBA can end the session, which rolls its transaction back.
sql: -- Ends the session and rolls back its open transaction
KILL 93;
cost: Ending the session discards that transaction's uncommitted changes.
verify: Re-scan: the oldest open transaction should be minutes old, not hours."#,
    ),
    (
        "mssql-wrote",
        r#"held-cleanup:71 · Medium · High · OptionalImprovement · score 22.00
title: Session 71 has kept a transaction with uncommitted changes open for 1 h
summary: Work logged in this database remains uncommitted. Log growth also depends on SQL Server's reported reuse wait. It comes from billing-worker. Keep transactions short.
impact: open 1 h
metric: Open for
fix: Find out what the application does in that session and make it commit or roll back promptly; transactions should never wait on a person or a slow call. After checking it is safe, a DBA can end the session, which rolls its transaction back.
sql: -- Ends the session and rolls back its open transaction
KILL 71;
cost: Ending the session discards that transaction's uncommitted changes.
verify: Re-scan: the oldest open transaction should be minutes old, not hours."#,
    ),
    (
        "mssql-snapshot-writer",
        r#"held-cleanup:72 · Medium · High · OptionalImprovement · score 22.00
title: Session 72 has kept a transaction with uncommitted changes open for 1 h
summary: This transaction uses row versions. The database's version-store size does not measure bytes retained by this transaction. Work logged in this database remains uncommitted. Log growth also depends on SQL Server's reported reuse wait. It comes from billing-worker. Keep transactions short.
impact: open 1 h
metric: Open for
fix: Find out what the application does in that session and make it commit or roll back promptly; transactions should never wait on a person or a slow call. After checking it is safe, a DBA can end the session, which rolls its transaction back.
sql: -- Ends the session and rolls back its open transaction
KILL 72;
cost: Ending the session discards that transaction's uncommitted changes.
verify: Re-scan: the oldest open transaction should be minutes old, not hours."#,
    ),
    (
        "mssql-open",
        r#"held-cleanup:73 · Medium · High · OptionalImprovement · score 22.00
title: Session 73 has kept a transaction open for 40 min
summary: The transaction remains open; its age alone does not establish blocking or log growth. Keep transactions short.
impact: open 40 min
metric: Open for
fix: Find out what the application does in that session and make it commit or roll back promptly; transactions should never wait on a person or a slow call. After checking it is safe, a DBA can end the session, which rolls its transaction back.
sql: -- Ends the session and rolls back its open transaction
KILL 73;
cost: Ending the session discards that transaction's uncommitted changes.
verify: Re-scan: the oldest open transaction should be minutes old, not hours."#,
    ),
    (
        "pg-slot",
        r#"held-cleanup:standby_1 · High · High · OptionalImprovement · score 45.00
title: Replication slot standby_1 holds VACUUM back by 150.0M transactions
summary: A replication slot with an xmin (a standby with hot_standby_feedback) keeps VACUUM from removing rows deleted or updated after it, in every database. It is inactive: the standby that used it is gone or disconnected, so it never moves on.
impact: 150.0M transactions behind
metric: Transactions behind
fix: Identify the slot's physical standby or logical-decoding consumer and restore its progress. Confirm whether it is intentionally retained before any removal; removing a slot can require rebuilding the consumer and lose its ability to resume.
cost: Preserve a slot needed by a consumer until recovery or replacement is planned.
verify: Re-scan: no replication slot should hold VACUUM back by millions of transactions."#,
    ),
    (
        "pg-active-slot",
        r#"held-cleanup:standby_1 · High · High · OptionalImprovement · score 45.00
title: Replication slot standby_1 holds VACUUM back by 150.0M transactions
summary: A replication slot with an xmin (a standby with hot_standby_feedback) keeps VACUUM from removing rows deleted or updated after it, in every database.
impact: 150.0M transactions behind
metric: Transactions behind
fix: Identify the slot's physical standby or logical-decoding consumer and restore its progress. Confirm whether it is intentionally retained before any removal; removing a slot can require rebuilding the consumer and lose its ability to resume.
cost: Preserve a slot needed by a consumer until recovery or replacement is planned.
verify: Re-scan: no replication slot should hold VACUUM back by millions of transactions."#,
    ),
    (
        "pg-catalog-slot",
        r#"held-cleanup:standby_1 · High · High · OptionalImprovement · score 45.00
title: Replication slot standby_1 holds system-catalog cleanup back by 150.0M transactions
summary: This replication slot's catalog_xmin keeps old system-catalog row versions for logical decoding, 150.0M transactions behind. It does not establish that this slot holds back cleanup of application-table rows. Check the consumer's progress and recovery requirements before changing or removing a slot.
impact: 150.0M transactions behind
metric: Transactions behind
fix: Identify the slot's physical standby or logical-decoding consumer and restore its progress. Confirm whether it is intentionally retained before any removal; removing a slot can require rebuilding the consumer and lose its ability to resume.
cost: Preserve a slot needed by a consumer until recovery or replacement is planned.
verify: Re-scan: no replication slot should hold VACUUM back by millions of transactions."#,
    ),
    (
        "pg-prepared",
        r#"held-cleanup:tx-17 · Medium · High · OptionalImprovement · score 22.00
title: Prepared transaction 'tx-17' has waited 2 h to be committed or rolled back, so VACUUM can't clean up behind it
summary: A prepared transaction (two-phase commit) keeps its locks and VACUUM's horizon until someone commits or rolls it back, and it survives restarts. 40K transactions have started since its snapshot. It was prepared by tm-coordinator.
impact: open 2 h
metric: Open for
metric: Transactions behind
fix: Find what prepared it (the transaction manager or application doing two-phase commit) and have it commit or roll back. If nothing will, a DBA can roll it back.
sql: ROLLBACK PREPARED 'tx-17';
cost: Rolling it back discards its changes.
verify: Re-scan: the oldest open transaction should be minutes old, not hours."#,
    ),
    (
        "pg-prepared-bare",
        r#"held-cleanup:tx-17 · Medium · High · OptionalImprovement · score 22.00
title: Prepared transaction 'tx-17' has waited 2 h to be committed or rolled back, so VACUUM can't clean up behind it
summary: A prepared transaction (two-phase commit) keeps its locks and VACUUM's horizon until someone commits or rolls it back, and it survives restarts.
impact: open 2 h
metric: Open for
fix: Find what prepared it (the transaction manager or application doing two-phase commit) and have it commit or roll back. If nothing will, a DBA can roll it back.
sql: ROLLBACK PREPARED 'tx-17';
cost: Rolling it back discards its changes.
verify: Re-scan: the oldest open transaction should be minutes old, not hours."#,
    ),
    (
        "pg-idle",
        r#"held-cleanup:4242 · High · High · OptionalImprovement · score 45.00
title: Session 4242 has kept a transaction open for 5 h doing nothing, so VACUUM can't clean up behind it
summary: PostgreSQL can't remove rows deleted or updated after this transaction's snapshot, so tables and indexes bloat and scans read dead rows. 25K transactions have started since its snapshot. This one is idle: the application opened it and never committed. It comes from billing-worker.
impact: open 5 h
metric: Open for
metric: Transactions behind
fix: Find out what the application does in that session and make it commit or roll back promptly; transactions should never wait on a person or a slow call. After checking it is safe, a DBA can end the session, which rolls its transaction back.
sql: -- Ends the session and rolls back its open transaction
SELECT pg_terminate_backend(4242);
cost: Ending the session discards that transaction's uncommitted changes.
verify: Re-scan: the oldest open transaction should be minutes old, not hours."#,
    ),
    (
        "pg-active",
        r#"held-cleanup:4243 · Medium · High · OptionalImprovement · score 22.00
title: Session 4243 has kept a transaction open for 40 min, so VACUUM can't clean up behind it
summary: PostgreSQL can't remove rows deleted or updated after this transaction's snapshot, so tables and indexes bloat and scans read dead rows.
impact: open 40 min
metric: Open for
fix: Find out what the application does in that session and make it commit or roll back promptly; transactions should never wait on a person or a slow call. After checking it is safe, a DBA can end the session, which rolls its transaction back.
sql: -- Ends the session and rolls back its open transaction
SELECT pg_terminate_backend(4243);
cost: Ending the session discards that transaction's uncommitted changes.
verify: Re-scan: the oldest open transaction should be minutes old, not hours."#,
    ),
    (
        "mysql-history",
        r#"held-cleanup:104 · Medium · Medium · OptionalImprovement · score 22.00
title: A transaction in session 104 has been open for 2 h; check its locks and read view
summary: InnoDB keeps old versions of changed rows until the oldest read view that might need them closes. Transaction age and isolation level do not establish whether this transaction has acquired that read view, or how much undo it retains. Inspect its read view and locks before attributing whole-server purge history to it. InnoDB's undo history (the whole server's) is 2.1M undo logs long now. It runs in database billing. It comes from billing-worker.
impact: open 2 h
metric: Open for
fix: Find out what the application does in that session and make it commit or roll back promptly; transactions should never wait on a person or a slow call. After checking it is safe, a DBA can end the session, which rolls its transaction back.
sql: -- Ends the session and rolls back its open transaction
KILL 104;
cost: Ending the session discards that transaction's uncommitted changes.
verify: Re-scan: the oldest open transaction should be minutes old, not hours."#,
    ),
    (
        "mysql",
        r#"held-cleanup:105 · Medium · Medium · OptionalImprovement · score 22.00
title: A transaction in session 105 has been open for 40 min; check its locks and read view
summary: InnoDB keeps old versions of changed rows until the oldest read view that might need them closes. Transaction age and isolation level do not establish whether this transaction has acquired that read view, or how much undo it retains. Inspect its read view and locks before attributing whole-server purge history to it.
impact: open 40 min
metric: Open for
fix: Find out what the application does in that session and make it commit or roll back promptly; transactions should never wait on a person or a slow call. After checking it is safe, a DBA can end the session, which rolls its transaction back.
sql: -- Ends the session and rolls back its open transaction
KILL 105;
cost: Ending the session discards that transaction's uncommitted changes.
verify: Re-scan: the oldest open transaction should be minutes old, not hours."#,
    ),
    (
        "sqlite",
        r#"held-cleanup:1 · Medium · High · OptionalImprovement · score 22.00
title: A transaction has been open for 1 h
summary: It holds a lock on the file.
impact: open 1 h
metric: Open for
fix: Find out what the application does in that session and make it commit or roll back promptly; transactions should never wait on a person or a slow call. After checking it is safe, a DBA can end the session, which rolls its transaction back.
sql: -- Ends the session and rolls back its open transaction
-- SQLite has no sessions to end: the application that holds the write lock has to finish its transaction.
cost: Ending the session discards that transaction's uncommitted changes.
verify: Re-scan: the oldest open transaction should be minutes old, not hours."#,
    ),
];
