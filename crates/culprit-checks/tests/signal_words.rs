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

// ── cluster-health ─────────────────────────────────────────────────────────────────────────────────

#[test]
fn cluster_health_of_a_node_out_of_the_component_and_of_flow_control() {
    use culprit_core::operations::ClusterHealth;
    let mut s = fixture("mysql-demo");
    s.operations.cluster = Some(ClusterHealth {
        ready: Some(false),
        connected: Some(true),
        primary_component: None,
        local_state: Some("Donor/Desynced".into()),
        flow_control_paused: Some(0.2),
    });
    let mut findings = run("cluster-health", &s);
    s.operations.cluster = Some(ClusterHealth {
        ready: Some(true),
        connected: Some(true),
        primary_component: Some(true),
        local_state: None,
        flow_control_paused: Some(0.3),
    });
    findings.extend(run("cluster-health", &s));
    says(
        &findings,
        r#"id: cluster-health:shop
title: This Galera node reports that it is not ready, disconnected or outside the primary component
summary: Measured node status: ready no, connected yes, primary component unknown. Local state: Donor/Desynced. This is the node's own availability verdict; it can reject application operations. Check whether it is intentionally being maintained before intervening.
impact: Node availability
metric: Recorded flow-control paused share
fix: Inspect cluster membership, node logs and state transfer with the cluster owner. Check network and capacity, and route traffic only to suitable nodes. Do not bootstrap a component, disable flow control or force quorum from this observation alone.
verify: Verify the intended node rejoins the primary component, reports ready and connected, and serves application operations. Compare flow control over a fresh known interval before diagnosing sustained pressure.

id: cluster-health:shop
title: Galera recorded time paused for flow control (whole cluster context)
summary: Measured node status: ready yes, connected yes, primary component yes. Local state: not recorded. Donor or desynchronized states can be deliberate. The retained flow-control ratio does not establish pressure or delays in this database now.
impact: Historical cluster context
metric: Recorded flow-control paused share
fix: Inspect cluster membership, node logs and state transfer with the cluster owner. Check network and capacity, and route traffic only to suitable nodes. Do not bootstrap a component, disable flow control or force quorum from this observation alone.
verify: Verify the intended node rejoins the primary component, reports ready and connected, and serves application operations. Compare flow control over a fresh known interval before diagnosing sustained pressure."#,
    );
}

// ── jit-overhead ───────────────────────────────────────────────────────────────────────────────────

/// `s` with one application statement left, the heaviest, made `text` with no costs: the one the query checks are
/// planted on.
fn only<'s>(s: &'s mut Snapshot, text: &str) -> &'s mut culprit_core::QueryStat {
    let w = s.workload.as_mut().expect("workload");
    w.queries.sort_by(|a, b| b.total_time_ms.total_cmp(&a.total_time_ms));
    let keep = w.queries.iter().position(|q| !q.text.to_ascii_lowercase().contains("pg_catalog")).expect("a query");
    let q = w.queries.swap_remove(keep);
    w.queries = vec![q];
    let q = &mut w.queries[0];
    q.text = text.into();
    q.costs = Some(Default::default());
    q
}

/// Make `s` a window of three days in which its statement last ran 30 hours before the end: it stopped running.
fn stopped(s: &mut Snapshot) {
    use culprit_core::format::{epoch_seconds, iso_from_epoch};
    let w = s.workload.as_mut().expect("workload");
    let end = w.window_end.clone().unwrap_or_else(|| s.taken_at.clone());
    w.window_end = Some(end.clone());
    crate::last_hours(w, 72.0);
    let end = epoch_seconds(&end).expect("a window end");
    for q in &mut w.queries {
        q.first_seen = Some(iso_from_epoch(end - 72.0 * 3600.0));
        q.last_seen = Some(iso_from_epoch(end - 30.0 * 3600.0));
    }
}

fn set(s: &mut Snapshot, name: &str, value: &str) {
    s.settings.retain(|x| x.name != name);
    s.settings.push(culprit_core::Setting {
        name: name.into(),
        value: value.into(),
        default_value: None,
        scope: "server".into(),
    });
}

fn jit(jit_ms: f64) -> Snapshot {
    let mut s = fixture("postgres-demo");
    let q = only(&mut s, "SELECT count(*) FROM orders WHERE status = $1");
    (q.executions, q.total_time_ms, q.mean_time_ms) = (100.0, 4_000.0, 40.0);
    q.costs.as_mut().unwrap().jit_ms = Some(jit_ms);
    s
}

#[test]
fn jit_longer_than_the_run_with_no_threshold_read() {
    says(
        &run("jit-overhead", &jit(2_500.0)),
        r#"id: jit-overhead:9065391607317686058
title: JIT compilation takes longer than running a read from orders
summary: PostgreSQL spent 25 ms of its 40 ms per run compiling parts of this statement to machine code (2.5 s in all since its statistics began 5 hours ago). JIT is chosen at plan time when the estimated cost exceeds the effective jit_above_cost. Culprit did not read jit_above_cost. The application's role, database or session can use another setting, and a cached plan can keep a decision made before this scan. JIT pays off for long analytical queries; on short runs its compilation can cost more than it saves. These totals alone do not show that the planner overestimated the rows.
impact: 62% JIT
metric: JIT share
metric: JIT per run
metric: Run time
fix: Turn JIT off for the application's role, or raise jit_above_cost in the scope that enables it. This scan does not establish that threshold for the application, so it cannot choose a higher value safely. Check the role, database and session settings before changing it; a role's new connections use the SQL below unless the application overrides it.
cost: This affects every statement that role runs, including analytical queries that may benefit from JIT.
sql: -- For the role the application connects as (takes effect on its next connections):
ALTER ROLE <app_role> SET jit = off;
verify: Once the application's connections use the change, its JIT time per run should drop to zero or near it between two scans: pg_stat_statements keeps running totals, so compare its JIT time and runs in a scan taken once the change is live and one a day or more later (2.5 s over 100 runs in this scan)."#,
    );
}

#[test]
fn jit_a_share_of_the_run_with_the_threshold_read() {
    let mut s = jit(1_200.0);
    set(&mut s, "jit_above_cost", "100000");
    says(
        &run("jit-overhead", &s),
        r#"id: jit-overhead:9065391607317686058
title: JIT compilation takes 30% of the time of a read from orders
summary: PostgreSQL spent 12 ms of its 40 ms per run compiling parts of this statement to machine code (1.2 s in all since its statistics began 5 hours ago). JIT is chosen at plan time when the estimated cost exceeds the effective jit_above_cost. jit_above_cost is 100000 in Culprit's session. The application's role, database or session can use another setting, and a cached plan can keep a decision made before this scan. JIT pays off for long analytical queries; on short runs its compilation can cost more than it saves. These totals alone do not show that the planner overestimated the rows.
impact: 30% JIT
metric: JIT share
metric: JIT per run
metric: Run time
fix: jit_above_cost is 100K in Culprit's session; first check the application's effective threshold. If it uses this value, raise it so JIT starts only for more expensive plans: Set jit_above_cost to 1000000 and reload the configuration (settings marked as needing a restart take effect after one). Apply the change in the role, database or session instead if that is where the application's threshold is set. Or turn JIT off for the application's role (the alternative SQL), whose new connections use it unless the application overrides it.
sql: -- Only if the application uses the threshold Culprit read, with no role, database or session override:
ALTER SYSTEM SET jit_above_cost = 1000000;
SELECT pg_reload_conf();
-- Alternative, for the application's role only:
-- ALTER ROLE <app_role> SET jit = off;
verify: Once the application's connections use the change, its JIT time per run should drop to zero or near it between two scans: pg_stat_statements keeps running totals, so compare its JIT time and runs in a scan taken once the change is live and one a day or more later (1.2 s over 100 runs in this scan)."#,
    );
}

#[test]
fn jit_that_stopped_on_a_managed_server() {
    let mut s = jit(1_200.0);
    set(&mut s, "jit_above_cost", "500000");
    s.server.set_platform(Some(culprit_core::Platform::AmazonRds));
    stopped(&mut s);
    says(
        &run("jit-overhead", &s),
        r#"id: jit-overhead:9065391607317686058
title: JIT compilation took 30% of the time of a read from orders
summary: PostgreSQL spent 12 ms of its 40 ms per run compiling parts of this statement to machine code (1.2 s in all in the last 3 days). JIT is chosen at plan time when the estimated cost exceeds the effective jit_above_cost. jit_above_cost is 500000 in Culprit's session. The application's role, database or session can use another setting, and a cached plan can keep a decision made before this scan. JIT pays off for long analytical queries; on short runs its compilation can cost more than it saves. These totals alone do not show that the planner overestimated the rows.
impact: 30% JIT
metric: JIT share
metric: JIT per run
metric: Run time
fix: jit_above_cost is 500K in Culprit's session; first check the application's effective threshold. If it uses this value, raise it so JIT starts only for more expensive plans: Set jit_above_cost to 5000000 for this database only, as its owner: sessions that connect after the change use it, and other databases on the server keep theirs. To change it for every database on the server, set it in the instance's DB parameter group. Apply the change in the role, database or session instead if that is where the application's threshold is set. Or turn JIT off for the application's role (the alternative SQL), whose new connections use it unless the application overrides it.
sql: -- Only if the application uses the threshold Culprit read, with no role, database or session override:
ALTER DATABASE demo SET jit_above_cost = 5000000;
-- Alternative, for the application's role only:
-- ALTER ROLE <app_role> SET jit = off;
verify: Re-scan: JIT time per run should drop to zero or near it, and the average run time with it."#,
    );
}

#[test]
fn jit_over_a_log_and_over_a_window_of_unknown_length() {
    let mut s = jit(2_500.0);
    s.workload.as_mut().unwrap().log = Some("slow query".into());
    let mut findings = run("jit-overhead", &s);
    let mut s = jit(2_500.0);
    s.workload.as_mut().unwrap().window_hours = 0.0;
    findings.extend(run("jit-overhead", &s));
    says(
        &findings,
        r#"id: jit-overhead:9065391607317686058
title: JIT compilation takes longer than running a read from orders
summary: PostgreSQL spent 25 ms of its 40 ms per run compiling parts of this statement to machine code (2.5 s in all over the 5 hours the slow query log covers). JIT is chosen at plan time when the estimated cost exceeds the effective jit_above_cost. Culprit did not read jit_above_cost. The application's role, database or session can use another setting, and a cached plan can keep a decision made before this scan. JIT pays off for long analytical queries; on short runs its compilation can cost more than it saves. These totals alone do not show that the planner overestimated the rows.
impact: 62% JIT
metric: JIT share
metric: JIT per run
metric: Run time
fix: Turn JIT off for the application's role, or raise jit_above_cost in the scope that enables it. This scan does not establish that threshold for the application, so it cannot choose a higher value safely. Check the role, database and session settings before changing it; a role's new connections use the SQL below unless the application overrides it.
cost: This affects every statement that role runs, including analytical queries that may benefit from JIT.
sql: -- For the role the application connects as (takes effect on its next connections):
ALTER ROLE <app_role> SET jit = off;
verify: Once the application's connections use the change, its JIT time per run should drop to zero or near it between two scans: pg_stat_statements keeps running totals, so compare its JIT time and runs in a scan taken once the change is live and one a day or more later (2.5 s over 100 runs in this scan).

id: jit-overhead:9065391607317686058
title: JIT compilation takes longer than running a read from orders
summary: PostgreSQL spent 25 ms of its 40 ms per run compiling parts of this statement to machine code (2.5 s in all since its statistics began). JIT is chosen at plan time when the estimated cost exceeds the effective jit_above_cost. Culprit did not read jit_above_cost. The application's role, database or session can use another setting, and a cached plan can keep a decision made before this scan. JIT pays off for long analytical queries; on short runs its compilation can cost more than it saves. These totals alone do not show that the planner overestimated the rows.
impact: 62% JIT
metric: JIT share
metric: JIT per run
metric: Run time
fix: Turn JIT off for the application's role, or raise jit_above_cost in the scope that enables it. This scan does not establish that threshold for the application, so it cannot choose a higher value safely. Check the role, database and session settings before changing it; a role's new connections use the SQL below unless the application overrides it.
cost: This affects every statement that role runs, including analytical queries that may benefit from JIT.
sql: -- For the role the application connects as (takes effect on its next connections):
ALTER ROLE <app_role> SET jit = off;
verify: Once the application's connections use the change, its JIT time per run should drop to zero or near it between two scans: pg_stat_statements keeps running totals, so compare its JIT time and runs in a scan taken once the change is live and one a day or more later (2.5 s over 100 runs in this scan)."#,
    );
}

// ── planning-time ──────────────────────────────────────────────────────────────────────────────────

fn planning(fixture_name: &str, text: &str, runs: f64, total_ms: f64, plan_ms: f64, plans: f64) -> Snapshot {
    let mut s = fixture(fixture_name);
    let q = only(&mut s, text);
    (q.executions, q.total_time_ms, q.mean_time_ms) = (runs, total_ms, total_ms / runs);
    let costs = q.costs.as_mut().unwrap();
    (costs.plan_ms, costs.plans) = (Some(plan_ms), Some(plans));
    s
}

#[test]
fn planning_on_postgres_for_every_run() {
    let s = planning(
        "postgres-demo",
        "SELECT * FROM orders o JOIN customers c ON c.id = o.customer_id WHERE o.id = $1",
        10_000.0,
        20_000.0,
        30_000.0,
        10_000.0,
    );
    says(
        &run("planning-time", &s),
        r#"id: planning-time:9065391607317686058
title: Planning takes 60% of the time of a read from orders
summary: Each plan takes 3 ms and the statement then runs for 2 ms: 60% of its time goes to planning, 30.0 s since its statistics began 5 hours ago (a new plan for every run). Planning gets expensive with many joins, partitions or large statistics targets, and it repeats when the plan isn't reused.
impact: 60% planning
metric: Planning share
metric: Per plan
metric: Plans
metric: Runs
fix: Prepare it once and reuse it: most drivers do when you bind parameters (pgjdbc prepareThreshold, Npgsql Max Auto Prepare, psycopg prepare=True). Behind PgBouncer in transaction mode, prepared statements need 1.21+ with max_prepared_statements set. If planning stays slow when reused, look at the number of joins and partitions it touches.
verify: Between two scans, its planning time per run should drop: pg_stat_statements keeps running totals, so compare its planning time and runs in a scan taken once the change is live and one a day or more later (30.0 s over 10K runs in this scan)."#,
    );
}

#[test]
fn compiling_on_sql_server_asked_for_by_a_recompile_hint() {
    let text = "SELECT id FROM dbo.orders WHERE customer_id = @P1 OPTION (RECOMPILE)";
    let s = planning("mssql-demo", text, 1_000.0, 10_000.0, 20_000.0, 1_000.0);
    says(
        &run("planning-time", &s),
        r#"id: planning-time:7
title: Compiling takes 67% of the time of a read from orders
summary: Each compile takes 20 ms and the statement then runs for 10 ms: 67% of its time goes to compiling, 20.0 s in the last 5 hours (a new plan for every run). Planning gets expensive with many joins, partitions or large statistics targets, and it repeats when the plan isn't reused. Its OPTION (RECOMPILE) hint explicitly asks SQL Server for a new plan on every run.
impact: 67% planning
metric: Planning share
metric: Per plan
metric: Plans
metric: Runs
fix: Test removing OPTION (RECOMPILE) from this statement so SQL Server can reuse a plan. Compare representative parameter values, including those that need different plans: keep the hint if the extra runtime from reuse costs more than the compiles it saves. Forced parameterization does not override this hint.
verify: Re-scan: planning should be a small part of its time.
verify: Compare compiles per run and planning time per run before and after, and check that time per run stays acceptable across representative parameter values. The rewritten statement has a new entry.
caveat: This Query Store entry's first compile falls within the selected period, so its compile totals can be compared with its runs in that period. Older entries' compile totals are shown separately as retained query history."#,
    );
}

#[test]
fn compiling_on_sql_server_for_every_run() {
    let s = planning("mssql-demo", "SELECT id FROM dbo.orders WHERE status = 3", 1_000.0, 10_000.0, 20_000.0, 1_000.0);
    says(
        &run("planning-time", &s),
        r#"id: planning-time:7
title: Compiling takes 67% of the time of a read from orders
summary: Each compile takes 20 ms and the statement then runs for 10 ms: 67% of its time goes to compiling, 20.0 s in the last 5 hours (a new plan for every run). Planning gets expensive with many joins, partitions or large statistics targets, and it repeats when the plan isn't reused.
impact: 67% planning
metric: Planning share
metric: Per plan
metric: Plans
metric: Runs
fix: It compiles on every run: look for OPTION (RECOMPILE), a procedure created WITH RECOMPILE, or literal values in the text instead of parameters (sp_executesql with parameters reuses one plan). Forced parameterization reuses plans for statements that differ only in literals, for the whole database: test it first.
sql: -- Database-wide: statements that differ only in literals share a plan
ALTER DATABASE CURRENT SET PARAMETERIZATION FORCED;
verify: Re-scan: planning should be a small part of its time.
caveat: This Query Store entry's first compile falls within the selected period, so its compile totals can be compared with its runs in that period. Older entries' compile totals are shown separately as retained query history."#,
    );
}

#[test]
fn compiling_on_sql_server_reused_but_slow_and_stopped() {
    let mut s =
        planning("mssql-demo", "SELECT id FROM dbo.orders WHERE status = @P1", 10_000.0, 10_000.0, 600_000.0, 100.0);
    stopped(&mut s);
    says(
        &run("planning-time", &s),
        r#"id: planning-time:7
title: Compiling took almost all of the time of a read from orders
summary: Each compile takes 6.0 s and the statement then runs for 1 ms: almost all of its time goes to compiling, 10 min in the last 3 days (a plan reused about 100 times). Planning gets expensive with many joins, partitions or large statistics targets, and it repeats when the plan isn't reused.
impact: 98% planning
metric: Planning share
metric: Per plan
metric: Plans
metric: Runs
fix: Plans are reused but each compile is slow: simplify the statement (fewer joins and views), or keep its plan with a Query Store hint or plan forcing so it isn't recompiled after statistics change.
verify: Re-scan: planning should be a small part of its time.
caveat: This Query Store entry's first compile falls within the selected period, so its compile totals can be compared with its runs in that period. Older entries' compile totals are shown separately as retained query history."#,
    );
}

// ── last-page-insert ───────────────────────────────────────────────────────────────────────────────

/// The SQL Server demo with only its first index contended, `uptime_hours` of counters behind it.
fn latched(uptime_hours: Option<f64>, c: culprit_core::IndexContention) -> Vec<Finding> {
    let mut s = fixture("mssql-demo");
    s.indexes.iter_mut().for_each(|i| i.contention = None);
    s.indexes[0].contention = Some(c);
    s.pressure.get_or_insert_with(Default::default).uptime_hours = uptime_hours;
    if uptime_hours.is_none() {
        s.server.started_at = None;
    }
    run("last-page-insert", &s)
}

fn latch(leaf_inserts: Option<f64>, optimize: Option<bool>) -> culprit_core::IndexContention {
    culprit_core::IndexContention {
        page_latch_waits: 5_000.0,
        page_latch_wait_ms: 600_000.0,
        leaf_inserts,
        sequential_key: true,
        optimize_for_sequential_key: optimize,
        ..Default::default()
    }
}

#[test]
fn last_page_inserts_on_sql_server_2019() {
    says(
        &latched(Some(240.0), latch(None, Some(false))),
        r#"id: last-page-insert:shop.customers.pk_customers
title: Inserts into customers queue for the last page of pk_customers
summary: pk_customers starts with a value that only grows (id), so every insert goes to the same last page. Concurrent inserts wait for each other's latch on it (PAGELATCH_EX): 5.0K waits, 10 min in all since the index was loaded. It gets worse with more concurrent writers.
impact: 10 min latch waits
metric: Page latch waits
metric: Time waiting
fix: SQL Server 2019 can pace concurrent inserts into a sequential key to help sustain throughput when many writers compete for the last page: try OPTIMIZE_FOR_SEQUENTIAL_KEY on this index and compare throughput. It's a metadata change and takes effect at once.
sql: ALTER INDEX [pk_customers] ON [shop].[customers] SET (OPTIMIZE_FOR_SEQUENTIAL_KEY = ON);
verify: Re-scan to confirm OPTIMIZE_FOR_SEQUENTIAL_KEY is ON. Compare completed inserts per second and the time to insert the same batch before and after, under comparable writer concurrency: the goal is better insert throughput under contention.
verify: Page-latch waits can continue, and BTREE_INSERT_FLOW_CONTROL waits may appear. Compare measurements over equal intervals; cumulative wait totals need not stop growing."#,
    );
}

#[test]
fn hot_pages_counted_for_half_an_hour() {
    says(
        &latched(Some(0.5), latch(Some(100.0), Some(false))),
        r#"id: last-page-insert:shop.customers.pk_customers
title: Sessions queue for the latch on a few hot pages of pk_customers
summary: pk_customers waited 5.0K times for page latches (10 min in all) since the index was loaded, but took only 100 inserts: the waits are sessions reading and changing the same few rows over and over (a counter, a queue table), not inserts on its last page.
impact: 10 min latch waits
metric: Page latch waits
metric: Time waiting
metric: Inserts
fix: Spread the hot rows (several counter rows summed when read, or a queue that hands out rows by key rather than from one end), and keep the transactions that change them short. OPTIMIZE_FOR_SEQUENTIAL_KEY paces inserts only, so it wouldn't help here.
verify: Compare completed row changes per second and their time under comparable load before and after. Compare the increase in page-latch wait time over equal intervals for that work; cumulative wait totals need not stop growing.
caveat: SQL Server has counted these latch waits only the 30 minutes since the server last restarted: too little time to make a rate, so the total doesn't raise it. A scan after an hour or more of counting can."#,
    );
}

#[test]
fn last_page_inserts_before_sql_server_2019_counted_since_an_unknown_start() {
    says(
        &latched(None, latch(Some(9_000.0), None)),
        r#"id: last-page-insert:shop.customers.pk_customers
title: Inserts into customers queue for the last page of pk_customers
summary: pk_customers starts with a value that only grows (id), so every insert goes to the same last page. Concurrent inserts wait for each other's latch on it (PAGELATCH_EX): 5.0K waits, 10 min in all since the index was loaded. It gets worse with more concurrent writers.
impact: 10 min latch waits
metric: Page latch waits
metric: Time waiting
metric: Inserts
fix: Before SQL Server 2019 the remedies change the key: lead the index with a value that spreads inserts (a hashed or partitioned key column), or batch inserts so fewer sessions compete.
verify: Compare completed inserts per second and their time under comparable load before and after. Compare the increase in page-latch wait time over equal intervals for that work; cumulative wait totals need not stop growing.
caveat: SQL Server counts latch waits from when it started, with no time for each, and this scan doesn't know when that was: they may come from one burst long ago. Culprit raises it only on a rate measured over an hour or more of counting."#,
    );
}

// ── untrusted-constraint ───────────────────────────────────────────────────────────────────────────

fn constraint(table: &str, name: &str, kind: culprit_core::ConstraintKind) -> culprit_core::ConstraintIssue {
    culprit_core::ConstraintIssue {
        schema: "shop".into(),
        table: table.into(),
        name: name.into(),
        kind,
        disabled: false,
        inheritance_child: false,
    }
}

#[test]
fn untrusted_constraints_on_sql_server() {
    use culprit_core::ConstraintKind::{Check, ForeignKey};
    let mut s = fixture("mssql-demo");
    s.constraints = vec![
        culprit_core::ConstraintIssue { disabled: true, ..constraint("orders", "fk_orders_customers", ForeignKey) },
        constraint("products", "ck_products_price", Check),
        constraint("customers", "ck_customers_email", Check),
        constraint("customers", "fk_customers_region", ForeignKey),
    ];
    says(
        &run("untrusted-constraint", &s),
        r#"id: untrusted-constraint:shop.customers
title: SQL Server doesn't trust 2 constraints on customers
summary: ck_customers_email and fk_customers_region were created or re-enabled WITH NOCHECK, so existing rows were never checked against them. The optimizer only relies on trusted constraints: it can't remove a join to the referenced table when no columns from it are used, or rule out values a check constraint excludes, so it plans work it could skip.
impact: 2 untrusted
metric: Constraints
fix: Check the existing rows so SQL Server trusts them again. It reads the whole table and locks it while it runs: schedule it off-peak. It fails if a row breaks the constraint; fix those rows first.
cost: Reads the whole table
sql: ALTER TABLE [shop].[customers] WITH CHECK CHECK CONSTRAINT [ck_customers_email];
ALTER TABLE [shop].[customers] WITH CHECK CHECK CONSTRAINT [fk_customers_region];
verify: Re-scan: the constraint should no longer be listed.

id: untrusted-constraint:shop.orders
title: The foreign key fk_orders_customers on orders is switched off
summary: fk_orders_customers was created or re-enabled WITH NOCHECK, so existing rows were never checked against it. The optimizer only relies on trusted constraints: it can't remove a join to the referenced table when no columns from it are used, or rule out values a check constraint excludes, so it plans work it could skip. A switched-off constraint doesn't check new rows either, so the data may already break it.
impact: 1 untrusted
metric: Constraints
fix: Check the existing rows so SQL Server trusts it again. It reads the whole table and locks it while it runs: schedule it off-peak. It fails if a row breaks the constraint; fix those rows first.
cost: Reads the whole table
sql: ALTER TABLE [shop].[orders] WITH CHECK CHECK CONSTRAINT [fk_orders_customers];
verify: Re-scan: the constraint should no longer be listed.

id: untrusted-constraint:shop.products
title: SQL Server doesn't trust the check constraint ck_products_price on products
summary: ck_products_price was created or re-enabled WITH NOCHECK, so existing rows were never checked against it. The optimizer only relies on trusted constraints: it can't remove a join to the referenced table when no columns from it are used, or rule out values a check constraint excludes, so it plans work it could skip.
impact: 1 untrusted
metric: Constraints
fix: Check the existing rows so SQL Server trusts it again. It reads the whole table and locks it while it runs: schedule it off-peak. It fails if a row breaks the constraint; fix those rows first.
cost: Reads the whole table
sql: ALTER TABLE [shop].[products] WITH CHECK CHECK CONSTRAINT [ck_products_price];
verify: Re-scan: the constraint should no longer be listed."#,
    );
}

#[test]
fn not_valid_checks_on_postgres() {
    use culprit_core::ConstraintKind::Check;
    let mut s = fixture("postgres-demo");
    s.constraints = vec![
        culprit_core::ConstraintIssue { inheritance_child: true, ..constraint("orders", "ck_orders_total", Check) },
        constraint("products", "ck_products_price", Check),
        constraint("customers", "ck_customers_email", Check),
        constraint("customers", "ck_customers_name", Check),
    ];
    let mut findings = run("untrusted-constraint", &s);
    set(&mut s, "constraint_exclusion", "on");
    s.constraints.retain(|c| c.table == "customers");
    findings.extend(run("untrusted-constraint", &s));
    says(
        &findings,
        r#"id: untrusted-constraint:shop.customers
title: 2 check constraints on customers were never validated
summary: ck_customers_email and ck_customers_name were added NOT VALID: PostgreSQL checks new and changed rows against them but never checked the rows that existed then, so some may break them. That is often deliberate: NOT VALID adds a constraint without a long scan under lock. The planner doesn't read it here either way (constraint exclusion looks at CHECK constraints only on inheritance children unless constraint_exclusion is on). Validating proves the old rows, and lets later steps skip their own full scan: SET NOT NULL (12+) and ATTACH PARTITION when the constraint implies them.
impact: 2 untrusted
metric: Constraints
fix: Validate it: PostgreSQL scans the table but takes only a SHARE UPDATE EXCLUSIVE lock, so reads and writes continue. It fails if a row breaks the constraint; fix those rows first.
cost: Reads the whole table
sql: ALTER TABLE shop.customers VALIDATE CONSTRAINT ck_customers_email;
ALTER TABLE shop.customers VALIDATE CONSTRAINT ck_customers_name;
verify: Re-scan: the constraint should no longer be listed.

id: untrusted-constraint:shop.orders
title: The check constraint ck_orders_total on orders was never validated
summary: ck_orders_total was added NOT VALID, so rows that existed then were never checked. Until it is validated the planner can't use it to skip the table when a query's filter contradicts it (constraint exclusion, which it applies to this inheritance child).
impact: 1 untrusted
metric: Constraints
fix: Validate it: PostgreSQL scans the table but takes only a SHARE UPDATE EXCLUSIVE lock, so reads and writes continue. It fails if a row breaks the constraint; fix those rows first.
cost: Reads the whole table
sql: ALTER TABLE shop.orders VALIDATE CONSTRAINT ck_orders_total;
verify: Re-scan: the constraint should no longer be listed.

id: untrusted-constraint:shop.products
title: The check constraint ck_products_price on products was never validated
summary: ck_products_price was added NOT VALID: PostgreSQL checks new and changed rows against it but never checked the rows that existed then, so some may break it. That is often deliberate: NOT VALID adds a constraint without a long scan under lock. The planner doesn't read it here either way (constraint exclusion looks at CHECK constraints only on inheritance children unless constraint_exclusion is on). Validating proves the old rows, and lets later steps skip their own full scan: SET NOT NULL (12+) and ATTACH PARTITION when the constraint implies them.
impact: 1 untrusted
metric: Constraints
fix: Validate it: PostgreSQL scans the table but takes only a SHARE UPDATE EXCLUSIVE lock, so reads and writes continue. It fails if a row breaks the constraint; fix those rows first.
cost: Reads the whole table
sql: ALTER TABLE shop.products VALIDATE CONSTRAINT ck_products_price;
verify: Re-scan: the constraint should no longer be listed.

id: untrusted-constraint:shop.customers
title: 2 check constraints on customers were never validated
summary: ck_customers_email and ck_customers_name were added NOT VALID, so rows that existed then were never checked. Until it is validated the planner can't use it to skip the table when a query's filter contradicts it (constraint exclusion, which constraint_exclusion = on applies to every table).
impact: 2 untrusted
metric: Constraints
fix: Validate it: PostgreSQL scans the table but takes only a SHARE UPDATE EXCLUSIVE lock, so reads and writes continue. It fails if a row breaks the constraint; fix those rows first.
cost: Reads the whole table
sql: ALTER TABLE shop.customers VALIDATE CONSTRAINT ck_customers_email;
ALTER TABLE shop.customers VALIDATE CONSTRAINT ck_customers_name;
verify: Re-scan: the constraint should no longer be listed."#,
    );
}

#[test]
fn constraints_awaiting_validation_on_yugabyte() {
    use culprit_core::ConstraintKind::{Check, ForeignKey};
    let mut s = fixture("yugabyte-demo");
    s.constraints = vec![
        constraint("orders", "fk_orders_customers", ForeignKey),
        constraint("customers", "ck_customers_email", Check),
        constraint("customers", "fk_customers_region", ForeignKey),
    ];
    says(
        &run("untrusted-constraint", &s),
        r#"id: untrusted-constraint:shop.customers
title: 2 constraints on customers were never validated
summary: ck_customers_email and fk_customers_region were added NOT VALID. YugabyteDB checks new and changed rows but has not checked the rows that existed when these constraints were added. Some old rows may violate them. Deferred validation can be deliberate during a migration; this catalog state does not establish a query-plan regression.
impact: 2 not validated
metric: Constraints
fix: Validate existing rows in a planned maintenance window after checking this YugabyteDB version's validation support. Validation can issue distributed scans and acquire locks. It fails if a row breaks a constraint; fix those rows first. Do not assume PostgreSQL lock or concurrent-write guarantees.
cost: Distributed table scans and validation locks
sql: ALTER TABLE shop.customers VALIDATE CONSTRAINT ck_customers_email;
ALTER TABLE shop.customers VALIDATE CONSTRAINT fk_customers_region;
verify: Re-scan: the constraint should no longer be listed.

id: untrusted-constraint:shop.orders
title: The foreign key fk_orders_customers on orders was never validated
summary: fk_orders_customers was added NOT VALID. YugabyteDB checks new and changed rows but has not checked the rows that existed when these constraints were added. Some old rows may violate them. Deferred validation can be deliberate during a migration; this catalog state does not establish a query-plan regression.
impact: 1 not validated
metric: Constraints
fix: Validate existing rows in a planned maintenance window after checking this YugabyteDB version's validation support. Validation can issue distributed scans and acquire locks. It fails if a row breaks a constraint; fix those rows first. Do not assume PostgreSQL lock or concurrent-write guarantees.
cost: Distributed table scans and validation locks
sql: ALTER TABLE shop.orders VALIDATE CONSTRAINT fk_orders_customers;
verify: Re-scan: the constraint should no longer be listed."#,
    );
}

// ── hot-updates ────────────────────────────────────────────────────────────────────────────────────

#[test]
fn updates_that_are_not_hot() {
    let mut s = fixture("postgres-demo");
    let mut fk = s.foreign_keys.iter().find(|f| f.ref_table == "products").expect("a key").clone();
    fk.name = "fk_events_products".into();
    fk.table = "events".into();
    s.foreign_keys.push(fk);
    let plant = |s: &mut Snapshot, table: &str, updates: f64, storage: culprit_core::TableStorage| {
        let t = s.tables.iter_mut().find(|t| t.name == table).expect("a table");
        t.updates = Some(updates);
        t.storage = Some(storage);
    };
    use culprit_core::TableStorage;
    plant(&mut s, "events", 10_000.0, TableStorage { hot_updates: Some(0.0), ..Default::default() });
    plant(
        &mut s,
        "order_items",
        10_000.0,
        TableStorage {
            hot_updates: Some(3_000.0),
            new_page_updates: Some(6_000.0),
            counter_hours: Some(48.0),
            ..Default::default()
        },
    );
    plant(
        &mut s,
        "orders",
        10_000_000.0,
        TableStorage {
            hot_updates: Some(2_000_000.0),
            new_page_updates: Some(1_000_000.0),
            fillfactor: Some(90.0),
            counter_hours: Some(24.0),
            ..Default::default()
        },
    );
    plant(
        &mut s,
        "products",
        20_000.0,
        TableStorage {
            hot_updates: Some(5_000.0),
            new_page_updates: Some(7_500.0),
            counter_hours: Some(2.0),
            ..Default::default()
        },
    );
    says(
        &run("hot-updates", &s),
        r#"id: hot-updates:public.orders
title: Only 20% of updates to orders are HOT: the rest rewrite every index
summary: An update that changes no indexed column and finds room on the same page is HOT: it writes no index entries and its old version is cleaned up without VACUUM. 8.0M of orders's 10.0M updates weren't, so each wrote a new entry in all 3 of its indexes. Only 12% of them lacked room on the page; the rest changed an indexed column, which rules HOT out. 1 foreign key (on public.order_items) references orders: in PostgreSQL, non-HOT updates on a referenced parent table write new index tuples and contend with FOR KEY SHARE tuple locks taken by concurrent inserts and updates on the child table.
impact: 20% HOT
metric: Updates
metric: HOT
metric: Indexes
metric: Fillfactor
metric: Referencing foreign keys
fix: The updates change an indexed column. Find the index on a column that changes often (updated_at, status, counters) and drop it if reads don't need it; every update that touches it can then be HOT.
verify: Re-scan a few days later: the HOT share counts since the last statistics reset, so compare how n_tup_hot_upd grows against n_tup_upd.

id: hot-updates:public.products
title: Only 25% of updates to products are HOT: the rest rewrite every index
summary: An update that changes no indexed column and finds room on the same page is HOT: it writes no index entries and its old version is cleaned up without VACUUM. 15K of products's 20K updates weren't, so each wrote a new entry in all 3 of its indexes. 50% of them moved to another page because their own was full (fillfactor 100). 2 foreign keys (on public.events and public.order_items) reference products: in PostgreSQL, non-HOT updates on a referenced parent table write new index tuples and contend with FOR KEY SHARE tuple locks taken by concurrent inserts and updates on the child tables.
impact: 25% HOT
metric: Updates
metric: HOT
metric: Indexes
metric: Fillfactor
metric: Referencing foreign keys
fix: Leave room on each page so updated rows fit next to the old version: a fillfactor of 85–90 is typical for tables updated often. It applies to pages written from now on; existing pages get the room as rows move (pg_repack rewrites the table online if you want it at once). If an index covers a column that changes on every update (updated_at, status, counters), dropping it makes those updates HOT too.
sql: ALTER TABLE public.products SET (fillfactor = 85);
verify: Re-scan a few days later: the HOT share counts since the last statistics reset, so compare how n_tup_hot_upd grows against n_tup_upd.

id: hot-updates:public.order_items
title: Only 30% of updates to order_items are HOT: the rest rewrite every index
summary: An update that changes no indexed column and finds room on the same page is HOT: it writes no index entries and its old version is cleaned up without VACUUM. 7.0K of order_items's 10K updates weren't, so each wrote a new entry in its only index. 86% of them moved to another page because their own was full (fillfactor 100).
impact: 30% HOT
metric: Updates
metric: HOT
metric: Indexes
metric: Fillfactor
fix: Leave room on each page so updated rows fit next to the old version: a fillfactor of 85–90 is typical for tables updated often. It applies to pages written from now on; existing pages get the room as rows move (pg_repack rewrites the table online if you want it at once). If an index covers a column that changes on every update (updated_at, status, counters), dropping it makes those updates HOT too.
sql: ALTER TABLE public.order_items SET (fillfactor = 85);
verify: Re-scan a few days later: the HOT share counts since the last statistics reset, so compare how n_tup_hot_upd grows against n_tup_upd.
caveat: The counts cover the 2 days since the statistics were last reset: one large update or backfill in that time keeps the share low long after it ran. Compare how n_tup_hot_upd grows against n_tup_upd over a few days before changing the table.

id: hot-updates:public.events
title: No update to events is HOT: each one rewrites every index
summary: An update that changes no indexed column and finds room on the same page is HOT: it writes no index entries and its old version is cleaned up without VACUUM. None of events's 10K updates were, so each wrote a new entry in both of its indexes. Either the pages are full (fillfactor 100) or the updates change an indexed column.
impact: 0% HOT
metric: Updates
metric: HOT
metric: Indexes
metric: Fillfactor
fix: Leave room on each page so updated rows fit next to the old version: a fillfactor of 85–90 is typical for tables updated often. It applies to pages written from now on; existing pages get the room as rows move (pg_repack rewrites the table online if you want it at once). If an index covers a column that changes on every update (updated_at, status, counters), dropping it makes those updates HOT too.
sql: ALTER TABLE public.events SET (fillfactor = 85);
verify: Re-scan a few days later: the HOT share counts since the last statistics reset, so compare how n_tup_hot_upd grows against n_tup_upd.
caveat: How long these counts have run isn't known (PostgreSQL 15+ records no start until its statistics are first reset, and the counts outlive restarts): one large update or backfill long ago would keep the share low. Compare how n_tup_hot_upd grows against n_tup_upd over a few days before changing the table."#,
    );
}

// ── connection-churn ───────────────────────────────────────────────────────────────────────────────

fn churn(name: &str, signals: &[(&str, f64)], settings: &[(&str, &str)]) -> Vec<Finding> {
    use culprit_core::{Signal, SignalScope, SignalUnit};
    let mut s = fixture(name);
    let p = s.pressure.get_or_insert_with(Default::default);
    p.uptime_hours = Some(72.0);
    p.signals = signals
        .iter()
        .map(|(key, value)| Signal::new(key, key, *value, SignalUnit::PerSecond, SignalScope::SinceStartup))
        .collect();
    for (name, value) in settings {
        set(&mut s, name, value);
    }
    run("connection-churn", &s)
}

#[test]
fn connection_churn_on_postgres() {
    let mut findings = churn("postgres-demo", &[("new-connections", 12.0), ("session-length", 0.5)], &[]);
    findings.extend(churn("postgres-demo", &[("new-connections", 3.0)], &[]));
    says(
        &findings,
        r#"id: connection-churn:server
title: This database recorded 12 new connections a second on average
summary: Since PostgreSQL started or its statistics were reset, 3.0 days ago the server accepted 12 new connections a second on average to this database. Each new PostgreSQL connection starts a server process and pays for TLS and authentication: milliseconds of CPU and a few MB of memory before the first query. Sessions here last 500 ms on average. If the application connects for each request, pooling can avoid that setup cost; this rate alone does not prove it does so or that connection setup is slowing requests.
impact: 12/s new connections
metric: New connections per second
fix: Pool connections in the application (every mainstream driver has a pool), or put PgBouncer in front in transaction mode so thousands of clients share a few dozen server connections.
verify: Compare new-connection counts over equal intervals before and after the change, together with request latency. Pooling should reduce connection setup for the same workload; there is no universal healthy connection rate.
caveat: A rate since startup: one busy reconnect storm counts for the whole period.

id: connection-churn:server
title: This database recorded 3 new connections a second on average
summary: Since PostgreSQL started or its statistics were reset, 3.0 days ago the server accepted 3 new connections a second on average to this database. Each new PostgreSQL connection starts a server process and pays for TLS and authentication: milliseconds of CPU and a few MB of memory before the first query. If the application connects for each request, pooling can avoid that setup cost; this rate alone does not prove it does so or that connection setup is slowing requests.
impact: 3/s new connections
metric: New connections per second
fix: Pool connections in the application (every mainstream driver has a pool), or put PgBouncer in front in transaction mode so thousands of clients share a few dozen server connections.
verify: Compare new-connection counts over equal intervals before and after the change, together with request latency. Pooling should reduce connection setup for the same workload; there is no universal healthy connection rate.
caveat: A rate since startup: one busy reconnect storm counts for the whole period."#,
    );
}

#[test]
fn connection_churn_on_sql_server() {
    let mut findings = churn("mssql-demo", &[("new-connections", 20.0), ("pooled-reuse", 40.0)], &[]);
    findings.extend(churn("mssql-demo", &[("new-connections", 20.0)], &[]));
    says(
        &findings,
        r#"id: connection-churn:server
title: The whole server recorded 20 new connections a second on average
summary: Since SQL Server started or its statistics were reset, 3.0 days ago the server accepted 20 new connections a second on average, counting every database on the instance. SQL Server recorded 40% resets compared with resets plus new logins; pools can also reuse connections without a reset, so this does not identify all pool use. If the application connects for each request, pooling can avoid that setup cost; this rate alone does not prove it does so or that connection setup is slowing requests.
impact: 20/s new connections
metric: New connections per second
fix: Keep connection pooling on (it is by default in ADO.NET and Microsoft.Data.SqlClient; JDBC needs a pool such as HikariCP) and close or dispose connections promptly so they go back to the pool. Every distinct connection string gets its own pool.
verify: Compare new-connection counts over equal intervals before and after the change, together with request latency. Pooling should reduce connection setup for the same workload; there is no universal healthy connection rate.
caveat: SQL Server counts connections for the whole server, so other databases' applications are in this number.
caveat: A rate since startup: one busy reconnect storm counts for the whole period.

id: connection-churn:server
title: The whole server recorded 20 new connections a second on average
summary: Since SQL Server started or its statistics were reset, 3.0 days ago the server accepted 20 new connections a second on average, counting every database on the instance. Every login pays for TLS, authentication and session setup. If the application connects for each request, pooling can avoid that setup cost; this rate alone does not prove it does so or that connection setup is slowing requests.
impact: 20/s new connections
metric: New connections per second
fix: Keep connection pooling on (it is by default in ADO.NET and Microsoft.Data.SqlClient; JDBC needs a pool such as HikariCP) and close or dispose connections promptly so they go back to the pool. Every distinct connection string gets its own pool.
verify: Compare new-connection counts over equal intervals before and after the change, together with request latency. Pooling should reduce connection setup for the same workload; there is no universal healthy connection rate.
caveat: SQL Server counts connections for the whole server, so other databases' applications are in this number.
caveat: A rate since startup: one busy reconnect storm counts for the whole period."#,
    );
}

#[test]
fn connection_churn_on_mysql() {
    let mut findings = churn("mysql-demo", &[("new-connections", 15.0), ("thread-cache-misses", 30.0)], &[]);
    findings.extend(churn("mysql-demo", &[("new-connections", 15.0)], &[("thread_cache_size", "100")]));
    says(
        &findings,
        r#"id: connection-churn:server
title: The whole server recorded 15 new connections a second on average
summary: Since MySQL started or its statistics were reset, 3.0 days ago the server accepted 15 new connections a second on average, counting every schema and application on it (failed attempts left out). 30% of them needed a new thread because the thread cache was empty. If the application connects for each request, pooling can avoid that setup cost; this rate alone does not prove it does so or that connection setup is slowing requests.
impact: 15/s new connections
metric: New connections per second
fix: Pool connections in the application. Until then, a larger thread cache lets new connections reuse threads: Set thread_cache_size to 64; SET PERSIST keeps it across restarts.
sql: SET PERSIST thread_cache_size = 64;
verify: Compare new-connection counts over equal intervals before and after the change, together with request latency. Pooling should reduce connection setup for the same workload; there is no universal healthy connection rate.
caveat: MySQL counts connections for the whole server, so other databases' applications are in this number.
caveat: A rate since startup: one busy reconnect storm counts for the whole period.

id: connection-churn:server
title: The whole server recorded 15 new connections a second on average
summary: Since MySQL started or its statistics were reset, 3.0 days ago the server accepted 15 new connections a second on average, counting every schema and application on it (failed attempts left out). Every connection pays for TLS, authentication and a thread. If the application connects for each request, pooling can avoid that setup cost; this rate alone does not prove it does so or that connection setup is slowing requests.
impact: 15/s new connections
metric: New connections per second
fix: Pool connections in the application: the thread cache already keeps 100 threads, so each connection still pays for TLS and authentication.
verify: Compare new-connection counts over equal intervals before and after the change, together with request latency. Pooling should reduce connection setup for the same workload; there is no universal healthy connection rate.
caveat: MySQL counts connections for the whole server, so other databases' applications are in this number.
caveat: A rate since startup: one busy reconnect storm counts for the whole period."#,
    );
}

#[test]
fn connection_churn_on_yugabyte_and_sqlite() {
    let mut findings = churn("yugabyte-demo", &[("new-connections", 12.0)], &[]);
    findings.extend(churn("sqlite-demo", &[("new-connections", 12.0)], &[]));
    says(
        &findings,
        r#"id: connection-churn:server
title: The whole server recorded 12 new connections a second on average
summary: Since YugabyteDB started or its statistics were reset, 3.0 days ago the server accepted 12 new connections a second on average, within the measured YSQL node scope. A YSQL logical client can share a physical backend through Connection Manager; these counts alone do not identify how clients are pooled. If the application connects for each request, pooling can avoid that setup cost; this rate alone does not prove it does so or that connection setup is slowing requests.
impact: 12/s new connections
metric: New connections per second
fix: Reuse connections through the application pool or YSQL Connection Manager; compare connection setup latency and client/backend counts before changing pool sizes.
verify: Compare new-connection counts over equal intervals before and after the change, together with request latency. Pooling should reduce connection setup for the same workload; there is no universal healthy connection rate.
caveat: YugabyteDB counts connections for the whole server, so other databases' applications are in this number.
caveat: A rate since startup: one busy reconnect storm counts for the whole period.

id: connection-churn:server
title: The whole server recorded 12 new connections a second on average
summary: Since SQLite started or its statistics were reset, 3.0 days ago the server accepted 12 new connections a second on average. SQLite has no connections to pool: each one opens the file. If the application connects for each request, pooling can avoid that setup cost; this rate alone does not prove it does so or that connection setup is slowing requests.
impact: 12/s new connections
metric: New connections per second
fix: Keep one connection open per worker instead of opening the file for each request.
verify: Compare new-connection counts over equal intervals before and after the change, together with request latency. Pooling should reduce connection setup for the same workload; there is no universal healthy connection rate.
caveat: SQLite counts connections for the whole server, so other databases' applications are in this number.
caveat: A rate since startup: one busy reconnect storm counts for the whole period."#,
    );
}
