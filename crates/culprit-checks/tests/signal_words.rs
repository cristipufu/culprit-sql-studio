//! What each signal check says, word for word, in every shape of finding it makes (the demo goldens reach only a few):
//! its words come from locales/en.json, and these pin them, so a change to a message or to the code that says it shows.

use culprit_core::{FileKind, FileScope, Finding, Snapshot};

fn fixture(name: &str) -> Snapshot {
    let path = format!("{}/../../fixtures/{name}.json", env!("CARGO_MANIFEST_DIR"));
    serde_json::from_str(&std::fs::read_to_string(path).expect("fixture")).expect("snapshot")
}

/// What one check finds on `s`, on its own.
fn run(check: &str, s: &Snapshot) -> Vec<Finding> {
    let c = culprit_checks::all().into_iter().find(|c| c.info().id == check).expect("a check");
    c.run(s)
}

/// Everything a finding says, a field a line.
fn words(f: &Finding) -> String {
    let mut out = vec![
        format!("id: {}", f.id),
        format!("title: {}", f.title),
        format!("summary: {}", f.summary),
        format!("impact: {}", f.impact.label),
    ];
    for m in &f.metrics {
        out.push(format!("metric: {}{}", m.label, m.text.as_deref().map(|t| format!(" = {t}")).unwrap_or_default()));
    }
    for q in &f.queries {
        if let Some(note) = &q.note {
            out.push(format!("note: {note}"));
        }
    }
    if let Some(fix) = &f.fix {
        out.push(format!("fix: {}", fix.summary));
        if let Some(cost) = &fix.cost {
            out.push(format!("cost: {cost}"));
        }
        if let Some(sql) = &fix.sql {
            out.push(format!("sql: {sql}"));
        }
    }
    for v in &f.verify {
        out.push(format!("verify: {v}"));
    }
    for c in &f.caveats {
        out.push(format!("caveat: {c}"));
    }
    out.join("\n")
}

/// The text is `expected`, word for word.
#[track_caller]
fn pinned(actual: String, expected: &str) {
    assert!(actual == expected, "what it says changed; it now says:\n---\n{actual}\n---");
}

/// The findings say `expected`, word for word.
#[track_caller]
fn says(findings: &[Finding], expected: &str) {
    pinned(findings.iter().map(words).collect::<Vec<_>>().join("\n\n"), expected);
}

#[test]
fn every_signal_check_describes_itself() {
    let ids = [
        "storage-capacity",
        "sequence-capacity",
        "operational-errors",
        "cluster-health",
        "sqlite-wal",
        "failing-query",
        "planning-time",
        "jit-overhead",
        "log-volume",
        "lock-time",
        "big-result",
        "hot-rows",
        "lock-escalations",
        "last-page-insert",
        "untrusted-constraint",
        "missing-histogram",
        "hot-updates",
        "table-bloat",
        "large-rows",
        "slow-storage",
        "connection-churn",
        "replica-lag",
    ];
    let infos = culprit_checks::infos();
    let actual = ids
        .iter()
        .map(|id| {
            let info = infos.iter().find(|i| i.id == *id).expect("a check");
            format!("{id}: {}\n{}", info.title, info.description)
        })
        .collect::<Vec<_>>()
        .join("\n");
    pinned(
        actual,
        r#"storage-capacity: Files and logs near their allocation limits
Current database file and transaction-log capacity, log reuse reasons, and shared tempdb allocation. Autogrowth room is distinguished from a hard limit.
sequence-capacity: Identity and sequence allocation limits
Non-cycling identities, sequences and auto-increment columns approaching their numeric bounds; cache allocation is not a forecast of remaining requests.
operational-errors: Archive, replication and redo failures
WAL archive failures, logical subscription errors, standby recovery cancellations, InnoDB redo buffer waits, connection handshake failures, and storage page checksum or I/O corruption errors. Counter history is kept separate from newly observed errors.
cluster-health: Galera node readiness and flow control
The current readiness, connectivity and primary-component verdict reported by a Galera node; its historical flow-control fraction is context, not database-specific pressure.
sqlite-wal: SQLite WAL file size and growth
Read-only WAL sidecar metadata and changes between scans; retained allocation is not inferred to be a checkpoint backlog or a blocked reader.
failing-query: Queries that fail or time out
Statements whose runs end in an error or are cancelled by the client (usually a command timeout): Query Store's aborted and exception runs, performance_schema's errors per digest. PostgreSQL keeps no per-statement error count.
planning-time: Queries that spend their time being planned
Statements where planning (PostgreSQL, with pg_stat_statements.track_planning on) or compiling (SQL Server Query Store) takes a large part of their time, usually because the plan isn't reused.
jit-overhead: JIT compilation that takes much of a statement's time
Statements where PostgreSQL's JIT compilation (pg_stat_statements jit_* columns, 15+) takes a large part of their run time.
log-volume: Writes that produce most of the log
Statements that write a large share of the WAL (PostgreSQL 13+) or transaction log (SQL Server 2017+), or many kilobytes of it per row changed. Log volume drives replication lag, backup size and checkpoints. MySQL keeps no redo bytes per statement.
lock-time: Queries that spend their time waiting for locks
Statements that spend a large part of their time waiting for locks other sessions hold: Query Store's lock waits per query, MySQL and MariaDB lock time per digest. PostgreSQL keeps no lock time per statement; Live shows blocking as it happens.
big-result: Queries that return huge results
Reads that send tens of thousands of rows to the application on every run, with an estimate in bytes from the plan's row width where the engine gives one.
hot-rows: Rows that many sessions fight over
Indexes where sessions waited a long time for row and page locks, and where locks escalated to the whole table (sys.dm_db_index_operational_stats). MySQL, MariaDB and PostgreSQL keep no lock waits per table; see lock-time and Live.
lock-escalations: Statements that lock a whole table
Tables where one statement took so many row or page locks that SQL Server replaced them with a lock on the whole table (sys.dm_db_index_operational_stats). MySQL, MariaDB and PostgreSQL never escalate row locks.
last-page-insert: Inserts that queue for the last page
Indexes keyed on an ever-increasing value (identity, sequence, insert time) where concurrent inserts wait for the latch on the last page (PAGELATCH_EX).
untrusted-constraint: Unvalidated or disabled constraints
SQL Server foreign keys and check constraints added or re-enabled WITH NOCHECK (or switched off), PostgreSQL CHECK constraints added NOT VALID, and YugabyteDB CHECK or foreign-key constraints awaiting validation.
missing-histogram: Filter columns the optimizer has to guess about
Columns that heavy queries filter on with no index leading on them and no histogram (MySQL 8 COLUMN_STATISTICS, MariaDB mysql.column_stats), so the optimizer assumes a fixed share of rows match.
hot-updates: Updates that rewrite every index
Tables where few updates are HOT (heap-only): each other update writes a new entry in every index and leaves cleanup for VACUUM. Says whether pages are too full (fillfactor) or an indexed column changes (PostgreSQL 16+ tells them apart).
table-bloat: Tables that are mostly empty space
Tables whose pages hold far fewer rows than they could, estimated from row counts and column widths in pg_stats (no extension needed). Every scan still reads the empty space.
large-rows: Tables whose rows are mostly large values
Tables whose rows average many kilobytes, most of it in large values kept apart from the rows (SQL Server's LOB pages, PostgreSQL's TOAST): every statement that reads whole rows pulls them in, every write that sets them writes them again. From the catalog's page counts; no data is read.
slow-storage: Storage that's slow to read or write
Average read and write latency per database file since startup: sys.dm_io_virtual_file_stats, pg_stat_io (16+, with track_io_timing), performance_schema file I/O.
connection-churn: Applications that connect for every request
New connections per second since startup (SQL Server logins, PostgreSQL 14+ sessions, MySQL and MariaDB Connections), with how long sessions last and how many came from a pool where the engine says.
replica-lag: Replicas that fall behind
Replicas far behind the primary or stopped, and PostgreSQL replication slots that keep WAL for a consumer that's gone: pg_stat_replication, availability group replica states, SHOW REPLICA STATUS."#,
    );
}

// ── storage-capacity ───────────────────────────────────────────────────────────────────────────────

fn space(kind: FileKind, scope: FileScope, total_gb: f64, used_gb: f64) -> culprit_core::operations::SpaceUsage {
    const GB: f64 = 1024.0 * 1024.0 * 1024.0;
    culprit_core::operations::SpaceUsage {
        parts: Vec::new(),
        name: "shop".into(),
        kind,
        scope,
        total_bytes: total_gb * GB,
        used_bytes: used_gb * GB,
        growth_enabled: None,
        limit_bytes: None,
        reuse_wait: None,
    }
}

fn storage_capacity(spaces: Vec<culprit_core::operations::SpaceUsage>) -> Vec<Finding> {
    let mut s = fixture("mssql-demo");
    s.operations.spaces = spaces;
    run("storage-capacity", &s)
}

#[test]
fn storage_capacity_at_a_hard_limit() {
    let mut data = space(FileKind::Data, FileScope::Database, 100.0, 97.0);
    data.growth_enabled = Some(false);
    data.parts =
        vec![culprit_core::operations::SpacePart { name: "shop_data".into(), bytes: 97.0 * 1024.0 * 1024.0 * 1024.0 }];
    says(
        &storage_capacity(vec![data]),
        r#"id: storage-capacity:shop
title: shop uses 97.0 GB of 100 GB available allocation
summary: Measured when the scan ran: 3.0 GB free in this allocation. The files cannot grow beyond this allocation under their current settings.  This does not measure free filesystem space or predict when it will fill.
impact: 97% occupied
metric: Used
metric: Allocation
metric: Free in allocation
metric: shop_data
fix: Check filesystem headroom and the file's growth/max-size configuration. Plan added capacity or appropriate retention with the owner; do not remove data or shrink files merely to clear this observation.
verify: Compare used/free allocation, growth settings and any log reuse reason in subsequent scans during representative load. Confirm writes can proceed and filesystem space remains available."#,
    );
}

#[test]
fn storage_capacity_of_a_log_held_by_its_reuse_reason() {
    let mut log = space(FileKind::Log, FileScope::Database, 10.0, 9.8);
    log.name = "shop_log".into();
    log.limit_bytes = Some(10.0 * 1024.0 * 1024.0 * 1024.0);
    log.reuse_wait = Some("LOG_BACKUP".into());
    says(
        &storage_capacity(vec![log]),
        r#"id: storage-capacity:shop_log
title: shop_log uses 9.8 GB of 10.0 GB available allocation
summary: Measured when the scan ran: 205 MB free in this allocation. The files cannot grow beyond this allocation under their current settings. SQL Server currently reports log_reuse_wait_desc = LOG_BACKUP.  This does not measure free filesystem space or predict when it will fill.
impact: 98% occupied
metric: Used
metric: Allocation
metric: Free in allocation
fix: Inspect the reported reuse reason: restore scheduled log backups for LOG_BACKUP, examine the owning transaction for ACTIVE_TRANSACTION, or repair the affected replication consumer. Confirm retention/recovery requirements and real disk headroom before increasing limits. Shrinking the log does not repair retained log records.
verify: Compare used/free allocation, growth settings and any log reuse reason in subsequent scans during representative load. Confirm writes can proceed and filesystem space remains available."#,
    );
}

#[test]
fn storage_capacity_of_shared_tempdb_with_room_to_grow() {
    let mut temp = space(FileKind::Temp, FileScope::Server, 10.0, 9.5);
    temp.name = "tempdb".into();
    temp.limit_bytes = Some(10.2 * 1024.0 * 1024.0 * 1024.0);
    let mut grown = space(FileKind::Data, FileScope::Database, 10.0, 9.5);
    grown.growth_enabled = Some(true);
    says(
        &storage_capacity(vec![temp, grown]),
        r#"id: storage-capacity:tempdb
title: tempdb uses 9.5 GB of 10.2 GB configured capacity
summary: Measured when the scan ran: 717 MB free in this allocation. These files are shared by the whole instance; this does not attribute their use to the selected database. Growth settings or additional storage may permit more space. Allocated space being mostly occupied alone is not an incident.  This does not measure free filesystem space or predict when it will fill.
impact: 93% occupied
metric: Used
metric: Allocation
metric: Free in allocation
fix: Inspect tempdb's user objects, internal objects and version-store allocation and the sessions using them. Check growth settings and disk headroom. Do not attribute shared use to this database without per-session evidence.
verify: Compare used/free allocation, growth settings and any log reuse reason in subsequent scans during representative load. Confirm writes can proceed and filesystem space remains available.

id: storage-capacity:shop
title: shop uses 9.5 GB of 10.0 GB currently allocated space
summary: Measured when the scan ran: 512 MB free in this allocation. Growth settings or additional storage may permit more space. Allocated space being mostly occupied alone is not an incident.  This does not measure free filesystem space or predict when it will fill.
impact: 95% occupied
metric: Used
metric: Allocation
metric: Free in allocation
fix: Check filesystem headroom and the file's growth/max-size configuration. Plan added capacity or appropriate retention with the owner; do not remove data or shrink files merely to clear this observation.
verify: Compare used/free allocation, growth settings and any log reuse reason in subsequent scans during representative load. Confirm writes can proceed and filesystem space remains available."#,
    );
}

// ── sqlite-wal ─────────────────────────────────────────────────────────────────────────────────────

fn sqlite_wal(wal_mb: f64, before_mb: Option<f64>) -> Vec<Finding> {
    let mut s = fixture("sqlite-demo");
    s.operations.sqlite = Some(culprit_core::operations::SqliteFileHealth {
        wal_bytes: Some(wal_mb * 1024.0 * 1024.0),
        before: before_mb.map(|mb| culprit_core::operations::CounterReading {
            at: "2026-10-16T09:00:00Z".into(),
            count: mb * 1024.0 * 1024.0,
            reference: None,
        }),
    });
    run("sqlite-wal", &s)
}

#[test]
fn sqlite_wal_without_an_earlier_size() {
    says(
        &sqlite_wal(80.0, None),
        r#"id: sqlite-wal:demo.db
title: The SQLite WAL file occupies 80.0 MB
summary: The sidecar's allocated file length was read without checkpointing or inspecting contents. No earlier comparable size was recorded. A WAL can retain space after checkpointing and reuse it. Size or growth alone does not establish uncheckpointed work, checkpoint starvation, a long reader or disk exhaustion.
impact: File allocation context
metric: WAL file length
fix: If this accompanies capacity or latency trouble, inspect the application's checkpoint results and open read transactions on its own connections. Confirm filesystem space and checkpoint policy. Do not delete the WAL or force a checkpoint just to reduce this number; live backups must preserve a consistent database/WAL state.
verify: Compare WAL allocation across scans and correlate it with application checkpoint progress, latency and filesystem headroom. Reuse of a stable allocated WAL can be healthy; shrinking is not required."#,
    );
}

#[test]
fn sqlite_wal_that_grew_and_one_that_shrank() {
    let mut both = sqlite_wal(40.0, Some(10.0));
    both.extend(sqlite_wal(70.0, Some(90.0)));
    says(
        &both,
        r#"id: sqlite-wal:demo.db
title: The SQLite WAL file occupies 40.0 MB
summary: The sidecar's allocated file length was read without checkpointing or inspecting contents. It changed by 30.0 MB since 2026-10-16T09:00:00Z. A WAL can retain space after checkpointing and reuse it. Size or growth alone does not establish uncheckpointed work, checkpoint starvation, a long reader or disk exhaustion.
impact: File allocation context
metric: WAL file length
fix: If this accompanies capacity or latency trouble, inspect the application's checkpoint results and open read transactions on its own connections. Confirm filesystem space and checkpoint policy. Do not delete the WAL or force a checkpoint just to reduce this number; live backups must preserve a consistent database/WAL state.
verify: Compare WAL allocation across scans and correlate it with application checkpoint progress, latency and filesystem headroom. Reuse of a stable allocated WAL can be healthy; shrinking is not required.

id: sqlite-wal:demo.db
title: The SQLite WAL file occupies 70.0 MB
summary: The sidecar's allocated file length was read without checkpointing or inspecting contents. It changed by −20.0 MB since 2026-10-16T09:00:00Z. A WAL can retain space after checkpointing and reuse it. Size or growth alone does not establish uncheckpointed work, checkpoint starvation, a long reader or disk exhaustion.
impact: File allocation context
metric: WAL file length
fix: If this accompanies capacity or latency trouble, inspect the application's checkpoint results and open read transactions on its own connections. Confirm filesystem space and checkpoint policy. Do not delete the WAL or force a checkpoint just to reduce this number; live backups must preserve a consistent database/WAL state.
verify: Compare WAL allocation across scans and correlate it with application checkpoint progress, latency and filesystem headroom. Reuse of a stable allocated WAL can be healthy; shrinking is not required."#,
    );
}

// ── operational-errors ─────────────────────────────────────────────────────────────────────────────

#[test]
fn operational_errors_of_every_kind() {
    use culprit_core::operations::{CounterReading, OperationalCounter, OperationalCounterKind as Kind};
    let counter = |kind: Kind, subject: &str, scope: FileScope, count: f64, before: Option<f64>| OperationalCounter {
        kind,
        subject: subject.into(),
        scope,
        count,
        since: None,
        last_event_at: None,
        before: before.map(|count| CounterReading { at: "2026-10-11T10:54:21Z".into(), count, reference: None }),
    };
    let mut s = fixture("postgres-demo");
    s.operations.counters = vec![
        OperationalCounter {
            since: Some("2026-10-01T00:00:00Z".into()),
            ..counter(Kind::ArchiveFailures, "archiver", FileScope::Server, 3.0, None)
        },
        counter(Kind::SubscriptionApplyErrors, "sub_orders", FileScope::Database, 50.0, Some(20.0)),
        counter(Kind::SubscriptionSyncErrors, "sub_items", FileScope::Database, 5.0, None),
        counter(Kind::StandbyConflicts, "demo", FileScope::Database, 12.0, Some(10.0)),
        counter(Kind::RedoLogWaits, "server", FileScope::Server, 1_000.0, Some(900.0)),
        OperationalCounter {
            last_event_at: Some("2026-10-11T09:00:00Z".into()),
            ..counter(Kind::ChecksumFailures, "demo", FileScope::Database, 2.0, None)
        },
        counter(Kind::ConnectionFailures, "server", FileScope::Server, 40.0, None),
    ];
    says(
        &run("operational-errors", &s),
        r#"id: operational-errors:archive:archiver
title: WAL archive failures: 3 recorded in the source's history
summary: archiver The archive command or library failed. This is a shared server counter, and a later successful archive may have resolved the incident. The total covers since 2026-10-01T00:00:00Z. It is context for the whole server and is not graded as a problem in this database.
impact: Historical counter
metric: Source total
fix: Check current archiver progress and server logs for the failure, archive destination capacity and access. Confirm retained WAL and backup/recovery requirements; never remove WAL files to clear the counter.
verify: Compare two scans over a stated interval: new errors or waits should stop increasing, and the relevant worker/archive or application operations should succeed. Lifetime totals need not return to zero.

id: operational-errors:subscription-apply:sub_orders
title: Logical replication apply errors: 30 newly counted between scans
summary: sub_orders The subscriber could not apply some incoming changes. Counters do not identify the conflicting row or establish that replication is still stopped. Its statistics period is unknown. The comparison covers 2026-10-11T10:54:21Z to 2026-10-11T11:54:21Z. These are newly recorded events over the stated interval, not proof that the cause persists now.
impact: 30 between scans
metric: Source total
metric: New events between scans
fix: Inspect the subscription worker state and server log. Resolve the reported schema, constraint or data conflict with the replication owner, then verify apply resumes. Do not skip transactions without proving correctness.
verify: Compare two scans over a stated interval: new errors or waits should stop increasing, and the relevant worker/archive or application operations should succeed. Lifetime totals need not return to zero.

id: operational-errors:subscription-sync:sub_items
title: Logical replication synchronization errors: 5 recorded in the source's history
summary: sub_items Initial table synchronization failed. A retained error count does not establish that the table is still unsynchronized. Its statistics period is unknown. No comparable counter interval inside the requested period establishes new failures; this is historical information.
impact: Historical counter
metric: Source total
fix: Inspect subscription relation states and the worker's server-log error. Repair permissions, schema or capacity as indicated, then verify affected tables finish synchronization.
verify: Compare two scans over a stated interval: new errors or waits should stop increasing, and the relevant worker/archive or application operations should succeed. Lifetime totals need not return to zero.

id: operational-errors:standby-conflicts:demo
title: Standby queries cancelled for recovery: 2 newly counted between scans
summary: demo Recovery cancelled queries whose snapshots, locks or buffers conflicted with replay. This records actual cancellations, not ordinary replica lag or a claim that every read failed. Its statistics period is unknown. The comparison covers 2026-10-11T10:54:21Z to 2026-10-11T11:54:21Z. These are newly recorded events over the stated interval, not proof that the cause persists now.
impact: 2 between scans
metric: Source total
metric: New events between scans
fix: Correlate cancelled requests with recovery-conflict logs and query duration. Consider shorter reads or a suitable reporting replica; feedback and longer replay delays trade cancellations for primary bloat, retained WAL or replica lag.
verify: Compare two scans over a stated interval: new errors or waits should stop increasing, and the relevant worker/archive or application operations should succeed. Lifetime totals need not return to zero.

id: operational-errors:redo-waits:server
title: Redo buffer waits (whole server): 100 newly counted between scans
summary: server InnoDB waited for redo buffer space to become available. This counter does not measure wait duration or prove that this database's requests were delayed. Its statistics period is unknown. The comparison covers 2026-10-11T10:54:21Z to 2026-10-11T11:54:21Z. It is context for the whole server and is not graded as a problem in this database.
impact: 100 between scans
metric: Source total
metric: New events between scans
fix: Compare counter growth, redo write/flush latency and representative write activity across the server. Inspect transaction size, log-buffer capacity and storage before testing a setting change; preserve durability requirements.
verify: Compare two scans over a stated interval: new errors or waits should stop increasing, and the relevant worker/archive or application operations should succeed. Lifetime totals need not return to zero.

id: operational-errors:checksum-failures:demo
title: Data page checksum or I/O corruption: 2 recorded in the source's history
summary: demo The storage engine recorded data pages that failed checksum verification or suffered a torn-page / I/O read error in this database. Its statistics period is unknown. Any recorded page checksum or I/O corruption in this database warrants immediate investigation of the server error log and backups.
impact: 2 corrupt pages
metric: Source total
metric: Latest recorded event = 2026-10-11T09:00:00Z
fix: Inspect the server error log immediately to identify the affected relation or file, verify recent backups, and check underlying storage health before running repair or restore procedures.
verify: Compare two scans over a stated interval: new errors or waits should stop increasing, and the relevant worker/archive or application operations should succeed. Lifetime totals need not return to zero.

id: operational-errors:connection-failures:server
title: Failed client connection handshakes (whole server): 40 recorded in the source's history
summary: server Clients attempted to connect to the server and failed during authentication, TLS negotiation or packet read (Aborted_connects). This is a shared server counter. Its statistics period is unknown. It is context for the whole server and is not graded as a problem in this database.
impact: Historical counter
metric: Source total
fix: Inspect server connection logs and client configuration for bad credentials, TLS mismatches, connect_timeout settings, or health checks that close TCP sockets without completing the handshake.
verify: Compare two scans over a stated interval: new errors or waits should stop increasing, and the relevant worker/archive or application operations should succeed. Lifetime totals need not return to zero."#,
    );
}
