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

// ── big-result ─────────────────────────────────────────────────────────────────────────────────────

fn returning(name: &str, text: &str, runs: f64, rows_a_run: f64, row_bytes: Option<f64>) -> Snapshot {
    let mut s = fixture(name);
    let q = only(&mut s, text);
    q.executions = runs;
    q.rows = Some(runs * rows_a_run);
    q.plan = None;
    q.costs.as_mut().unwrap().row_bytes = row_bytes;
    s
}

#[test]
fn big_results_with_a_width_over_running_totals() {
    let s =
        returning("postgres-demo", "SELECT id, note FROM orders WHERE created_at > $1", 200.0, 80_000.0, Some(150.0));
    says(
        &run("big-result", &s),
        r#"id: big-result:9065391607317686058
title: A read from orders returns 80K rows (≈ 11.4 MB) per run
summary: Each run sends 80K rows to the application, about 11.4 MB at the plan's estimated 150 B per row; its 200 runs since its statistics began 5 hours ago moved 16.0M rows. Few screens or jobs need that many at once: the database reads them, the network carries them and the application holds them in memory.
impact: 80K rows per run
metric: Rows per run
metric: Runs
metric: Bytes per run (estimated)
fix: Return only what's used: page the results (keyset pagination, or LIMIT / OFFSET … FETCH), filter in SQL rather than in the application, select only the columns you need, and aggregate in the database.
verify: Between two scans, the rows it returns per run should drop to what a page or a job actually uses: pg_stat_statements keeps running totals, so compare its rows and runs in a scan taken once the change is live and one a day or more later (16.0M rows in 200 runs in this scan)."#,
    );
}

#[test]
fn big_results_without_a_width_in_a_window_and_stopped() {
    let s = returning("mssql-demo", "SELECT id FROM dbo.orders WHERE status = 3", 50.0, 150_000.0, None);
    let mut findings = run("big-result", &s);
    let mut s = returning("mssql-demo", "SELECT id FROM dbo.orders WHERE status = 4", 50.0, 20_000.0, None);
    stopped(&mut s);
    findings.extend(run("big-result", &s));
    says(
        &findings,
        r#"id: big-result:7
title: A read from orders returns 150K rows per run
summary: Each run sends 150K rows to the application; its 50 runs in the last 5 hours moved 7.5M rows. Few screens or jobs need that many at once: the database reads them, the network carries them and the application holds them in memory.
impact: 150K rows per run
metric: Rows per run
metric: Runs
fix: Return only what's used: page the results (keyset pagination, or LIMIT / OFFSET … FETCH), filter in SQL rather than in the application, select only the columns you need, and aggregate in the database.
verify: Re-scan: rows per run should drop to what a page or a job actually uses.

id: big-result:7
title: A read from orders returned 20K rows per run
summary: Each run sends 20K rows to the application; its 50 runs in the last 3 days moved 1.0M rows. Few screens or jobs need that many at once: the database reads them, the network carries them and the application holds them in memory.
impact: 20K rows per run
metric: Rows per run
metric: Runs
fix: Return only what's used: page the results (keyset pagination, or LIMIT / OFFSET … FETCH), filter in SQL rather than in the application, select only the columns you need, and aggregate in the database.
verify: Re-scan: rows per run should drop to what a page or a job actually uses."#,
    );
}

#[test]
fn big_results_of_an_export() {
    let s = returning("mysql-demo", "SELECT SQL_NO_CACHE * FROM `orders`", 30.0, 20_000.0, None);
    says(
        &run("big-result", &s),
        r#"id: big-result:04cd7be9c366fadc
title: A read from orders returns 20K rows per run
summary: Each run sends 20K rows to the application; its 30 runs since its statistics began 2 minutes ago moved 600K rows. It reads the whole table 30 times in 2 minutes: an export, a dump or an ETL job looks like this, and for those the rows are the point. It needs work only if a screen or a request runs it.
impact: 20K rows per run
metric: Rows per run
metric: Runs
fix: If it is an export or ETL job, leave it as it is, and run it on a replica or off-peak if it competes with the application. If the application runs it: Return only what's used: page the results (keyset pagination, or LIMIT / OFFSET … FETCH), filter in SQL rather than in the application, select only the columns you need, and aggregate in the database.
verify: Between two scans, the rows it returns per run should drop to what a page or a job actually uses: performance_schema keeps running totals, so compare its rows and runs in a scan taken once the change is live and one a day or more later (600K rows in 30 runs in this scan)."#,
    );
}

// ── lock-time ──────────────────────────────────────────────────────────────────────────────────────

fn locked(name: &str, text: &str, runs: f64, total_ms: f64, lock_ms: f64, max_ms: Option<f64>) -> Snapshot {
    let mut s = fixture(name);
    let q = only(&mut s, text);
    (q.executions, q.total_time_ms, q.mean_time_ms, q.max_time_ms) = (runs, total_ms, total_ms / runs, max_ms);
    q.waits = vec![culprit_core::WaitStat {
        category: "Lock".into(),
        wait_type: Some("lock time".into()),
        wait_ms: lock_ms,
        tasks: None,
    }];
    s
}

#[test]
fn lock_time_on_mysql() {
    let s = locked(
        "mysql-demo",
        "UPDATE `stock` SET `qty` = `qty` - ? WHERE `sku` = ?",
        1_000.0,
        100_000.0,
        60_000.0,
        None,
    );
    says(
        &run("lock-time", &s),
        r#"id: lock-time:04cd7be9c366fadc
title: An update of stock waits on locks for 60% of its time, 60 ms per run
summary: Of its 2 min since its statistics began 2 minutes ago, 1 min went to waiting for locks other sessions held. That time belongs to the sessions holding the locks: making this statement faster won't help, shortening the transactions that hold the rows will. MySQL counts row-lock waits in a statement's lock time from 8.0.28; earlier versions count table locks only.
impact: 1 min waiting on locks
metric: Lock wait
metric: Share of its time
metric: Per run
fix: Find what holds the locks while it waits (Live shows the blocking chain). Keep the holding transactions short and commit before slow work; make UPDATE and DELETE filter on an index so they lock only the rows they change; touch rows in the same order everywhere.
verify: Between two scans, its lock time per run should drop: performance_schema keeps running totals, so compare its lock time and runs in a scan taken once the change is live and one a day or more later (1 min over 1.0K runs in this scan).
caveat: The statistics don't cover enough time (an hour or more from a known start) to say how many minutes of waiting a day this is, so it isn't raised on its share alone."#,
    );
}

#[test]
fn lock_time_of_reads_that_row_versioning_would_spare() {
    let read = |runs: f64, total: f64, lock: f64, max: Option<f64>| {
        let mut s = locked("mssql-demo", "SELECT id FROM dbo.orders WHERE customer_id = @P1", runs, total, lock, max);
        set(&mut s, "is_read_committed_snapshot_on", "0");
        s
    };
    let mut findings = run("lock-time", &read(1.0, 10_500.0, 10_400.0, None));
    findings.extend(run("lock-time", &read(2.0, 21_000.0, 15_000.0, Some(20_000.0))));
    findings.extend(run("lock-time", &read(2_000.0, 100_000.0, 60_000.0, None)));
    says(
        &findings,
        r#"id: lock-time:7
title: A read from orders spends nearly all its time waiting on locks: its one run waited 10.4 s
summary: Of its 10.5 s in the last 5 hours, 10.4 s went to waiting for locks other sessions held. That time belongs to the sessions holding the locks: making this statement faster won't help, shortening the transactions that hold the rows will.
impact: 10.4 s waiting on locks
metric: Lock wait
metric: Share of its time
metric: Per run
fix: This is a read, and READ_COMMITTED_SNAPSHOT is off for this database, so under read committed it waits for any write holding a lock on the rows it reads. With row versioning on (config:read-committed-snapshot has the change), it would read the last committed version of the rows instead of waiting. Without it, it waits for the writers. It ran once in the last 5 hours, so there is no wait to catch now; if it runs again, have its lock waits logged (SQL Server's blocked process report names the holder) to see what holds them. Keep the holding transactions short and commit before slow work; make UPDATE and DELETE filter on an index so they lock only the rows they change; touch rows in the same order everywhere.
verify: Re-scan: lock waits should be a small part of its time.

id: lock-time:7
title: A read from orders waits on locks for 71% of its time: one run waited at least 14.0 s
summary: Of its 21.0 s in the last 5 hours, 15.0 s went to waiting for locks other sessions held. That time belongs to the sessions holding the locks: making this statement faster won't help, shortening the transactions that hold the rows will.
impact: 15.0 s waiting on locks
metric: Lock wait
metric: Share of its time
metric: Longest run
fix: This is a read, and READ_COMMITTED_SNAPSHOT is off for this database, so under read committed it waits for any write holding a lock on the rows it reads. With row versioning on (config:read-committed-snapshot has the change), it would read the last committed version of the rows instead of waiting. Without it, it waits for the writers. It ran 2 times in 5 hours, too seldom to count on catching it while it waits. Have the next ones reported instead: SQL Server's blocked process report names the session holding the lock, with its statement, for each wait longer than the server's blocked process threshold (s) (0, off, by default; sp_configure sets it). Capture sqlserver.blocked_process_report in an event session to keep them. Keep the holding transactions short and commit before slow work; make UPDATE and DELETE filter on an index so they lock only the rows they change; touch rows in the same order everywhere.
verify: Re-scan: lock waits should be a small part of its time.

id: lock-time:7
title: A read from orders waits on locks for 60% of its time, 30 ms per run
summary: Of its 2 min in the last 5 hours, 1 min went to waiting for locks other sessions held. That time belongs to the sessions holding the locks: making this statement faster won't help, shortening the transactions that hold the rows will.
impact: 1 min waiting on locks
metric: Lock wait
metric: Share of its time
metric: Per run
fix: This is a read, and READ_COMMITTED_SNAPSHOT is off for this database, so under read committed it waits for any write holding a lock on the rows it reads. With row versioning on (config:read-committed-snapshot has the change), it would read the last committed version of the rows instead of waiting. Without it, find what holds the locks while it waits (Live shows the blocking chain). Keep the holding transactions short and commit before slow work; make UPDATE and DELETE filter on an index so they lock only the rows they change; touch rows in the same order everywhere.
verify: Re-scan: lock waits should be a small part of its time."#,
    );
}

#[test]
fn lock_time_of_a_write_that_stopped() {
    let mut s =
        locked("mssql-demo", "UPDATE dbo.orders SET status = 2 WHERE id = @P1", 2_000.0, 100_000.0, 97_000.0, None);
    stopped(&mut s);
    says(
        &run("lock-time", &s),
        r#"id: lock-time:7
title: An update of orders spent nearly all its time waiting on locks, 48 ms per run
summary: Of its 2 min in the last 3 days, 2 min went to waiting for locks other sessions held. That time belongs to the sessions holding the locks: making this statement faster won't help, shortening the transactions that hold the rows will.
impact: 2 min waiting on locks
metric: Lock wait
metric: Share of its time
metric: Per run
fix: It no longer runs, so there is no wait to catch now; if it comes back, find what holds the locks while it waits (Live shows the blocking chain). Keep the holding transactions short and commit before slow work; make UPDATE and DELETE filter on an index so they lock only the rows they change; touch rows in the same order everywhere.
verify: Re-scan: lock waits should be a small part of its time."#,
    );
}

#[test]
fn lock_time_of_a_write_that_waited_for_part_of_its_time_and_stopped() {
    let mut s =
        locked("mssql-demo", "UPDATE dbo.orders SET status = 2 WHERE id = @P1", 2_000.0, 100_000.0, 60_000.0, None);
    stopped(&mut s);
    says(
        &run("lock-time", &s),
        r#"id: lock-time:7
title: An update of orders waited on locks for 60% of its time, 30 ms per run
summary: Of its 2 min in the last 3 days, 1 min went to waiting for locks other sessions held. That time belongs to the sessions holding the locks: making this statement faster won't help, shortening the transactions that hold the rows will.
impact: 1 min waiting on locks
metric: Lock wait
metric: Share of its time
metric: Per run
fix: It no longer runs, so there is no wait to catch now; if it comes back, find what holds the locks while it waits (Live shows the blocking chain). Keep the holding transactions short and commit before slow work; make UPDATE and DELETE filter on an index so they lock only the rows they change; touch rows in the same order everywhere.
verify: Re-scan: lock waits should be a small part of its time."#,
    );
}

// ── failing-query ──────────────────────────────────────────────────────────────────────────────────

fn failing(name: &str, text: &str, runs: f64, costs: culprit_core::QueryCosts) -> Snapshot {
    let mut s = fixture(name);
    let q = only(&mut s, text);
    q.executions = runs;
    q.query_hash = Some("abc123".into());
    q.costs = Some(costs);
    s
}

#[test]
fn failing_runs_on_sql_server() {
    use culprit_core::QueryCosts;
    let read = "SELECT id FROM dbo.orders WHERE customer_id = @P1";
    let mut findings = run(
        "failing-query",
        &failing(
            "mssql-demo",
            read,
            1_000.0,
            QueryCosts { aborted: Some(60.0), aborted_mean_ms: Some(30_050.0), ..Default::default() },
        ),
    );
    findings.extend(run(
        "failing-query",
        &failing(
            "mssql-demo",
            read,
            500.0,
            QueryCosts {
                aborted: Some(50.0),
                failed: Some(30.0),
                aborted_mean_ms: Some(5_000.0),
                ..Default::default()
            },
        ),
    ));
    findings.extend(run(
        "failing-query",
        &failing("mssql-demo", read, 200.0, QueryCosts { failed: Some(50.0), ..Default::default() }),
    ));
    let mut s = failing("mssql-demo", read, 10.0, QueryCosts { aborted: Some(10.0), ..Default::default() });
    stopped(&mut s);
    findings.extend(run("failing-query", &s));
    let mut s = failing("mssql-demo", read, 200.0, QueryCosts { failed: Some(50.0), ..Default::default() });
    s.server.set_platform(Some(culprit_core::Platform::AzureSqlDatabase));
    findings.extend(run("failing-query", &s));
    says(
        &findings,
        r#"id: failing-query:7
title: A read from orders times out 6% of the time, at about 30 s
summary: 60 of its 1.0K runs in the last 5 hours didn't finish. 60 were cancelled by the client after 30.1 s on average, which is what a command timeout looks like. A run that doesn't finish still costs the time it ran, and the application retries it or shows an error.
impact: 6% of runs fail
metric: Runs
metric: Didn't finish
metric: Cancelled
metric: Cancelled after
fix: Cancelled runs are almost always the application's command timeout (here about 30 s; 30 s is the default in ADO.NET and most drivers). Make the statement faster first: open its plan. If it legitimately runs long, raise the timeout for this call only.
verify: Re-scan after the change: runs that fail or are cancelled should drop to near zero.

id: failing-query:7
title: A read from orders fails or times out 16% of the time
summary: 80 of its 500 runs in the last 5 hours didn't finish. 50 were cancelled by the client after 5.0 s on average. 30 ended in an error. A run that doesn't finish still costs the time it ran, and the application retries it or shows an error.
impact: 16% of runs fail
metric: Runs
metric: Didn't finish
metric: Cancelled
metric: Errors
metric: Cancelled after
fix: Cancelled runs are almost always the application's command timeout (30 s is the default in ADO.NET and most drivers). Make the statement faster first: open its plan. If it legitimately runs long, raise the timeout for this call only.
verify: Re-scan after the change: runs that fail or are cancelled should drop to near zero.

id: failing-query:7
title: A read from orders fails 25% of the time
summary: 50 of its 200 runs in the last 5 hours didn't finish. 50 ended in an error. A run that doesn't finish still costs the time it ran, and the application retries it or shows an error.
impact: 25% of runs fail
metric: Runs
metric: Didn't finish
metric: Errors
fix: Query Store counts the errors but doesn't keep them. Capture them for a while with an Extended Events session on error_reported for this database, read the ring buffer, then drop the session.
sql: -- Capture errors raised in this database (one session for every failing statement: once is enough)
IF NOT EXISTS (SELECT 1 FROM sys.server_event_sessions WHERE name = N'culprit_errors')
    CREATE EVENT SESSION culprit_errors ON SERVER
    ADD EVENT sqlserver.error_reported (
      ACTION (sqlserver.client_app_name, sqlserver.sql_text)
      WHERE severity >= 11 AND sqlserver.database_name = N'culprit_demo')
    ADD TARGET package0.ring_buffer;
IF NOT EXISTS (SELECT 1 FROM sys.dm_xe_sessions WHERE name = N'culprit_errors')
    ALTER EVENT SESSION culprit_errors ON SERVER STATE = START;
-- Once it has caught what you need, stop and remove it: DROP EVENT SESSION culprit_errors ON SERVER;
verify: Re-scan after the change: runs that fail or are cancelled should drop to near zero.

id: failing-query:7
title: A read from orders is cancelled on every run
summary: None of its 10 runs in the last 3 days finished. 10 were cancelled by the client. A run that doesn't finish still costs the time it ran, and the application retries it or shows an error.
impact: 100% of runs fail
metric: Runs
metric: Didn't finish
metric: Cancelled
fix: Cancelled runs are almost always the application's command timeout (30 s is the default in ADO.NET and most drivers). Make the statement faster first: open its plan. If it legitimately runs long, raise the timeout for this call only.
verify: Re-scan after the change: runs that fail or are cancelled should drop to near zero.

id: failing-query:7
title: A read from orders fails 25% of the time
summary: 50 of its 200 runs in the last 5 hours didn't finish. 50 ended in an error. A run that doesn't finish still costs the time it ran, and the application retries it or shows an error.
impact: 25% of runs fail
metric: Runs
metric: Didn't finish
metric: Errors
fix: Query Store counts the errors but doesn't keep them. Capture them for a while with an Extended Events session on error_reported for this database, read the ring buffer, then drop the session.
sql: -- Capture errors raised in this database (one session for every failing statement: once is enough)
IF NOT EXISTS (SELECT 1 FROM sys.database_event_sessions WHERE name = N'culprit_errors')
    CREATE EVENT SESSION culprit_errors ON DATABASE
    ADD EVENT sqlserver.error_reported (
      ACTION (sqlserver.client_app_name, sqlserver.sql_text)
      WHERE severity >= 11 AND sqlserver.database_name = N'culprit_demo')
    ADD TARGET package0.ring_buffer;
IF NOT EXISTS (SELECT 1 FROM sys.dm_xe_database_sessions WHERE name = N'culprit_errors')
    ALTER EVENT SESSION culprit_errors ON DATABASE STATE = START;
-- Once it has caught what you need, stop and remove it: DROP EVENT SESSION culprit_errors ON DATABASE;
verify: Re-scan after the change: runs that fail or are cancelled should drop to near zero."#,
    );
}

#[test]
fn failing_runs_on_mysql() {
    use culprit_core::QueryCosts;
    let update = "UPDATE `orders` SET `status` = ? WHERE `id` = ?";
    let error = |e: &str| QueryCosts { failed: Some(30.0), top_error: Some(e.into()), ..Default::default() };
    let mut findings = Vec::new();
    for e in ["1205 lock wait timeout", "1213 deadlock", "3024 statement timeout", "1062 duplicate entry", "1048 null"]
    {
        findings.extend(run("failing-query", &failing("mysql-demo", update, 200.0, error(e))));
    }
    let old = QueryCosts { failed: Some(20.0), ..Default::default() };
    findings.extend(run("failing-query", &failing("mysql-demo", update, 200.0, old)));
    says(
        &findings,
        r#"id: failing-query:04cd7be9c366fadc
title: An update of orders fails 15% of the time (1205 lock wait timeout)
summary: 30 of its 200 runs since its statistics began 2 minutes ago didn't finish. 30 ended in an error; the recent ones were 1205 lock wait timeout. A run that doesn't finish still costs the time it ran, and the application retries it or shows an error.
impact: 15% of runs fail
metric: Runs
metric: Didn't finish
metric: Errors
metric: Recent error = 1205 lock wait timeout
fix: Another transaction holds the rows it needs for longer than innodb_lock_wait_timeout. Find what holds the locks while it waits (Live shows the blocking chain). Keep transactions short. The recent errors and their messages are in performance_schema.
sql: SELECT MYSQL_ERRNO, RETURNED_SQLSTATE, MESSAGE_TEXT
  FROM performance_schema.events_statements_history
 WHERE DIGEST = 'abc123' AND MYSQL_ERRNO <> 0;
verify: Between two scans, runs that fail or are cancelled should drop to near zero: performance_schema keeps running totals, so compare its failed runs and runs in a scan taken once the change is live and one a day or more later (30 of 200 runs in this scan).
caveat: MySQL counts errors from when each statement's entry began counting, so old failures stay in its total until the counters are reset or the server restarts.

id: failing-query:04cd7be9c366fadc
title: An update of orders fails 15% of the time (1213 deadlock)
summary: 30 of its 200 runs since its statistics began 2 minutes ago didn't finish. 30 ended in an error; the recent ones were 1213 deadlock. A run that doesn't finish still costs the time it ran, and the application retries it or shows an error.
impact: 15% of runs fail
metric: Runs
metric: Didn't finish
metric: Errors
metric: Recent error = 1213 deadlock
fix: It loses deadlocks: see Deadlocks for the other statement and the order to take locks in. The recent errors and their messages are in performance_schema.
sql: SELECT MYSQL_ERRNO, RETURNED_SQLSTATE, MESSAGE_TEXT
  FROM performance_schema.events_statements_history
 WHERE DIGEST = 'abc123' AND MYSQL_ERRNO <> 0;
verify: Between two scans, runs that fail or are cancelled should drop to near zero: performance_schema keeps running totals, so compare its failed runs and runs in a scan taken once the change is live and one a day or more later (30 of 200 runs in this scan).
caveat: MySQL counts errors from when each statement's entry began counting, so old failures stay in its total until the counters are reset or the server restarts.

id: failing-query:04cd7be9c366fadc
title: An update of orders fails 15% of the time (3024 statement timeout)
summary: 30 of its 200 runs since its statistics began 2 minutes ago didn't finish. 30 ended in an error; the recent ones were 3024 statement timeout. A run that doesn't finish still costs the time it ran, and the application retries it or shows an error.
impact: 15% of runs fail
metric: Runs
metric: Didn't finish
metric: Errors
metric: Recent error = 3024 statement timeout
fix: It hits the statement time limit: make it faster (open its plan), or raise the limit for this statement only. The recent errors and their messages are in performance_schema.
sql: SELECT MYSQL_ERRNO, RETURNED_SQLSTATE, MESSAGE_TEXT
  FROM performance_schema.events_statements_history
 WHERE DIGEST = 'abc123' AND MYSQL_ERRNO <> 0;
verify: Between two scans, runs that fail or are cancelled should drop to near zero: performance_schema keeps running totals, so compare its failed runs and runs in a scan taken once the change is live and one a day or more later (30 of 200 runs in this scan).
caveat: MySQL counts errors from when each statement's entry began counting, so old failures stay in its total until the counters are reset or the server restarts.

id: failing-query:04cd7be9c366fadc
title: An update of orders fails 15% of the time (1062 duplicate entry)
summary: 30 of its 200 runs since its statistics began 2 minutes ago didn't finish. 30 ended in an error; the recent ones were 1062 duplicate entry. A run that doesn't finish still costs the time it ran, and the application retries it or shows an error.
impact: 15% of runs fail
metric: Runs
metric: Didn't finish
metric: Errors
metric: Recent error = 1062 duplicate entry
fix: It inserts rows that already exist: use INSERT … ON DUPLICATE KEY UPDATE, or check first. The recent errors and their messages are in performance_schema.
sql: SELECT MYSQL_ERRNO, RETURNED_SQLSTATE, MESSAGE_TEXT
  FROM performance_schema.events_statements_history
 WHERE DIGEST = 'abc123' AND MYSQL_ERRNO <> 0;
verify: Between two scans, runs that fail or are cancelled should drop to near zero: performance_schema keeps running totals, so compare its failed runs and runs in a scan taken once the change is live and one a day or more later (30 of 200 runs in this scan).
caveat: MySQL counts errors from when each statement's entry began counting, so old failures stay in its total until the counters are reset or the server restarts.

id: failing-query:04cd7be9c366fadc
title: An update of orders fails 15% of the time (1048 null)
summary: 30 of its 200 runs since its statistics began 2 minutes ago didn't finish. 30 ended in an error; the recent ones were 1048 null. A run that doesn't finish still costs the time it ran, and the application retries it or shows an error.
impact: 15% of runs fail
metric: Runs
metric: Didn't finish
metric: Errors
metric: Recent error = 1048 null
fix: Read the recent errors with the statement below, then fix the cause in the application or the statement. The recent errors and their messages are in performance_schema.
sql: SELECT MYSQL_ERRNO, RETURNED_SQLSTATE, MESSAGE_TEXT
  FROM performance_schema.events_statements_history
 WHERE DIGEST = 'abc123' AND MYSQL_ERRNO <> 0;
verify: Between two scans, runs that fail or are cancelled should drop to near zero: performance_schema keeps running totals, so compare its failed runs and runs in a scan taken once the change is live and one a day or more later (30 of 200 runs in this scan).
caveat: MySQL counts errors from when each statement's entry began counting, so old failures stay in its total until the counters are reset or the server restarts.

id: failing-query:04cd7be9c366fadc
title: An update of orders fails 10% of the time
summary: 20 of its 200 runs since its statistics began 2 minutes ago didn't finish. 20 ended in an error. A run that doesn't finish still costs the time it ran, and the application retries it or shows an error.
impact: 10% of runs fail
metric: Runs
metric: Didn't finish
metric: Errors
fix: Read the recent errors with the statement below, then fix the cause in the application or the statement. The recent errors and their messages are in performance_schema.
sql: SELECT MYSQL_ERRNO, RETURNED_SQLSTATE, MESSAGE_TEXT
  FROM performance_schema.events_statements_history
 WHERE DIGEST = 'abc123' AND MYSQL_ERRNO <> 0;
verify: Between two scans, runs that fail or are cancelled should drop to near zero: performance_schema keeps running totals, so compare its failed runs and runs in a scan taken once the change is live and one a day or more later (20 of 200 runs in this scan).
caveat: MySQL counts errors per statement from when it first saw the statement, with no time for each, and none of its runs in the recent statement history performance_schema keeps failed: these failures may be old. Culprit raises it once a recent run fails too."#,
    );
}

#[test]
fn failing_runs_told_in_the_past() {
    use culprit_core::QueryCosts;
    let read = "SELECT id FROM dbo.orders WHERE customer_id = @P1";
    let shapes = [
        ("mssql-demo", QueryCosts { aborted: Some(60.0), aborted_mean_ms: Some(30_050.0), ..Default::default() }),
        ("mssql-demo", QueryCosts { aborted: Some(50.0), failed: Some(30.0), ..Default::default() }),
        ("mssql-demo", QueryCosts { failed: Some(50.0), ..Default::default() }),
        (
            "mysql-demo",
            QueryCosts { failed: Some(50.0), top_error: Some("1062 duplicate entry".into()), ..Default::default() },
        ),
    ];
    let mut findings = Vec::new();
    for (name, costs) in shapes {
        let mut s = failing(name, read, 500.0, costs);
        stopped(&mut s);
        findings.extend(run("failing-query", &s));
    }
    says(
        &findings,
        r#"id: failing-query:7
title: A read from orders timed out 12% of the time, at about 30 s
summary: 60 of its 500 runs in the last 3 days didn't finish. 60 were cancelled by the client after 30.1 s on average, which is what a command timeout looks like. A run that doesn't finish still costs the time it ran, and the application retries it or shows an error.
impact: 12% of runs fail
metric: Runs
metric: Didn't finish
metric: Cancelled
metric: Cancelled after
fix: Cancelled runs are almost always the application's command timeout (here about 30 s; 30 s is the default in ADO.NET and most drivers). Make the statement faster first: open its plan. If it legitimately runs long, raise the timeout for this call only.
verify: Re-scan after the change: runs that fail or are cancelled should drop to near zero.

id: failing-query:7
title: A read from orders failed or timed out 16% of the time
summary: 80 of its 500 runs in the last 3 days didn't finish. 50 were cancelled by the client. 30 ended in an error. A run that doesn't finish still costs the time it ran, and the application retries it or shows an error.
impact: 16% of runs fail
metric: Runs
metric: Didn't finish
metric: Cancelled
metric: Errors
fix: Cancelled runs are almost always the application's command timeout (30 s is the default in ADO.NET and most drivers). Make the statement faster first: open its plan. If it legitimately runs long, raise the timeout for this call only.
verify: Re-scan after the change: runs that fail or are cancelled should drop to near zero.

id: failing-query:7
title: A read from orders failed 10% of the time
summary: 50 of its 500 runs in the last 3 days didn't finish. 50 ended in an error. A run that doesn't finish still costs the time it ran, and the application retries it or shows an error.
impact: 10% of runs fail
metric: Runs
metric: Didn't finish
metric: Errors
fix: Query Store counts the errors but doesn't keep them. Capture them for a while with an Extended Events session on error_reported for this database, read the ring buffer, then drop the session.
sql: -- Capture errors raised in this database (one session for every failing statement: once is enough)
IF NOT EXISTS (SELECT 1 FROM sys.server_event_sessions WHERE name = N'culprit_errors')
    CREATE EVENT SESSION culprit_errors ON SERVER
    ADD EVENT sqlserver.error_reported (
      ACTION (sqlserver.client_app_name, sqlserver.sql_text)
      WHERE severity >= 11 AND sqlserver.database_name = N'culprit_demo')
    ADD TARGET package0.ring_buffer;
IF NOT EXISTS (SELECT 1 FROM sys.dm_xe_sessions WHERE name = N'culprit_errors')
    ALTER EVENT SESSION culprit_errors ON SERVER STATE = START;
-- Once it has caught what you need, stop and remove it: DROP EVENT SESSION culprit_errors ON SERVER;
verify: Re-scan after the change: runs that fail or are cancelled should drop to near zero.

id: failing-query:04cd7be9c366fadc
title: A read from orders failed 10% of the time (1062 duplicate entry)
summary: 50 of its 500 runs in the last 3 days didn't finish. 50 ended in an error; the recent ones were 1062 duplicate entry. A run that doesn't finish still costs the time it ran, and the application retries it or shows an error.
impact: 10% of runs fail
metric: Runs
metric: Didn't finish
metric: Errors
metric: Recent error = 1062 duplicate entry
fix: It inserts rows that already exist: use INSERT … ON DUPLICATE KEY UPDATE, or check first. The recent errors and their messages are in performance_schema.
sql: SELECT MYSQL_ERRNO, RETURNED_SQLSTATE, MESSAGE_TEXT
  FROM performance_schema.events_statements_history
 WHERE DIGEST = 'abc123' AND MYSQL_ERRNO <> 0;
verify: Re-scan after the change: runs that fail or are cancelled should drop to near zero.
caveat: MySQL counts errors from when each statement's entry began counting, so old failures stay in its total until the counters are reset or the server restarts."#,
    );
}

// ── hot-rows ───────────────────────────────────────────────────────────────────────────────────────

/// Lock waits on one index, by its position: (waits, ms) at the earlier scan and now, and escalations now.
type IndexWaits = (usize, (f64, f64), (f64, f64), f64);

/// The SQL Server demo with lock waits on its indexes, the earlier scan `before_hours` ago.
fn hot(before_hours: Option<f64>, rcsi: &str, counts: &[IndexWaits]) -> Vec<Finding> {
    use culprit_core::format::{epoch_seconds, iso_from_epoch};
    let mut s = fixture("mssql-demo");
    s.indexes.iter_mut().for_each(|i| i.contention = None);
    set(&mut s, "is_read_committed_snapshot_on", rcsi);
    let mut before = s.clone();
    for &(i, then, now, escalations) in counts {
        before.indexes[i].contention = Some(culprit_core::IndexContention {
            row_lock_waits: then.0,
            row_lock_wait_ms: then.1,
            ..Default::default()
        });
        s.indexes[i].contention = Some(culprit_core::IndexContention {
            row_lock_waits: now.0,
            row_lock_wait_ms: now.1,
            lock_escalations: escalations,
            ..Default::default()
        });
    }
    if let Some(hours) = before_hours {
        before.taken_at = iso_from_epoch(epoch_seconds(&s.taken_at).expect("a time") - hours * 3600.0);
        s.contention_before = culprit_core::signals::ContentionBaseline::from_scan(&before, &s);
        assert!(s.contention_before.is_some(), "a comparable earlier scan");
    }
    run("hot-rows", &s)
}

#[test]
fn hot_rows_without_an_earlier_scan() {
    let mut findings = hot(None, "0", &[(0, (0.0, 0.0), (1.0, 20_000.0), 0.0)]);
    findings.extend(hot(None, "1", &[(5, (0.0, 0.0), (40.0, 90_000.0), 3.0), (6, (0.0, 0.0), (10.0, 30_000.0), 0.0)]));
    says(
        &findings,
        r#"id: hot-rows:shop.customers
title: One historical lock wait on customers took 20.0 s
summary: Since its index pk_customers was loaded into memory, sessions waited once for locks on the rows and pages of customers, 20.0 s in all, 20.0 s each on average. Waits like these come from transactions that hold rows for long, or from writes whose filter isn't indexed and so lock more rows than they change. No comparable earlier counters place these waits in the selected window or establish that contention continues.
impact: 20.0 s waiting on locks
metric: Historical lock waits
metric: Historical time waiting
metric: Historical average wait
fix: These counters alone justify no change. If the application experiences blocking, capture the current blocked statement and its blocker under representative load before changing transactions or indexes.
verify: Compare new lock waits and their time between scans under comparable load on customers. An unchanged counter reports no new waits; an idle interval alone does not verify a fix.
caveat: Counted since the indexes' metadata was loaded into memory, usually since SQL Server started.

id: hot-rows:shop.orders
title: orders has 50 recorded historical lock waits
summary: Since its indexes were loaded into memory, sessions waited on pk_orders and ix_orders_customer_id 50 times for locks on the rows and pages of orders, 2 min in all, 2.4 s each on average. 3 times SQL Server escalated many row locks to a lock on the whole table, which can block sessions needing incompatible locks. Waits like these come from transactions that hold rows for long, or from writes whose filter isn't indexed and so lock more rows than they change. No comparable earlier counters place these waits in the selected window or establish that contention continues.
impact: 2 min waiting on locks
metric: Historical lock waits
metric: Historical time waiting
metric: Historical average wait
metric: Escalations since SQL Server started
fix: These counters alone justify no change. If the application experiences blocking, capture the current blocked statement and its blocker under representative load before changing transactions or indexes.
verify: Compare new lock waits and their time between scans under comparable load on orders. An unchanged counter reports no new waits; an idle interval alone does not verify a fix.
caveat: Counted since the indexes' metadata was loaded into memory, usually since SQL Server started."#,
    );
}

#[test]
fn hot_rows_since_an_earlier_scan() {
    let mut findings = hot(Some(2.0), "0", &[(0, (10.0, 5_000.0), (15.0, 25_000.0), 0.0)]);
    findings.extend(hot(Some(30.0), "1", &[(0, (10.0, 5_000.0), (11.0, 10_000.0), 2.0)]));
    findings.extend(hot(Some(150.0), "1", &[(0, (10.0, 20_000.0), (10.0, 20_000.0), 0.0)]));
    findings.extend(hot(Some(30.0), "1", &[(0, (10.0, 20_000.0), (10.0, 35_000.0), 0.0)]));
    findings.extend(hot(Some(300.0), "1", &[(0, (10.0, 5_000.0), (20.0, 25_000.0), 0.0)]));
    findings.extend(hot(Some(0.5), "1", &[(0, (10.0, 5_000.0), (20.0, 25_000.0), 0.0)]));
    says(
        &findings,
        r#"id: hot-rows:shop.customers
title: 5 lock waits on customers averaged 4.0 s since Oct 11, 10:13 UTC
summary: Between Oct 11, 10:13 UTC and this scan, 5 lock waits totaling 20.0 s were recorded on customers. Since its index pk_customers was loaded into memory, sessions waited 15 times for locks on the rows and pages of customers, 25.0 s in all, 1.7 s each on average. Waits like these come from transactions that hold rows for long, or from writes whose filter isn't indexed and so lock more rows than they change.
impact: 20.0 s waiting on locks
metric: Historical lock waits
metric: Historical time waiting
metric: Historical average wait
metric: Lock waits since earlier scan
metric: Time waiting since earlier scan
metric: Counter interval
fix: Keep the transactions that change customers short and commit before slow work; make their WHERE use an index so they lock only the rows they change. If readers are the ones waiting, READ_COMMITTED_SNAPSHOT lets them read the last committed version instead: it changes what readers see and uses tempdb, so test it first.
sql: -- Readers stop waiting for writers; test first
ALTER DATABASE CURRENT SET READ_COMMITTED_SNAPSHOT ON WITH ROLLBACK IMMEDIATE;
verify: Compare new lock waits and their time between scans under comparable load on customers. An unchanged counter reports no new waits; an idle interval alone does not verify a fix.
caveat: Counted since the indexes' metadata was loaded into memory, usually since SQL Server started.

id: hot-rows:shop.customers
title: One lock wait on customers took 5.0 s since Oct 10, 06:13 UTC
summary: Between Oct 10, 06:13 UTC and this scan, 1 lock wait totaling 5.0 s was recorded on customers. Since its index pk_customers was loaded into memory, sessions waited 11 times for locks on the rows and pages of customers, 10.0 s in all, 909 ms each on average. 2 times SQL Server escalated many row locks to a lock on the whole table, which can block sessions needing incompatible locks. Waits like these come from transactions that hold rows for long, or from writes whose filter isn't indexed and so lock more rows than they change.
impact: 5.0 s waiting on locks
metric: Historical lock waits
metric: Historical time waiting
metric: Historical average wait
metric: Lock waits since earlier scan
metric: Time waiting since earlier scan
metric: Counter interval
metric: Waiting a day
metric: Escalations since SQL Server started
fix: Keep the transactions that change customers short and commit before slow work; make their WHERE use an index so they lock only the rows they change.
verify: Compare new lock waits and their time between scans under comparable load on customers. An unchanged counter reports no new waits; an idle interval alone does not verify a fix.
caveat: Counted since the indexes' metadata was loaded into memory, usually since SQL Server started.

id: hot-rows:shop.customers
title: No new lock waits on customers since Oct 5, 06:13 UTC
summary: Between Oct 5, 06:13 UTC and this scan, 0 lock waits totaling 0 ms were recorded on customers. The earlier waits do not establish contention in the selected window. Since its index pk_customers was loaded into memory, sessions waited 10 times for locks on the rows and pages of customers, 20.0 s in all, 2.0 s each on average. Waits like these come from transactions that hold rows for long, or from writes whose filter isn't indexed and so lock more rows than they change.
impact: No new lock waits
metric: Historical lock waits
metric: Historical time waiting
metric: Historical average wait
metric: Lock waits since earlier scan
metric: Time waiting since earlier scan
metric: Counter interval
metric: Waiting a day
fix: These counters alone justify no change. If the application experiences blocking, capture the current blocked statement and its blocker under representative load before changing transactions or indexes.
verify: Compare new lock waits and their time between scans under comparable load on customers. An unchanged counter reports no new waits; an idle interval alone does not verify a fix.
caveat: Counted since the indexes' metadata was loaded into memory, usually since SQL Server started.

id: hot-rows:shop.customers
title: Recorded lock wait time on customers grew by 15.0 s since Oct 10, 06:13 UTC
summary: Between Oct 10, 06:13 UTC and this scan, 0 lock waits totaling 15.0 s were recorded on customers. Since its index pk_customers was loaded into memory, sessions waited 10 times for locks on the rows and pages of customers, 35.0 s in all, 3.5 s each on average. Waits like these come from transactions that hold rows for long, or from writes whose filter isn't indexed and so lock more rows than they change.
impact: 15.0 s waiting on locks
metric: Historical lock waits
metric: Historical time waiting
metric: Historical average wait
metric: Lock waits since earlier scan
metric: Time waiting since earlier scan
metric: Counter interval
metric: Waiting a day
fix: Keep the transactions that change customers short and commit before slow work; make their WHERE use an index so they lock only the rows they change.
verify: Compare new lock waits and their time between scans under comparable load on customers. An unchanged counter reports no new waits; an idle interval alone does not verify a fix.
caveat: Counted since the indexes' metadata was loaded into memory, usually since SQL Server started.

id: hot-rows:shop.customers
title: 10 lock waits on customers averaged 2.0 s since Sep 29, 00:13 UTC
summary: Between Sep 29, 00:13 UTC and this scan, 10 lock waits totaling 20.0 s were recorded on customers. Since its index pk_customers was loaded into memory, sessions waited 20 times for locks on the rows and pages of customers, 25.0 s in all, 1.2 s each on average. Waits like these come from transactions that hold rows for long, or from writes whose filter isn't indexed and so lock more rows than they change. This counter interval extends beyond the selected window, so the scan cannot place these waits inside that window.
impact: 20.0 s waiting on locks
metric: Historical lock waits
metric: Historical time waiting
metric: Historical average wait
metric: Lock waits since earlier scan
metric: Time waiting since earlier scan
metric: Counter interval
fix: These counters alone justify no change. If the application experiences blocking, capture the current blocked statement and its blocker under representative load before changing transactions or indexes.
verify: Compare new lock waits and their time between scans under comparable load on customers. An unchanged counter reports no new waits; an idle interval alone does not verify a fix.
caveat: Counted since the indexes' metadata was loaded into memory, usually since SQL Server started.

id: hot-rows:shop.customers
title: 10 lock waits on customers averaged 2.0 s since Oct 11, 11:43 UTC
summary: Between Oct 11, 11:43 UTC and this scan, 10 lock waits totaling 20.0 s were recorded on customers. Since its index pk_customers was loaded into memory, sessions waited 20 times for locks on the rows and pages of customers, 25.0 s in all, 1.2 s each on average. Waits like these come from transactions that hold rows for long, or from writes whose filter isn't indexed and so lock more rows than they change.
impact: 20.0 s waiting on locks
metric: Historical lock waits
metric: Historical time waiting
metric: Historical average wait
metric: Lock waits since earlier scan
metric: Time waiting since earlier scan
metric: Counter interval
fix: Keep the transactions that change customers short and commit before slow work; make their WHERE use an index so they lock only the rows they change.
verify: Compare new lock waits and their time between scans under comparable load on customers. An unchanged counter reports no new waits; an idle interval alone does not verify a fix.
caveat: Counted since the indexes' metadata was loaded into memory, usually since SQL Server started.
caveat: The comparison covers less than an hour: only its measured total is graded, without extrapolating a daily rate."#,
    );
}

// ── missing-histogram ──────────────────────────────────────────────────────────────────────────────

/// The MySQL demo's statements with plans made to filter `table` on `flavor` with the optimizer's fixed guess.
fn guessed(product: &str, filtered: f64, statements: usize) -> Vec<Finding> {
    let mut s = fixture("mysql-demo");
    s.server.product = product.into();
    let table = s.tables.iter().find(|t| t.rows.unwrap_or(0.0) >= 1_000.0).expect("a table").clone();
    let w = s.workload.as_mut().expect("workload");
    w.queries.sort_by(|a, b| b.total_time_ms.total_cmp(&a.total_time_ms));
    let mut planted = 0;
    for q in w.queries.iter_mut().filter(|q| q.plan.is_some()) {
        if planted == statements {
            q.plan = None;
            continue;
        }
        q.text = format!("SELECT * FROM `{}` WHERE `flavor` = ? AND `size` > ?", table.name);
        let plan = q.plan.as_mut().unwrap();
        plan.root.object = Some(format!("`{}`.`{}`", table.schema, table.name));
        plan.root.predicate =
            Some(format!("((`{0}`.`{1}`.`flavor` = 'x') and (`{0}`.`{1}`.`size` > 3))", table.schema, table.name));
        plan.root.props.insert("filtered".into(), format!("{filtered}"));
        plan.root.facts_mut().estimated_filtered_percent = Some(filtered);
        planted += 1;
    }
    s.histograms = Some(vec![]);
    run("missing-histogram", &s).into_iter().filter(|f| f.subject.name.ends_with(&format!(".{}", table.name))).collect()
}

#[test]
fn guessed_filters_on_mysql_and_mariadb() {
    let mut findings = guessed("MySQL 8.4.11", 100.0 / 30.0, 1);
    findings.extend(guessed("MySQL 8.4.11", 100.0 / 30.0, 2));
    findings.extend(guessed("MariaDB 11.8.9", 100.0, 1));
    says(
        &findings,
        r#"id: missing-histogram:shop.customers
title: MySQL guesses how many customers rows match flavor and size: no histogram
summary: A read from customers filters customers on flavor and size, which no index starts with and no histogram describes, so MySQL assumes a fixed share of rows match (10% for =, a third for ranges); EXPLAIN shows filtered = 3.33%, from those guesses. When the real share is far off, common for status flags and skewed values, it picks the wrong join order or access path. A histogram costs nothing at query time and changes only when you run ANALYZE again.
impact: 77% of DB time
metric: Columns = flavor, size
metric: EXPLAIN filtered
fix: Build a histogram for these columns. It reads the table once; MySQL doesn't refresh it on its own, so run it again after large data changes.
cost: Reads the table
sql: ANALYZE TABLE `shop`.`customers` UPDATE HISTOGRAM ON `flavor`, `size` WITH 64 BUCKETS;
verify: Re-scan: the columns should have histograms, and EXPLAIN ANALYZE should show estimated rows close to actual for that filter.

id: missing-histogram:shop.customers
title: MySQL guesses how many customers rows match flavor and size: no histogram
summary: 2 of the heaviest queries filter customers on flavor and size, which no index starts with and no histogram describes, so MySQL assumes a fixed share of rows match (10% for =, a third for ranges); EXPLAIN shows filtered = 3.33%, from those guesses. When the real share is far off, common for status flags and skewed values, it picks the wrong join order or access path. A histogram costs nothing at query time and changes only when you run ANALYZE again.
impact: 86% of DB time
metric: Columns = flavor, size
metric: EXPLAIN filtered
fix: Build a histogram for these columns. It reads the table once; MySQL doesn't refresh it on its own, so run it again after large data changes.
cost: Reads the table
sql: ANALYZE TABLE `shop`.`customers` UPDATE HISTOGRAM ON `flavor`, `size` WITH 64 BUCKETS;
verify: Re-scan: the columns should have histograms, and EXPLAIN ANALYZE should show estimated rows close to actual for that filter.

id: missing-histogram:shop.customers
title: MariaDB guesses how many customers rows match flavor and size: no column statistics
summary: A read from customers filters customers on flavor and size, which no index starts with and no statistics describe, so MariaDB assumes the condition keeps every row (EXPLAIN shows filtered = 100%). When it actually keeps few, common for status flags and skewed values, it picks the wrong join order or access path. Column statistics cost nothing at query time and change only when you run ANALYZE again.
impact: 77% of DB time
metric: Columns = flavor, size
metric: EXPLAIN filtered
fix: Collect a histogram for these columns. MariaDB uses it when use_stat_tables is PREFERABLY_FOR_QUERIES and optimizer_use_condition_selectivity is 4 or more, the defaults since 10.4. Run it again after large data changes.
cost: Reads the whole table
sql: ANALYZE TABLE `shop`.`customers` PERSISTENT FOR COLUMNS (`flavor`, `size`) INDEXES ();
verify: Re-scan: the columns should have histograms, and EXPLAIN ANALYZE should show estimated rows close to actual for that filter."#,
    );
}

// ── slow-storage ───────────────────────────────────────────────────────────────────────────────────

fn file(name: &str, db: &str, kind: FileKind, reads: (f64, f64, f64), writes: (f64, f64)) -> culprit_core::FileIo {
    culprit_core::FileIo {
        name: name.into(),
        kind,
        database: Some(db.into()),
        reads: reads.0,
        read_ms: Some(reads.1),
        read_bytes: Some(reads.2),
        writes: writes.0,
        write_ms: Some(writes.1),
        write_bytes: None,
        scope: None,
        stats_reset: None,
        write_operation: None,
    }
}

fn slow(name: &str, uptime_hours: Option<f64>, files: Vec<culprit_core::FileIo>) -> Snapshot {
    let mut s = fixture(name);
    let p = s.pressure.get_or_insert_with(Default::default);
    p.uptime_hours = uptime_hours;
    p.files = files;
    s
}

#[test]
fn slow_storage_on_sql_server() {
    let db = "culprit_demo";
    let s = slow(
        "mssql-demo",
        Some(72.0),
        vec![
            file("shop_data", db, FileKind::Data, (50_000.0, 2_000_000.0, 50_000.0 * 8_192.0), (0.0, 0.0)),
            file("shop_log", db, FileKind::Log, (0.0, 0.0, 0.0), (20_000.0, 400_000.0)),
            file("tempdev", "tempdb", FileKind::Temp, (5_000.0, 150_000.0, 5_000.0 * 8_192.0), (0.0, 0.0)),
            file("other_data", "reports", FileKind::Data, (5_000.0, 150_000.0, 5_000.0 * 512_000.0), (0.0, 0.0)),
            file("other_2", "reports", FileKind::Data, (5_000.0, 150_000.0, 5_000.0 * 512_000.0), (0.0, 0.0)),
        ],
    );
    let mut findings = run("slow-storage", &s);
    let mut s = slow(
        "mssql-demo",
        None,
        vec![culprit_core::FileIo {
            write_operation: Some(culprit_core::FileWriteOperation::Flush),
            ..file("shop_log", db, FileKind::Log, (0.0, 0.0, 0.0), (20_000.0, 400_000.0))
        }],
    );
    s.server.set_platform(Some(culprit_core::Platform::AzureSqlDatabase));
    findings.extend(run("slow-storage", &s));
    s.server.set_platform(Some(culprit_core::Platform::AzureSqlManagedInstance));
    findings.extend(run("slow-storage", &s));
    says(
        &findings,
        r#"id: slow-storage:culprit_demo:shop_data
title: Recorded reads from culprit_demo's data file shop_data averaged 40 ms
summary: Reads from culprit_demo's data file shop_data took 40 ms on average across 50K reads since SQL Server started or its statistics were reset, 3.0 days ago. Healthy SSD storage answers in a few milliseconds; when it happens during a query it can delay reads that miss memory. This historical average does not establish a delay now.
impact: 40 ms per read
metric: Average read
metric: Reads
metric: File = shop_data
metric: Average read size
fix: Check the volume's limits first: cloud disks throttle at their provisioned IOPS and throughput, and burst credits run out. Then look for other load on the same disk and antivirus scanning data files, and give the log its own low-latency volume. A since-startup average hides bursts: sample Waits → Right now during the busy hour.
verify: Compare two readings after the change with the same source and reset period: divide the additional read time by the additional operation count. Measure during representative load; the cumulative average includes older operations.
caveat: Historical averages: a slow hour long ago still counts, and a later scan does not isolate new operations without comparing counter differences.

id: slow-storage:reports:data files
title: Recorded reads from reports's data files averaged 30 ms, mostly large reads of 500 KB
summary: Reads from reports's data files took 30 ms on average across 10K reads since SQL Server started or its statistics were reset, 3.0 days ago. Healthy SSD storage answers in a few milliseconds; when it happens during a query it can delay reads that miss memory. This historical average does not establish a delay now. These reads average 500 KB each: mostly large sequential reads (backups, CHECKDB, read-ahead of big scans), which take longer each than the page-sized reads queries wait on, so the average overstates what queries see.
impact: 30 ms per read
metric: Average read
metric: Reads
metric: Files = other_data, other_2
metric: Average read size
fix: Check the volume's limits first: cloud disks throttle at their provisioned IOPS and throughput, and burst credits run out. Then look for other load on the same disk and antivirus scanning data files, and give the log its own low-latency volume. A since-startup average hides bursts: sample Waits → Right now during the busy hour.
verify: Compare two readings after the change with the same source and reset period: divide the additional read time by the additional operation count. Measure during representative load; the cumulative average includes older operations.
caveat: Historical averages: a slow hour long ago still counts, and a later scan does not isolate new operations without comparing counter differences.
caveat: Historical file statistics do not establish that this database's queries are slow now; check the stated source scope and measure recent operations alongside query waits.

id: slow-storage:tempdb:tempdev
title: Recorded reads from tempdb averaged 30 ms
summary: Reads from tempdb took 30 ms on average across 5.0K reads since SQL Server started or its statistics were reset, 3.0 days ago. Healthy SSD storage answers in a few milliseconds; when it happens during a query it can delay reads that miss memory. This historical average does not establish a delay now.
impact: 30 ms per read
metric: Average read
metric: Reads
metric: File = tempdev
metric: Average read size
fix: Check the volume's limits first: cloud disks throttle at their provisioned IOPS and throughput, and burst credits run out. Then look for other load on the same disk and antivirus scanning data files, and give the log its own low-latency volume. A since-startup average hides bursts: sample Waits → Right now during the busy hour.
verify: Compare two readings after the change with the same source and reset period: divide the additional read time by the additional operation count. Measure during representative load; the cumulative average includes older operations.
caveat: Historical averages: a slow hour long ago still counts, and a later scan does not isolate new operations without comparing counter differences.
caveat: Historical file statistics do not establish that this database's queries are slow now; check the stated source scope and measure recent operations alongside query waits.

id: slow-storage:culprit_demo:shop_log
title: Recorded log writes to culprit_demo's log averaged 20 ms
summary: Writes to culprit_demo's log took 20 ms on average across 20K writes since SQL Server started or its statistics were reset, 3.0 days ago. Healthy SSD storage answers in a few milliseconds and log writes in under 2 ms; when it happens during a query it can delay reads that miss memory and every commit. This historical average does not establish a delay now.
impact: 20 ms per write
metric: Average write
metric: Writes
metric: File = shop_log
fix: Check the volume's limits first: cloud disks throttle at their provisioned IOPS and throughput, and burst credits run out. Then look for other load on the same disk and antivirus scanning data files, and give the log its own low-latency volume. A since-startup average hides bursts: sample Waits → Right now during the busy hour.
verify: Compare two readings after the change with the same source and reset period: divide the additional write time by the additional operation count. Measure during representative load; the cumulative average includes older operations.
caveat: Historical averages: a slow hour long ago still counts, and a later scan does not isolate new operations without comparing counter differences.

id: slow-storage:culprit_demo:shop_log
title: Recorded log flushes to culprit_demo's log averaged 20 ms
summary: Flushes to culprit_demo's log took 20 ms on average across 20K flushes since SQL Server started. Healthy SSD storage answers in a few milliseconds and log flushes in under 2 ms; when it happens during a query it can delay reads that miss memory and every commit. This historical average does not establish a delay now.
impact: 20 ms per flush
metric: Average flush
metric: Flushes
metric: File = shop_log
fix: In Azure SQL Database the service tier decides this: General Purpose reads from remote storage (5 to 10 ms is normal there), Hyperscale serves most reads from a local cache and the rest from page servers, Business Critical uses local SSDs. Check avg_data_io_percent and avg_log_write_percent in sys.dm_db_resource_stats: near 100% means the tier's I/O limit is throttling, so add vCores or move to a faster tier; well below it, cut the reads instead (indexes, fewer full scans). A since-startup average hides bursts: sample Waits → Right now during the busy hour.
verify: Compare two readings after the change with the same source and reset period: divide the additional flush time by the additional operation count. Measure during representative load; the cumulative average includes older operations.
caveat: Historical averages: a slow hour long ago still counts, and a later scan does not isolate new operations without comparing counter differences.

id: slow-storage:culprit_demo:shop_log
title: Recorded log flushes to culprit_demo's log averaged 20 ms
summary: Flushes to culprit_demo's log took 20 ms on average across 20K flushes since SQL Server started. Healthy SSD storage answers in a few milliseconds and log flushes in under 2 ms; when it happens during a query it can delay reads that miss memory and every commit. This historical average does not establish a delay now.
impact: 20 ms per flush
metric: Average flush
metric: Flushes
metric: File = shop_log
fix: On Managed Instance General Purpose each data and log file is a remote disk whose IOPS and throughput come from the file's size (up to 128 GB about 500 IOPS, up to 512 GB about 2,300, up to 1 TB 5,000): growing a busy file past the next size step raises its limit, and the next-gen General Purpose tier sets IOPS apart from size. Business Critical uses local SSDs. Compare each file's latency in sys.dm_io_virtual_file_stats with its size, and cut the reads too (indexes, fewer full scans). A since-startup average hides bursts: sample Waits → Right now during the busy hour.
verify: Compare two readings after the change with the same source and reset period: divide the additional flush time by the additional operation count. Measure during representative load; the cumulative average includes older operations.
caveat: Historical averages: a slow hour long ago still counts, and a later scan does not isolate new operations without comparing counter differences."#,
    );
}

#[test]
fn slow_storage_on_postgres_and_mysql() {
    let pg = |kind: FileKind, scope: Option<FileScope>, reset: Option<&str>| culprit_core::FileIo {
        scope,
        stats_reset: reset.map(str::to_string),
        ..file("relation", "demo", kind, (5_000.0, 150_000.0, 5_000.0 * 8_192.0), (5_000.0, 150_000.0))
    };
    let s = slow(
        "postgres-demo",
        Some(72.0),
        vec![
            pg(FileKind::Data, Some(FileScope::Database), Some("2026-10-01T00:00:00Z")),
            pg(FileKind::Temp, Some(FileScope::Server), None),
            pg(FileKind::Log, None, None),
        ],
    );
    let mut findings = run("slow-storage", &s);
    let s = slow(
        "mysql-demo",
        Some(72.0),
        vec![
            file("./shop/orders.ibd", "shop", FileKind::Data, (5_000.0, 150_000.0, 5_000.0 * 16_384.0), (0.0, 0.0)),
            file("./ib_logfile0", "shop", FileKind::Log, (0.0, 0.0, 0.0), (5_000.0, 150_000.0)),
            file("./ib_logfile1", "shop", FileKind::Log, (0.0, 0.0, 0.0), (5_000.0, 150_000.0)),
            file(
                "./#innodb_temp/temp_1.ibt",
                "shop",
                FileKind::Temp,
                (5_000.0, 150_000.0, 5_000.0 * 16_384.0),
                (0.0, 0.0),
            ),
            file(
                "./#innodb_temp/temp_2.ibt",
                "shop",
                FileKind::Temp,
                (5_000.0, 150_000.0, 5_000.0 * 16_384.0),
                (0.0, 0.0),
            ),
        ],
    );
    findings.extend(run("slow-storage", &s));
    says(
        &findings,
        r#"id: slow-storage:demo:relation
title: Recorded reads from table and index files averaged 30 ms
summary: Reads from table and index files took 30 ms on average across 5.0K reads (this database, since 2026-10-01T00:00:00Z). Healthy SSD storage answers in a few milliseconds; when it happens during a query it can delay reads that miss memory. This historical average does not establish a delay now. PostgreSQL's numbers include reads the operating system served from its cache, so reads that reached the disk were slower still.
impact: 30 ms per read
metric: Average read
metric: Reads
metric: File = relation
metric: Average read size
fix: Check the volume's limits first: cloud disks throttle at their provisioned IOPS and throughput, and burst credits run out. Then look for other load on the same disk and antivirus scanning data files, and give the log its own low-latency volume. A since-startup average hides bursts: sample Waits → Right now during the busy hour.
verify: Compare two readings after the change with the same source and reset period: divide the additional read time by the additional operation count. Measure during representative load; the cumulative average includes older operations.
caveat: Historical averages: a slow hour long ago still counts, and a later scan does not isolate new operations without comparing counter differences.
caveat: Historical file statistics do not establish that this database's queries are slow now; check the stated source scope and measure recent operations alongside query waits.

id: slow-storage:demo:relation
title: Recorded reads from temporary tables averaged 30 ms
summary: Reads from temporary tables took 30 ms on average across 5.0K reads (whole server, over an unknown statistics period). Healthy SSD storage answers in a few milliseconds; when it happens during a query it can delay reads that miss memory. This historical average does not establish a delay now. PostgreSQL's numbers include reads the operating system served from its cache, so reads that reached the disk were slower still.
impact: 30 ms per read
metric: Average read
metric: Reads
metric: File = relation
metric: Average read size
fix: Check the volume's limits first: cloud disks throttle at their provisioned IOPS and throughput, and burst credits run out. Then look for other load on the same disk and antivirus scanning data files, and give the log its own low-latency volume. A since-startup average hides bursts: sample Waits → Right now during the busy hour.
verify: Compare two readings after the change with the same source and reset period: divide the additional read time by the additional operation count. Measure during representative load; the cumulative average includes older operations.
caveat: Historical averages: a slow hour long ago still counts, and a later scan does not isolate new operations without comparing counter differences.
caveat: Historical file statistics do not establish that this database's queries are slow now; check the stated source scope and measure recent operations alongside query waits.

id: slow-storage:demo:relation
title: Recorded log writes to WAL averaged 30 ms
summary: Writes to WAL took 30 ms on average across 5.0K writes (recorded file statistics, over an unknown statistics period). Healthy SSD storage answers in a few milliseconds and log writes in under 2 ms; when it happens during a query it can delay reads that miss memory and every commit. This historical average does not establish a delay now. PostgreSQL's numbers include reads the operating system served from its cache, so reads that reached the disk were slower still.
impact: 30 ms per write
metric: Average write
metric: Writes
metric: File = relation
fix: Check the volume's limits first: cloud disks throttle at their provisioned IOPS and throughput, and burst credits run out. Then look for other load on the same disk and antivirus scanning data files, and give the log its own low-latency volume. A since-startup average hides bursts: sample Waits → Right now during the busy hour.
verify: Compare two readings after the change with the same source and reset period: divide the additional write time by the additional operation count. Measure during representative load; the cumulative average includes older operations.
caveat: Historical averages: a slow hour long ago still counts, and a later scan does not isolate new operations without comparing counter differences.
caveat: Historical file statistics do not establish that this database's queries are slow now; check the stated source scope and measure recent operations alongside query waits.

id: slow-storage:shop:redacted-5fc3de68dedcbae19de6274bd873c094985dac3a6cef0c20d60248884ae2905e
title: Recorded reads from ./shop/orders.ibd averaged 30 ms
summary: Reads from ./shop/orders.ibd took 30 ms on average across 5.0K reads since MySQL started or its statistics were reset, 3.0 days ago. Healthy SSD storage answers in a few milliseconds; when it happens during a query it can delay reads that miss memory. This historical average does not establish a delay now. The timings include reads the operating system served from its cache unless InnoDB uses O_DIRECT.
impact: 30 ms per read
metric: Average read
metric: Reads
metric: File = ./shop/orders.ibd
metric: Average read size
fix: Check the volume's limits first: cloud disks throttle at their provisioned IOPS and throughput, and burst credits run out. Then look for other load on the same disk and antivirus scanning data files, and give the log its own low-latency volume. A since-startup average hides bursts: sample Waits → Right now during the busy hour.
verify: Compare two readings after the change with the same source and reset period: divide the additional read time by the additional operation count. Measure during representative load; the cumulative average includes older operations.
caveat: Historical averages: a slow hour long ago still counts, and a later scan does not isolate new operations without comparing counter differences.
caveat: Historical file statistics do not establish that this database's queries are slow now; check the stated source scope and measure recent operations alongside query waits.

id: slow-storage:shop:temp files
title: Recorded reads from temporary files averaged 30 ms
summary: Reads from temporary files took 30 ms on average across 10K reads since MySQL started or its statistics were reset, 3.0 days ago. Healthy SSD storage answers in a few milliseconds; when it happens during a query it can delay reads that miss memory. This historical average does not establish a delay now. The timings include reads the operating system served from its cache unless InnoDB uses O_DIRECT.
impact: 30 ms per read
metric: Average read
metric: Reads
metric: Files = ./#innodb_temp/temp_1.ibt, ./#innodb_temp/temp_2.ibt
metric: Average read size
fix: Check the volume's limits first: cloud disks throttle at their provisioned IOPS and throughput, and burst credits run out. Then look for other load on the same disk and antivirus scanning data files, and give the log its own low-latency volume. A since-startup average hides bursts: sample Waits → Right now during the busy hour.
verify: Compare two readings after the change with the same source and reset period: divide the additional read time by the additional operation count. Measure during representative load; the cumulative average includes older operations.
caveat: Historical averages: a slow hour long ago still counts, and a later scan does not isolate new operations without comparing counter differences.
caveat: Historical file statistics do not establish that this database's queries are slow now; check the stated source scope and measure recent operations alongside query waits.

id: slow-storage:shop:redo log
title: Recorded log writes to redo log averaged 30 ms
summary: Writes to redo log took 30 ms on average across 10K writes since MySQL started or its statistics were reset, 3.0 days ago. Healthy SSD storage answers in a few milliseconds and log writes in under 2 ms; when it happens during a query it can delay reads that miss memory and every commit. This historical average does not establish a delay now. The timings include reads the operating system served from its cache unless InnoDB uses O_DIRECT.
impact: 30 ms per write
metric: Average write
metric: Writes
metric: File = redo log
fix: Check the volume's limits first: cloud disks throttle at their provisioned IOPS and throughput, and burst credits run out. Then look for other load on the same disk and antivirus scanning data files, and give the log its own low-latency volume. A since-startup average hides bursts: sample Waits → Right now during the busy hour.
verify: Compare two readings after the change with the same source and reset period: divide the additional write time by the additional operation count. Measure during representative load; the cumulative average includes older operations.
caveat: Historical averages: a slow hour long ago still counts, and a later scan does not isolate new operations without comparing counter differences.
caveat: Historical file statistics do not establish that this database's queries are slow now; check the stated source scope and measure recent operations alongside query waits."#,
    );
}

#[test]
fn slow_storage_of_several_files_on_mysql() {
    let s = slow(
        "mysql-demo",
        Some(72.0),
        vec![
            file("./shop/orders.ibd", "shop", FileKind::Data, (5_000.0, 150_000.0, 5_000.0 * 16_384.0), (0.0, 0.0)),
            file("./shop/items.ibd", "shop", FileKind::Data, (5_000.0, 150_000.0, 5_000.0 * 16_384.0), (0.0, 0.0)),
            culprit_core::FileIo {
                write_operation: Some(culprit_core::FileWriteOperation::Flush),
                ..file("./binlog.000001", "shop", FileKind::Log, (0.0, 0.0, 0.0), (5_000.0, 150_000.0))
            },
            file("./undo_001", "shop", FileKind::Log, (0.0, 0.0, 0.0), (5_000.0, 150_000.0)),
        ],
    );
    says(
        &run("slow-storage", &s),
        r#"id: slow-storage:shop:data files
title: Recorded reads from data files averaged 30 ms
summary: Reads from data files took 30 ms on average across 10K reads since MySQL started or its statistics were reset, 3.0 days ago. Healthy SSD storage answers in a few milliseconds; when it happens during a query it can delay reads that miss memory. This historical average does not establish a delay now. The timings include reads the operating system served from its cache unless InnoDB uses O_DIRECT.
impact: 30 ms per read
metric: Average read
metric: Reads
metric: Files = ./shop/orders.ibd, ./shop/items.ibd
metric: Average read size
fix: Check the volume's limits first: cloud disks throttle at their provisioned IOPS and throughput, and burst credits run out. Then look for other load on the same disk and antivirus scanning data files, and give the log its own low-latency volume. A since-startup average hides bursts: sample Waits → Right now during the busy hour.
verify: Compare two readings after the change with the same source and reset period: divide the additional read time by the additional operation count. Measure during representative load; the cumulative average includes older operations.
caveat: Historical averages: a slow hour long ago still counts, and a later scan does not isolate new operations without comparing counter differences.
caveat: Historical file statistics do not establish that this database's queries are slow now; check the stated source scope and measure recent operations alongside query waits.

id: slow-storage:shop:log files
title: Recorded log writes to the log files averaged 30 ms
summary: Writes to the log files took 30 ms on average across 10K writes since MySQL started or its statistics were reset, 3.0 days ago. Healthy SSD storage answers in a few milliseconds and log writes in under 2 ms; when it happens during a query it can delay reads that miss memory and every commit. This historical average does not establish a delay now. The timings include reads the operating system served from its cache unless InnoDB uses O_DIRECT.
impact: 30 ms per write
metric: Average write
metric: Writes
metric: Files = ./binlog.000001, ./undo_001
fix: Check the volume's limits first: cloud disks throttle at their provisioned IOPS and throughput, and burst credits run out. Then look for other load on the same disk and antivirus scanning data files, and give the log its own low-latency volume. A since-startup average hides bursts: sample Waits → Right now during the busy hour.
verify: Compare two readings after the change with the same source and reset period: divide the additional write time by the additional operation count. Measure during representative load; the cumulative average includes older operations.
caveat: Historical averages: a slow hour long ago still counts, and a later scan does not isolate new operations without comparing counter differences.
caveat: Historical file statistics do not establish that this database's queries are slow now; check the stated source scope and measure recent operations alongside query waits."#,
    );
}

// ── replica-lag ────────────────────────────────────────────────────────────────────────────────────

fn lagging(name: &str, version: Option<&str>, replicas: Vec<culprit_core::Replica>) -> Vec<Finding> {
    let mut s = fixture(name);
    if let Some(v) = version {
        s.server.version = v.into();
    }
    s.pressure.get_or_insert_with(Default::default).replicas = replicas;
    run("replica-lag", &s)
}

fn replica(name: &str, role: culprit_core::ReplicaRole, state: &str) -> culprit_core::Replica {
    culprit_core::Replica { state: Some(state.into()), ..culprit_core::Replica::new(name, role) }
}

#[test]
fn replicas_stopped_or_left_configured() {
    use culprit_core::{Replica, ReplicaRole::Replica as Sends, ReplicaRole::ThisServer};
    let mut findings = lagging(
        "mysql-demo",
        None,
        vec![
            replica("from 10.0.0.5", ThisServer, "configured, not running (no error; accepts writes)"),
            replica("from 10.0.0.6", ThisServer, "stopped by hand (no error)"),
            Replica {
                error: Some("1062 duplicate entry".into()),
                ..replica("from 10.0.0.7", ThisServer, "stopped (SQL thread error)")
            },
        ],
    );
    findings.extend(lagging(
        "postgres-demo",
        None,
        vec![
            replica("this server", ThisServer, "not receiving"),
            replica("standby-1", Sends, "stopped"),
            Replica { logical: Some(true), ..replica("sub_orders", Sends, "stopped") },
        ],
    ));
    says(
        &findings,
        r#"id: replica-lag:from 10.0.0.5
title: This server is still set up as a replica, but replication isn't running
summary: Both replication threads are off and no error is recorded, so someone stopped them, and this server takes writes (read_only is off). A former replica promoted to primary looks like this: its old replication settings stay behind, and starting them by mistake would apply another server's changes on top of this one's.
impact: configured, not running (no error; accepts writes)
fix: If it is no longer a replica, remove the configuration with RESET REPLICA ALL (RESET SLAVE ALL before MySQL 8.0.22 and MariaDB 10.5.1). If it should replicate, make it read-only and START REPLICA.
verify: Re-scan: the leftover replication configuration should be gone.

id: replica-lag:from 10.0.0.6
title: Replication to this server was stopped by hand
summary: Both replication threads are off and no error is recorded: someone ran STOP REPLICA, for a backup or maintenance perhaps. While it stays stopped the copy gets older by the minute, and failing over to it would lose the changes since.
impact: stopped by hand (no error)
fix: If it should replicate, START REPLICA and let it catch up before relying on it; if it is no longer a replica, RESET REPLICA ALL removes the configuration.
verify: Re-scan: the lag should be seconds.

id: replica-lag:from 10.0.0.7
title: Replication to this server has stopped (stopped (SQL thread error))
summary: The last error was 1062 duplicate entry. The copy no longer keeps up, so its data is getting older by the minute and failing over to it now would lose changes.
impact: stopped (SQL thread error)
fix: Read the full error with SHOW REPLICA STATUS on the replica, fix its cause, then START REPLICA.
verify: Re-scan: the lag should be seconds.

id: replica-lag:this server
title: Replication to this server has stopped (not receiving)
summary: This standby is set up to stream from its primary (primary_conninfo is set) but no WAL receiver is running, so it isn't getting changes: its data is getting older by the minute and failing over to it now would lose them.
impact: not receiving
fix: Find why it stopped (the replica's error log, network, disk space), fix it, and let it catch up before relying on it.
verify: Re-scan: the lag should be seconds.

id: replica-lag:standby-1
title: The replica standby-1 isn't receiving changes (stopped)
summary: The copy no longer keeps up, so its data is getting older by the minute and failing over to it now would lose changes.
impact: stopped
fix: Find why it stopped (the replica's error log, network, disk space), fix it, and let it catch up before relying on it.
verify: Re-scan: the lag should be seconds.

id: replica-lag:sub_orders
title: The logical consumer sub_orders isn't receiving changes (stopped)
summary: The copy no longer keeps up, so its data is getting older by the minute and failing over to it now would lose changes.
impact: stopped
fix: Find why it stopped (the replica's error log, network, disk space), fix it, and let it catch up before relying on it.
verify: Re-scan: the lag should be seconds."#,
    );
}

#[test]
fn replicas_behind() {
    use culprit_core::{Replica, ReplicaRole::Replica as Sends, ReplicaRole::ThisServer};
    const GB: f64 = 1024.0 * 1024.0 * 1024.0;
    let mut findings = lagging(
        "postgres-demo",
        None,
        vec![
            Replica {
                lag_seconds: Some(900.0),
                lag_bytes: Some(2.0 * GB),
                ..replica("this server", ThisServer, "streaming")
            },
            Replica { lag_seconds: Some(45.0), ..replica("standby-1", Sends, "streaming") },
            Replica { lag_bytes: Some(3.0 * GB), ..replica("standby-2", Sends, "streaming") },
            Replica { logical: Some(true), lag_seconds: Some(120.0), ..replica("debezium", Sends, "streaming") },
        ],
    );
    findings.extend(lagging(
        "mysql-demo",
        None,
        vec![Replica {
            lag_seconds: Some(700.0),
            delay_seconds: Some(3_600.0),
            ..replica("from 10.0.0.5", ThisServer, "replicating")
        }],
    ));
    findings.extend(lagging(
        "postgres-demo",
        None,
        vec![Replica {
            lag_seconds: Some(100.0),
            delay_seconds: Some(600.0),
            ..replica("this server", ThisServer, "streaming")
        }],
    ));
    says(
        &findings,
        r#"id: replica-lag:this server
title: This server is 15 min behind its primary
summary: Reads from the replica return data 15 min old, and failing over now would lose or wait for that much. Lag grows when the replica applies changes slower than the primary writes them: large transactions, long queries on the replica (which can pause replay), or slower storage.
impact: 15 min behind
metric: Behind by
metric: Not yet applied
fix: Split large write transactions into batches, check the replica's storage and CPU, and move long reports off it or let them run against a snapshot.
verify: Re-scan: the lag should be seconds.

id: replica-lag:standby-1
title: The replica standby-1 is 45.0 s behind
summary: Reads from the replica return data 45.0 s old, and failing over now would lose or wait for that much. Lag grows when the replica applies changes slower than the primary writes them: large transactions, long queries on the replica (which can pause replay), or slower storage.
impact: 45.0 s behind
metric: Behind by
fix: Split large write transactions into batches, check the replica's storage and CPU, and move long reports off it or let them run against a snapshot.
verify: Re-scan: the lag should be seconds.

id: replica-lag:standby-2
title: The replica standby-2 is 3.0 GB of log behind
summary: Reads from the replica return data 3.0 GB of log old, and failing over now would lose or wait for that much. Lag grows when the replica applies changes slower than the primary writes them: large transactions, long queries on the replica (which can pause replay), or slower storage.
impact: 3.0 GB of log behind
metric: Not yet applied
fix: Split large write transactions into batches, check the replica's storage and CPU, and move long reports off it or let them run against a snapshot.
verify: Re-scan: the lag should be seconds.

id: replica-lag:debezium
title: The logical consumer debezium is 2 min behind
summary: debezium receives this database's changes through logical decoding: a subscription, or a CDC tool such as Debezium. It has confirmed changes only up to 2 min ago, so whatever it feeds sees data that old, and its slot keeps the WAL since then on this server. A consumer falls behind when it applies changes slower than this server writes them: one very large transaction, a slow destination, or a connector that is throttled or failing.
impact: 2 min behind
metric: Behind by
fix: Look at the consumer, not this server: the subscriber's apply worker (pg_stat_subscription and its log) or the CDC connector's lag and errors. Split very large transactions so changes flow in smaller pieces.
verify: Re-scan: the lag should be seconds.

id: replica-lag:from 10.0.0.5
title: This server is 12 min behind its primary
summary: Reads from the replica return data 12 min old, and failing over now would lose or wait for that much. Lag grows when the replica applies changes slower than the primary writes them: large transactions, long queries on the replica, or slower storage. It is also set up to apply changes 1 h late on purpose (SOURCE_DELAY); that part isn't counted.
impact: 12 min behind
metric: Behind by
metric: Delayed on purpose by
fix: Split large write transactions into batches, check the replica's storage and CPU, and move long reports off it or let them run against a snapshot.
verify: Re-scan: the lag should be seconds.

id: replica-lag:this server
title: This server is 2 min behind its primary
summary: Reads from the replica return data 2 min old, and failing over now would lose or wait for that much. Lag grows when the replica applies changes slower than the primary writes them: large transactions, long queries on the replica (which can pause replay), or slower storage. It is also set up to apply changes 10 min late on purpose (recovery_min_apply_delay); that part isn't counted.
impact: 2 min behind
metric: Behind by
metric: Delayed on purpose by
fix: Split large write transactions into batches, check the replica's storage and CPU, and move long reports off it or let them run against a snapshot.
verify: Re-scan: the lag should be seconds."#,
    );
}

#[test]
fn inactive_replication_slots() {
    use culprit_core::{Replica, ReplicaRole::Slot};
    const GB: f64 = 1024.0 * 1024.0 * 1024.0;
    let slot = |name: &str, logical: bool, gb: f64| Replica {
        logical: Some(logical),
        lag_bytes: Some(gb * GB),
        ..replica(name, Slot, "inactive")
    };
    let mut findings = lagging(
        "postgres-demo",
        None,
        vec![
            Replica {
                inactive_seconds: Some(86_400.0),
                wal_status: Some("extended".into()),
                ..slot("old_standby", false, 12.0)
            },
            Replica {
                wal_status: Some("unreserved".into()),
                safe_wal_bytes: Some(2.0 * GB),
                ..slot("debezium_slot", true, 3.0)
            },
            Replica {
                wal_status: Some("reserved".into()),
                safe_wal_bytes: Some(5.0 * GB),
                ..slot("airbyte", true, 2.0)
            },
            slot("unknown_cap", false, 2.0),
            Replica { inactive_seconds: Some(3_600.0), ..slot("logical_unknown", true, 2.0) },
        ],
    );
    findings.extend(lagging("postgres-demo", Some("12.18"), vec![slot("old_server_slot", false, 2.0)]));
    says(
        &findings,
        r#"id: replica-lag:old_standby
title: The inactive replication slot old_standby keeps 12.0 GB of WAL
summary: A replication slot keeps every WAL file its consumer hasn't confirmed. old_standby has had no consumer for 24 h, so the WAL it holds keeps growing. It is a physical slot: a standby or a backup tool streams WAL through it. Nothing caps what it keeps (max_slot_wal_keep_size is -1), so it grows until the disk fills and the server stops accepting writes.
impact: 12.0 GB of WAL kept
metric: WAL kept
metric: Without a consumer for
fix: If nothing will stream through this slot again (a standby that was removed), drop it. To keep any slot from filling the disk, cap what slots may keep: Set max_slot_wal_keep_size to 50GB and reload the configuration (settings marked as needing a restart take effect after one).
sql: SELECT pg_drop_replication_slot('old_standby');
-- Cap what any slot may keep:
ALTER SYSTEM SET max_slot_wal_keep_size = '50GB';
SELECT pg_reload_conf();
verify: Re-scan: the slot should be gone, or active again.

id: replica-lag:debezium_slot
title: The logical slot debezium_slot keeps 3.0 GB of WAL while its consumer is away
summary: A replication slot keeps every WAL file its consumer hasn't confirmed. debezium_slot has no consumer connected, so the WAL it holds keeps growing. It is a logical slot: a subscription or a CDC tool (Debezium, Fivetran, Airbyte…) reads changes through it, and many connect only between syncs. Dropping it makes that consumer copy everything again. max_slot_wal_keep_size caps what it can keep: 2.0 GB more WAL and PostgreSQL gives the slot up, so the disk is safe, but its consumer then has to start over.
impact: 3.0 GB of WAL kept
metric: WAL kept
metric: WAL left before it is given up
fix: Find its consumer first: the slot's name and plugin usually say which subscription or connector it is. If it still syncs, leave the slot: it catches up when the consumer connects. If the consumer is gone for good, drop it.
sql: -- Only once its consumer is gone for good: dropping it makes the consumer copy everything again
-- SELECT pg_drop_replication_slot('debezium_slot');
verify: Re-scan once the consumer has synced: the WAL kept should drop, or the slot be gone.

id: replica-lag:airbyte
title: The logical slot airbyte keeps 2.0 GB of WAL while its consumer is away
summary: A replication slot keeps every WAL file its consumer hasn't confirmed. airbyte has no consumer connected, so the WAL it holds keeps growing. It is a logical slot: a subscription or a CDC tool (Debezium, Fivetran, Airbyte…) reads changes through it, and many connect only between syncs. Dropping it makes that consumer copy everything again. max_slot_wal_keep_size caps what it can keep: 5.0 GB more WAL and PostgreSQL gives the slot up, so the disk is safe, but its consumer then has to start over.
impact: 2.0 GB of WAL kept
metric: WAL kept
metric: WAL left before it is given up
fix: Find its consumer first: the slot's name and plugin usually say which subscription or connector it is. If it still syncs, leave the slot: it catches up when the consumer connects. If the consumer is gone for good, drop it.
sql: -- Only once its consumer is gone for good: dropping it makes the consumer copy everything again
-- SELECT pg_drop_replication_slot('airbyte');
verify: Re-scan once the consumer has synced: the WAL kept should drop, or the slot be gone.

id: replica-lag:unknown_cap
title: The inactive replication slot unknown_cap keeps 2.0 GB of WAL
summary: A replication slot keeps every WAL file its consumer hasn't confirmed. unknown_cap has no consumer connected, so the WAL it holds keeps growing. It is a physical slot: a standby or a backup tool streams WAL through it. Unless max_slot_wal_keep_size caps what slots keep, it grows until the disk fills and the server stops accepting writes.
impact: 2.0 GB of WAL kept
metric: WAL kept
fix: If nothing will stream through this slot again (a standby that was removed), drop it. If max_slot_wal_keep_size isn't set, cap what slots may keep so none can fill the disk: Set max_slot_wal_keep_size to 50GB and reload the configuration (settings marked as needing a restart take effect after one).
sql: SELECT pg_drop_replication_slot('unknown_cap');
-- If max_slot_wal_keep_size isn't set, cap what any slot may keep:
ALTER SYSTEM SET max_slot_wal_keep_size = '50GB';
SELECT pg_reload_conf();
verify: Re-scan: the slot should be gone, or active again.

id: replica-lag:logical_unknown
title: The logical slot logical_unknown keeps 2.0 GB of WAL while its consumer is away
summary: A replication slot keeps every WAL file its consumer hasn't confirmed. logical_unknown has had no consumer for 1 h, so the WAL it holds keeps growing. It is a logical slot: a subscription or a CDC tool (Debezium, Fivetran, Airbyte…) reads changes through it, and many connect only between syncs. Dropping it makes that consumer copy everything again. Unless max_slot_wal_keep_size caps what slots keep, it grows until the disk fills and the server stops accepting writes.
impact: 2.0 GB of WAL kept
metric: WAL kept
metric: Without a consumer for
fix: Find its consumer first: the slot's name and plugin usually say which subscription or connector it is. If it still syncs, leave the slot: it catches up when the consumer connects. If the consumer is gone for good, drop it. If max_slot_wal_keep_size isn't set, cap what slots may keep so none can fill the disk: Set max_slot_wal_keep_size to 50GB and reload the configuration (settings marked as needing a restart take effect after one).
sql: -- Only once its consumer is gone for good: dropping it makes the consumer copy everything again
-- SELECT pg_drop_replication_slot('logical_unknown');
-- If max_slot_wal_keep_size isn't set, cap what any slot may keep:
ALTER SYSTEM SET max_slot_wal_keep_size = '50GB';
SELECT pg_reload_conf();
verify: Re-scan once the consumer has synced: the WAL kept should drop, or the slot be gone.

id: replica-lag:old_server_slot
title: The inactive replication slot old_server_slot keeps 2.0 GB of WAL
summary: A replication slot keeps every WAL file its consumer hasn't confirmed. old_server_slot has no consumer connected, so the WAL it holds keeps growing. It is a physical slot: a standby or a backup tool streams WAL through it. Before PostgreSQL 13 nothing can cap it: it grows until the disk fills and the server stops accepting writes.
impact: 2.0 GB of WAL kept
metric: WAL kept
fix: If nothing will stream through this slot again (a standby that was removed), drop it. PostgreSQL 13 and later can cap what slots keep (max_slot_wal_keep_size).
sql: SELECT pg_drop_replication_slot('old_server_slot');
verify: Re-scan: the slot should be gone, or active again."#,
    );
}

// ── large-rows ─────────────────────────────────────────────────────────────────────────────────────

const MB: f64 = 1024.0 * 1024.0;

/// `fixture` with `table`'s rows made 2.9 GB of large values in 9,728 rows, its columns `columns`, and no statements.
fn large(name: &str, table: &str, columns: &[(&str, &str)]) -> Snapshot {
    let mut s = fixture(name);
    s.taken_at = "2026-10-13T09:00:00Z".into();
    let t = s.tables.iter_mut().find(|t| t.name == table).expect("a table");
    (t.rows, t.in_row_bytes, t.lob_bytes, t.overflow_bytes, t.data_bytes) =
        (Some(9_728.0), Some(31.0 * MB), Some(2_900.0 * MB), Some(0.0), Some(2_931.0 * MB));
    t.dead_rows = None;
    let (schema, table) = (t.schema.clone(), t.name.clone());
    s.columns.retain(|c| c.table != table);
    for (ordinal, (name, data_type)) in columns.iter().enumerate() {
        s.columns.push(culprit_core::ColumnInfo {
            schema: schema.clone(),
            table: table.clone(),
            name: (*name).into(),
            data_type: (*data_type).into(),
            nullable: true,
            default: None,
            generated: false,
            ordinal: 100 + ordinal as u32,
        });
    }
    let w = s.workload.as_mut().expect("workload");
    w.window_hours = 24.0;
    let mut template = w.queries[0].clone();
    (template.plan, template.costs, template.first_seen, template.last_seen) = (None, None, None, None);
    (template.object_name, template.resolved_write_target, template.query_hash) = (None, None, None);
    template.waits.clear();
    w.queries = vec![template];
    s
}

/// A statement on `s` like its template: `runs` runs returning `rows` rows a run, reading `reads` pages a run.
fn statement(s: &mut Snapshot, id: &str, text: &str, runs: f64, rows: f64, reads: f64) {
    let w = s.workload.as_mut().expect("workload");
    let mut q = w.queries[0].clone();
    (q.id, q.text, q.executions, q.rows, q.logical_reads) =
        (id.into(), text.into(), runs, Some(rows * runs), Some(reads * runs));
    w.queries.push(q);
}

fn grown_since(s: &mut Snapshot, key: &str, taken_at: &str, mb: f64) {
    let tables = std::collections::BTreeMap::from([(key.to_string(), mb * MB)]);
    s.sizes_before = Some(culprit_core::SizeBaseline { taken_at: taken_at.into(), tables, reference: None });
}

#[test]
fn large_rows_read_whole_and_growing_on_sql_server() {
    let columns = [("id", "int"), ("body", "nvarchar(max)"), ("attachments", "varbinary(max)")];
    let mut s = large("mssql-demo", "support_tickets", &columns);
    statement(&mut s, "201", "SELECT * FROM shop.support_tickets WHERE id = @P1", 12_700.0, 1.0, 280.0);
    grown_since(&mut s, "shop.support_tickets", "2026-10-12T09:00:00Z", 2_331.0);
    let mut findings = run("large-rows", &s);
    let wide: Vec<(String, String)> = (0..8).map(|i| (format!("note_{i}"), "nvarchar(max)".to_string())).collect();
    let wide: Vec<(&str, &str)> = wide.iter().map(|(n, t)| (n.as_str(), t.as_str())).collect();
    let mut s = large("mssql-demo", "support_tickets", &wide);
    statement(&mut s, "201", "SELECT * FROM shop.support_tickets WHERE id = @P1", 300.0, 1.0, 280.0);
    statement(&mut s, "202", "UPDATE shop.support_tickets SET body = @P1 WHERE id = @P2", 200.0, 1.0, 300.0);
    grown_since(&mut s, "shop.support_tickets", "2026-10-13T03:00:00Z", 2_331.0);
    findings.extend(run("large-rows", &s));
    let mut s = large("mssql-demo", "support_tickets", &[("id", "int")]);
    grown_since(&mut s, "shop.support_tickets", "2026-10-12T09:00:00Z", 3_500.0);
    findings.extend(run("large-rows", &s));
    says(
        &findings,
        r#"id: large-rows:shop.support_tickets
title: support_tickets holds 2.9 GB in 9.7K rows: 309 KB a row, mostly large values
summary: The 9.7K rows of support_tickets take 2.9 GB: 309 KB each on average, and almost all of that is large values kept apart from the rows (columns that can hold them: body and attachments). Every statement that reads whole rows pulls them in, every insert or update that sets them writes them again, and they keep their space until the rows are deleted and purged. The statement below reads them about 13K times a day, going through about 280 pages (2.2 MB) a run. It grew by 600 MB since the scan on Oct 12 at 09:00 UTC, about 600 MB a day.
impact: 2.9 GB, 309 KB a row
metric: Table data
metric: Rows
metric: Average row
metric: In large-value pages
metric: Whole-row statements a day
metric: Growth a day
fix: Read only the columns a statement needs: a SELECT * or an ORM loading whole rows pulls every large value. Keep a value that many rows repeat in one place and point to it. Clear values a row no longer needs once its work is done. Check how long deleted rows are kept before they're purged: until then they keep their values. For values that must stay, COMPRESS() them as they're written (GZip; DECOMPRESS() reads them back), or keep them in object storage and store a reference.
verify: If you read fewer columns, measure how much data the application receives for the same rows before and after. Compare the changed statements' time per run in windows before and after the change, for the same rows and representative values. A rewritten statement has a new entry. Reading fewer columns need not change the table's stored size or the number of in-row pages read.
verify: If you make stored values smaller, compare bytes per live row in later scans; if you remove rows or values, compare total large-value bytes and subsequent growth. Catalog page counts can lag until freed space is reclaimed, and the percentage in large values need not fall.
caveat: Sizes are the pages the table has reserved, from the catalog: they include space allocated but not yet filled.

id: large-rows:shop.support_tickets
title: support_tickets holds 2.9 GB in 9.7K rows: 309 KB a row, mostly large values
summary: The 9.7K rows of support_tickets take 2.9 GB: 309 KB each on average, and almost all of that is large values kept apart from the rows (columns that can hold them: note_0, note_1, note_2, note_3, note_4, note_5 and 2 more). Every statement that reads whole rows pulls them in, every insert or update that sets them writes them again, and they keep their space until the rows are deleted and purged. The 2 statements below read or write them about 500 times a day, going through about 288 pages (2.2 MB) a run. It grew by 600 MB in the 6 hours since the scan on Oct 13 at 03:00 UTC.
impact: 2.9 GB, 309 KB a row
metric: Table data
metric: Rows
metric: Average row
metric: In large-value pages
metric: Whole-row statements a day
fix: Read only the columns a statement needs: a SELECT * or an ORM loading whole rows pulls every large value. Keep a value that many rows repeat in one place and point to it. Clear values a row no longer needs once its work is done. Check how long deleted rows are kept before they're purged: until then they keep their values. For values that must stay, COMPRESS() them as they're written (GZip; DECOMPRESS() reads them back), or keep them in object storage and store a reference.
verify: If you read fewer columns, measure how much data the application receives for the same rows before and after. Compare the changed statements' time per run in windows before and after the change, for the same rows and representative values. A rewritten statement has a new entry. Reading fewer columns need not change the table's stored size or the number of in-row pages read.
verify: If you make stored values smaller, compare bytes per live row in later scans; if you remove rows or values, compare total large-value bytes and subsequent growth. Catalog page counts can lag until freed space is reclaimed, and the percentage in large values need not fall.
caveat: Sizes are the pages the table has reserved, from the catalog: they include space allocated but not yet filled.

id: large-rows:shop.support_tickets
title: support_tickets holds 2.9 GB in 9.7K rows: 309 KB a row, mostly large values
summary: The 9.7K rows of support_tickets take 2.9 GB: 309 KB each on average, and almost all of that is large values kept apart from the rows. Every statement that reads whole rows pulls them in, every insert or update that sets them writes them again, and they keep their space until the rows are deleted and purged. It's 569 MB smaller than at the scan on Oct 12 at 09:00 UTC.
impact: 2.9 GB, 309 KB a row
metric: Table data
metric: Rows
metric: Average row
metric: In large-value pages
fix: Read only the columns a statement needs: a SELECT * or an ORM loading whole rows pulls every large value. Keep a value that many rows repeat in one place and point to it. Clear values a row no longer needs once its work is done. Check how long deleted rows are kept before they're purged: until then they keep their values. For values that must stay, COMPRESS() them as they're written (GZip; DECOMPRESS() reads them back), or keep them in object storage and store a reference.
verify: If you read fewer columns, measure how much data the application receives for the same rows before and after. Record representative runs of the changed statement in the workload, then compare its time per run. A rewritten statement has a new entry. Reading fewer columns need not change the table's stored size or the number of in-row pages read.
verify: If you make stored values smaller, compare bytes per live row in later scans; if you remove rows or values, compare total large-value bytes and subsequent growth. Catalog page counts can lag until freed space is reclaimed, and the percentage in large values need not fall.
caveat: Sizes are the pages the table has reserved, from the catalog: they include space allocated but not yet filled."#,
    );
}

#[test]
fn large_rows_on_postgres_with_dead_versions() {
    let mut s = large("postgres-demo", "events", &[("id", "bigint"), ("body", "text")]);
    s.tables.iter_mut().find(|t| t.name == "events").unwrap().dead_rows = Some(4_000.0);
    grown_since(&mut s, "public.events", "2026-10-12T09:00:00Z", 2_931.0);
    says(
        &run("large-rows", &s),
        r#"id: large-rows:public.events
title: events holds 2.9 GB in 9.7K rows: 309 KB a row, mostly large values
summary: The 9.7K rows of events take 2.9 GB: 309 KB each on average, and almost all of that is large values kept apart from the rows (the column that can hold them: body). Every statement that reads whole rows pulls them in, every insert or update that sets them writes them again, and they keep their space until the rows are deleted and purged. PostgreSQL has already compressed them: these are the sizes on disk. 29% of its row versions are dead rows not yet vacuumed, and their large values still take space until VACUUM clears them. It hasn't grown since the scan on Oct 12 at 09:00 UTC.
impact: 2.9 GB, 309 KB a row
metric: Table data
metric: Rows
metric: Average row
metric: In large-value pages
fix: Read only the columns a statement needs: a SELECT * or an ORM loading whole rows pulls every large value. Keep a value that many rows repeat in one place and point to it. Clear values a row no longer needs once its work is done. Check how long deleted rows are kept before they're purged: until then they keep their values. For values that must stay, LZ4 compresses them faster than the default (ALTER TABLE … ALTER COLUMN … SET COMPRESSION lz4, PostgreSQL 14+), or keep them in object storage and store a reference.
verify: If you read fewer columns, measure how much data the application receives for the same rows before and after. Record representative runs of the changed statement in the workload, then compare its time per run. A rewritten statement has a new entry. Reading fewer columns need not change the table's stored size or the number of in-row pages read.
verify: If you make stored values smaller, compare bytes per live row in later scans; if you remove rows or values, compare total large-value bytes and subsequent growth. Catalog page counts can lag until freed space is reclaimed, and the percentage in large values need not fall.
caveat: Sizes are the pages the table has reserved, from the catalog: they include space allocated but not yet filled."#,
    );
}

#[test]
fn json_columns_on_large_postgres_tables() {
    let mut s = fixture("postgres-demo");
    for (table, rows, columns) in
        [("orders", 200_000.0, vec!["payload"]), ("customers", 150_000.0, vec!["prefs", "tags"])]
    {
        s.tables.iter_mut().find(|t| t.name == table).unwrap().rows = Some(rows);
        for (i, column) in columns.into_iter().enumerate() {
            s.columns.push(culprit_core::ColumnInfo {
                schema: "public".into(),
                table: table.into(),
                name: column.into(),
                data_type: "json".into(),
                nullable: true,
                default: None,
                generated: false,
                ordinal: 300 + i as u32,
            });
        }
    }
    says(
        &run("large-rows", &s),
        r#"id: large-rows:json-column:public.orders.payload
title: orders.payload uses legacy json instead of jsonb (200K rows)
summary: public.orders (200K rows) declares `payload` as `json` rather than `jsonb`. PostgreSQL's `json` type stores raw unparsed text, re-parses the document on every field access, preserves duplicate keys and whitespace, and supports neither GIN indexing nor direct equality/containment operators (`@>`, `=`).
impact: json -> jsonb
metric: Rows in table
metric: Column = orders.payload (json)
fix: Convert `payload` on public.orders from `json` to `jsonb` during a maintenance window if exact whitespace/key-order preservation is not required.
cost: Changing a column from json to jsonb rewrites the table while holding an ACCESS EXCLUSIVE lock.
sql: ALTER TABLE public.orders ALTER COLUMN payload TYPE jsonb USING payload::jsonb;
verify: Re-scan after converting public.orders.payload to `jsonb`: the column type is `jsonb` and this finding is gone.

id: large-rows:json-column:public.customers.prefs,tags
title: customers.prefs, tags uses legacy json instead of jsonb (150K rows)
summary: public.customers (150K rows) declares `prefs`, `tags` as `json` rather than `jsonb`. PostgreSQL's `json` type stores raw unparsed text, re-parses the document on every field access, preserves duplicate keys and whitespace, and supports neither GIN indexing nor direct equality/containment operators (`@>`, `=`).
impact: json -> jsonb
metric: Rows in table
metric: Column = customers.prefs (json)
fix: Convert `prefs`, `tags` on public.customers from `json` to `jsonb` during a maintenance window if exact whitespace/key-order preservation is not required.
cost: Changing a column from json to jsonb rewrites the table while holding an ACCESS EXCLUSIVE lock.
sql: ALTER TABLE public.customers ALTER COLUMN prefs TYPE jsonb USING prefs::jsonb;
verify: Re-scan after converting public.customers.prefs to `jsonb`: the column type is `jsonb` and this finding is gone."#,
    );
}

// ── log-volume ─────────────────────────────────────────────────────────────────────────────────────

fn logging(name: &str, text: &str, rows: Option<f64>, costs: culprit_core::QueryCosts) -> Snapshot {
    let mut s = fixture(name);
    let q = only(&mut s, text);
    q.rows = rows;
    q.costs = Some(costs);
    s
}

#[test]
fn wal_per_row_with_full_page_images() {
    use culprit_core::QueryCosts;
    let costs = || QueryCosts {
        log_bytes: Some(200.0 * MB),
        log_share: Some(0.2),
        log_full_pages: Some(20_000.0),
        ..Default::default()
    };
    let update = "UPDATE events SET note = $1 WHERE id = $2";
    let mut findings = run("log-volume", &logging("postgres-demo", update, Some(20_000.0), costs()));
    for compression in ["off", "lz4"] {
        let mut s = logging("postgres-demo", update, Some(20_000.0), costs());
        set(&mut s, "wal_compression", compression);
        findings.extend(run("log-volume", &s));
    }
    says(
        &findings,
        r#"id: log-volume:9065391607317686058
title: An update of events writes 10.2 KB of WAL per row it changes
summary: It wrote 200 MB of WAL since its statistics began 5 hours ago: 20% of what statements wrote, about 10.2 KB per row it changed. Every byte of WAL is also written to backups and sent to replicas, so it drives replication lag, backup size and checkpoints. 20K of its records are full-page images: the first change to each page after a checkpoint logs the whole page.
impact: 200 MB of WAL
metric: Log written
metric: Share of log
metric: Per row
fix: Touch each page once per checkpoint: batch the changes (one statement for many rows, sorted by key) rather than many scattered single-row writes. If full-page images dominate, a longer checkpoint interval (checkpoint_timeout, max_wal_size) logs fewer of them, and wal_compression shrinks them if it is off.
verify: Between two scans, the WAL it writes per row should drop: pg_stat_statements keeps running totals, so compare its WAL and the rows it changed in a scan taken once the change is live and one a day or more later (200 MB for 20K rows in this scan).

id: log-volume:9065391607317686058
title: An update of events writes 10.2 KB of WAL per row it changes
summary: It wrote 200 MB of WAL since its statistics began 5 hours ago: 20% of what statements wrote, about 10.2 KB per row it changed. Every byte of WAL is also written to backups and sent to replicas, so it drives replication lag, backup size and checkpoints. Up to 78% of it is full-page images: the first change to each page after a checkpoint logs the whole page, 8 KB at most.
impact: 200 MB of WAL
metric: Log written
metric: Share of log
metric: Per row
fix: Touch each page once per checkpoint: batch the changes (one statement for many rows, sorted by key) rather than many scattered single-row writes. Full-page images dominate and wal_compression is off: turning it on shrinks them (lz4 or zstd on PostgreSQL 15+). Set wal_compression to on and reload the configuration (settings marked as needing a restart take effect after one).
sql: ALTER SYSTEM SET wal_compression = 'on';
SELECT pg_reload_conf();
verify: Between two scans, the WAL it writes per row should drop: pg_stat_statements keeps running totals, so compare its WAL and the rows it changed in a scan taken once the change is live and one a day or more later (200 MB for 20K rows in this scan).

id: log-volume:9065391607317686058
title: An update of events writes 10.2 KB of WAL per row it changes
summary: It wrote 200 MB of WAL since its statistics began 5 hours ago: 20% of what statements wrote, about 10.2 KB per row it changed. Every byte of WAL is also written to backups and sent to replicas, so it drives replication lag, backup size and checkpoints. 20K of its records are full-page images: the first change to each page after a checkpoint logs the whole page, compressed with wal_compression.
impact: 200 MB of WAL
metric: Log written
metric: Share of log
metric: Per row
fix: Touch each page once per checkpoint: batch the changes (one statement for many rows, sorted by key) rather than many scattered single-row writes. wal_compression already shrinks full-page images; a longer checkpoint interval (checkpoint_timeout, max_wal_size) logs fewer of them.
verify: Between two scans, the WAL it writes per row should drop: pg_stat_statements keeps running totals, so compare its WAL and the rows it changed in a scan taken once the change is live and one a day or more later (200 MB for 20K rows in this scan)."#,
    );
}

#[test]
fn a_share_of_the_wal_on_a_table_with_an_unread_index() {
    use culprit_core::QueryCosts;
    let costs = QueryCosts { log_bytes: Some(2_048.0 * MB), log_share: Some(0.6), ..Default::default() };
    let mut s = logging("postgres-demo", "UPDATE events SET note = $1 WHERE id = $2", None, costs);
    s.pressure.get_or_insert_with(Default::default).uptime_hours = Some(240.0);
    let mut idle = s.indexes.iter().find(|i| i.table == "events").expect("an index on events").clone();
    idle.name = "events_idle_idx".into();
    idle.columns = vec!["kind".into()];
    (idle.is_primary_key, idle.is_unique, idle.is_clustered) = (false, false, false);
    idle.reads = Some(0.0);
    idle.size_bytes = Some(8.0 * MB);
    s.indexes.push(idle);
    says(
        &run("log-volume", &s),
        r#"id: log-volume:9065391607317686058
title: An update of events writes 60% of the WAL statements write
summary: It wrote 2.0 GB of WAL since its statistics began 5 hours ago: 60% of what statements wrote. Every byte of WAL is also written to backups and sent to replicas, so it drives replication lag, backup size and checkpoints.
impact: 2.0 GB of WAL
metric: Log written
metric: Share of log
fix: Touch each page once per checkpoint: batch the changes (one statement for many rows, sorted by key) rather than many scattered single-row writes. Every index on the table adds its own WAL, and events has one nobody reads: drop it (see Indexes). If full-page images dominate, a longer checkpoint interval (checkpoint_timeout, max_wal_size) logs fewer of them, and wal_compression shrinks them if it is off.
verify: Between two scans, the WAL it writes per row should drop: pg_stat_statements keeps running totals, so compare its WAL and the rows it changed in a scan taken once the change is live and one a day or more later (2.0 GB for 0 rows in this scan)."#,
    );
}

#[test]
fn wal_of_a_writer_that_stopped() {
    use culprit_core::QueryCosts;
    let costs = QueryCosts { log_bytes: Some(2_048.0 * MB), log_share: Some(0.6), ..Default::default() };
    let mut s = logging("postgres-demo", "UPDATE events SET note = $1 WHERE id = $2", Some(20_000.0), costs);
    stopped(&mut s);
    says(
        &run("log-volume", &s),
        r#"id: log-volume:9065391607317686058
title: An update of events wrote 105 KB of WAL per row it changed
summary: It wrote 2.0 GB of WAL in the last 3 days: 60% of what statements wrote, about 105 KB per row it changed. Every byte of WAL is also written to backups and sent to replicas, so it drives replication lag, backup size and checkpoints.
impact: 2.0 GB of WAL
metric: Log written
metric: Share of log
metric: Per row
fix: Touch each page once per checkpoint: batch the changes (one statement for many rows, sorted by key) rather than many scattered single-row writes. If full-page images dominate, a longer checkpoint interval (checkpoint_timeout, max_wal_size) logs fewer of them, and wal_compression shrinks them if it is off.
verify: Re-scan: its share of the WAL and the bytes per row should drop."#,
    );
}

#[test]
fn transaction_log_on_sql_server() {
    use culprit_core::QueryCosts;
    let costs = || QueryCosts { log_bytes: Some(2_048.0 * MB), log_share: Some(0.6), ..Default::default() };
    let mut findings = run(
        "log-volume",
        &logging("mssql-demo", "INSERT INTO shop.orders (customer_id) VALUES (@P1)", Some(100_000.0), costs()),
    );
    findings.extend(run(
        "log-volume",
        &logging("mssql-demo", "UPDATE shop.orders SET status = @P1 WHERE id = @P2", Some(100_000.0), costs()),
    ));
    let columns = [("id", "int"), ("body", "nvarchar(max)")];
    let mut s = large("mssql-demo", "support_tickets", &columns);
    statement(&mut s, "301", "INSERT INTO shop.support_tickets (body) VALUES (@P1)", 2_000.0, 1.0, 10.0);
    s.workload.as_mut().unwrap().queries.retain(|q| q.id == "301");
    let q = &mut s.workload.as_mut().unwrap().queries[0];
    q.costs =
        Some(QueryCosts { log_bytes: Some(2_000.0 * 300.0 * 1024.0), log_share: Some(0.9), ..Default::default() });
    findings.extend(run("log-volume", &s));
    says(
        &findings,
        r#"id: log-volume:7
title: An insert into orders writes 21.0 KB of transaction log per row it changes
summary: It wrote 2.0 GB of transaction log in the last 5 hours: 60% of what statements wrote, about 21.0 KB per row it changed. Every byte of transaction log is also written to backups and sent to replicas, so it drives replication lag, backup size and checkpoints.
impact: 2.0 GB of transaction log
metric: Log written
metric: Share of log
metric: Per row
fix: An insert logs every row it adds in full, with an entry in each index: this much transaction log is this much data. Store less per row (large values compressed as they're written, or kept in object storage with a reference in the row), and commit many rows per transaction rather than one.
verify: Re-scan: its share of the transaction log and the bytes per row should drop.

id: log-volume:7
title: An update of orders writes 21.0 KB of transaction log per row it changes
summary: It wrote 2.0 GB of transaction log in the last 5 hours: 60% of what statements wrote, about 21.0 KB per row it changed. Every byte of transaction log is also written to backups and sent to replicas, so it drives replication lag, backup size and checkpoints.
impact: 2.0 GB of transaction log
metric: Log written
metric: Share of log
metric: Per row
fix: Log this heavy usually means wide rows rewritten whole, many indexes updated, or one transaction per row. Batch writes into transactions of about a thousand rows, and update only the columns that change.
verify: Re-scan: its share of the transaction log and the bytes per row should drop.

id: log-volume:301
title: An insert into support_tickets writes 300 KB of transaction log per row it changes
summary: It wrote 586 MB of transaction log in the last 24 hours: 90% of what statements wrote, about 300 KB per row it changed. Every byte of transaction log is also written to backups and sent to replicas, so it drives replication lag, backup size and checkpoints.
impact: 586 MB of transaction log
metric: Log written
metric: Share of log
metric: Per row
fix: The rows of support_tickets average 309 KB, almost all of it large values, and every insert or update that sets them writes them to the transaction log in full: batching the writes won't shrink that, smaller or fewer values will. For values that must stay, COMPRESS() them as they're written (GZip; DECOMPRESS() reads them back), or keep them in object storage and store a reference.
verify: Re-scan: its share of the transaction log and the bytes per row should drop."#,
    );
}

// ── table-bloat ────────────────────────────────────────────────────────────────────────────────────

fn storage(options: &[&str]) -> Option<culprit_core::TableStorage> {
    Some(culprit_core::TableStorage { options: options.iter().map(|o| o.to_string()).collect(), ..Default::default() })
}

/// A copy of `s`'s first table named `name`, a partition of `parent` bounded by `bound`.
fn partition(s: &Snapshot, name: &str, parent: &str, bound: &str) -> culprit_core::TableStat {
    let mut part = s.tables[0].clone();
    part.name = name.into();
    part.partition_of = Some(parent.into());
    part.storage = storage(&[&format!("partition_bound={bound}")]);
    part
}

#[test]
fn bloated_tables() {
    const GB: f64 = 1024.0 * MB;
    let mut s = fixture("postgres-demo");
    let events = s.tables.iter_mut().find(|t| t.name == "events").unwrap();
    (events.data_bytes, events.dead_rows) = (Some(3.0 * GB), None);
    events.storage = Some(culprit_core::TableStorage { bloat_bytes: Some(1.5 * GB), ..Default::default() });
    let orders = s.tables.iter_mut().find(|t| t.name == "orders").unwrap();
    (orders.data_bytes, orders.rows, orders.dead_rows) = (Some(400.0 * MB), Some(1_000.0), Some(1_000.0));
    orders.storage = Some(culprit_core::TableStorage { bloat_bytes: Some(200.0 * MB), ..Default::default() });
    says(
        &run("table-bloat", &s),
        r#"id: table-bloat:public.events
title: events is about 50% empty space (≈ 1.5 GB)
summary: From its row count and average row width, events's rows need about 1.5 GB; it takes 3.0 GB. New rows reuse freed space, but every scan still reads the empty pages, and the cache holds them. This is an estimate from statistics, good to tens of percent; pgstattuple measures it exactly.
impact: ≈ 1.5 GB empty
metric: Empty space (estimated)
metric: Table size
metric: Share
fix: Measure it exactly first. To give the space back, rewrite the table: pg_repack does it online; VACUUM FULL does it while blocking all reads and writes. If the table keeps growing, make autovacuum visit it more often so freed space is reused sooner.
cost: Rewrites the table
sql: -- Exact numbers (needs the pgstattuple extension)
SELECT * FROM pgstattuple('public.events');
-- Rewrite offline: blocks reads and writes until it finishes
-- VACUUM (FULL, ANALYZE) public.events;
verify: Re-scan after the rewrite: the estimate should fall under 20%.

id: table-bloat:public.orders
title: orders is about 50% empty space (≈ 200 MB)
summary: From its row count and average row width, orders's rows need about 200 MB; it takes 400 MB. New rows reuse freed space, but every scan still reads the empty pages, and the cache holds them. This is an estimate from statistics, good to tens of percent; pgstattuple measures it exactly. 50% of its row versions are dead rows VACUUM hasn't cleared yet, enough to be most of this space: vacuum it first and estimate again before rewriting it.
impact: ≈ 200 MB empty
metric: Empty space (estimated)
metric: Table size
metric: Share
fix: Vacuum it: VACUUM clears the dead rows without blocking reads or writes, and new rows then reuse their space. Make autovacuum visit it sooner so they don't pile up again. Rewrite the table (pg_repack online, VACUUM FULL offline) only if the estimate stays high after a vacuum.
cost: VACUUM doesn't block reads or writes
sql: VACUUM (ANALYZE) public.orders;
verify: Re-scan after the rewrite: the estimate should fall under 20%."#,
    );
}

#[test]
fn default_partitions_that_hold_rows() {
    const GB: f64 = 1024.0 * MB;
    let mut s = fixture("postgres-demo");
    for (name, rows, bytes) in [("events_default", 250_000.0, 120.0 * MB), ("orders_default", 2_000_000.0, 2.0 * GB)] {
        let mut part = partition(&s, name, &format!("public.{}", name.trim_end_matches("_default")), "DEFAULT");
        (part.rows, part.data_bytes, part.total_bytes) = (Some(rows), Some(bytes), None);
        s.tables.push(part);
    }
    let mut empty_size = partition(&s, "items_default", "public.order_items", "DEFAULT");
    (empty_size.rows, empty_size.data_bytes, empty_size.total_bytes) = (Some(150_000.0), None, None);
    s.tables.push(empty_size);
    says(
        &run("table-bloat", &s),
        r#"id: table-bloat:default-partition:public.orders_default
title: DEFAULT partition orders_default holds 2.0M rows (2.0 GB)
summary: orders_default is the DEFAULT partition of public.orders, and holds 2.0M rows (2.0 GB). Rows whose partition key has no matching partition land here — often when scheduled partition creation falls behind. In PostgreSQL, adding or attaching a new partition on public.orders scans the DEFAULT partition under an ACCESS EXCLUSIVE lock and fails if any rows in orders_default belong in the new partition's range.
impact: 2.0 GB in DEFAULT
metric: Rows in DEFAULT partition
metric: Size
fix: Create the missing partitions on public.orders, move the matching rows out of orders_default into them (or detach orders_default, split its rows, and re-attach), and check the job that creates future partitions ahead of time.
cost: Moving rows or attaching partitions locks the affected partitions.
verify: Re-scan after moving the rows out: orders_default should be empty or small.

id: table-bloat:default-partition:public.events_default
title: DEFAULT partition events_default holds 250K rows (120 MB)
summary: events_default is the DEFAULT partition of public.events, and holds 250K rows (120 MB). Rows whose partition key has no matching partition land here — often when scheduled partition creation falls behind. In PostgreSQL, adding or attaching a new partition on public.events scans the DEFAULT partition under an ACCESS EXCLUSIVE lock and fails if any rows in events_default belong in the new partition's range.
impact: 120 MB in DEFAULT
metric: Rows in DEFAULT partition
metric: Size
fix: Create the missing partitions on public.events, move the matching rows out of events_default into them (or detach events_default, split its rows, and re-attach), and check the job that creates future partitions ahead of time.
cost: Moving rows or attaching partitions locks the affected partitions.
verify: Re-scan after moving the rows out: events_default should be empty or small.

id: table-bloat:default-partition:public.items_default
title: DEFAULT partition items_default holds 150K rows
summary: items_default is the DEFAULT partition of public.order_items, and holds 150K rows. Rows whose partition key has no matching partition land here — often when scheduled partition creation falls behind. In PostgreSQL, adding or attaching a new partition on public.order_items scans the DEFAULT partition under an ACCESS EXCLUSIVE lock and fails if any rows in items_default belong in the new partition's range.
impact: 150K rows in DEFAULT
metric: Rows in DEFAULT partition
fix: Create the missing partitions on public.order_items, move the matching rows out of items_default into them (or detach items_default, split its rows, and re-attach), and check the job that creates future partitions ahead of time.
cost: Moving rows or attaching partitions locks the affected partitions.
verify: Re-scan after moving the rows out: items_default should be empty or small."#,
    );
}

#[test]
fn range_partitions_about_to_end_or_ended() {
    let mut s = fixture("postgres-demo");
    let parts = [
        partition(&s, "events_2026_09", "public.events", "FOR VALUES FROM ('2026-09-01') TO ('2026-10-01')"),
        partition(&s, "events_2026_10", "public.events", "FOR VALUES FROM ('2026-10-01') TO ('2026-10-15')"),
        partition(&s, "orders_2026_10", "public.orders", "FOR VALUES FROM ('2026-10-01') TO ('2026-10-08')"),
        partition(&s, "items_2026_10", "public.order_items", "FOR VALUES FROM ('2026-10-01') TO ('2026-10-12')"),
    ];
    s.tables.extend(parts);
    says(
        &run("table-bloat", &s),
        r#"id: table-bloat:expiring-partition:public.order_items
title: order_items's last partition (items_2026_10) ends on 2026-10-12 (in 12 h 6 min)
summary: public.order_items has 1 range partition and no DEFAULT or MAXVALUE partition. Its latest partition (items_2026_10) covers values up to '2026-10-12': once timestamps reach '2026-10-12', INSERTs into public.order_items will fail with a partition-routing error.
impact: ends in 12 h 6 min
metric: Partitions
metric: Days until upper bound
fix: Create the next range partition(s) on public.order_items covering values from '2026-10-12' onward, and check the scheduled job or partition manager that pre-creates future partitions.
cost: Creating a new empty partition is fast metadata DDL.
verify: Re-scan after creating future partitions: public.order_items's highest partition bound should be more than 7 days ahead.

id: table-bloat:expiring-partition:public.orders
title: orders's last partition (orders_2026_10) ended on 2026-10-08
summary: public.orders has 1 range partition and no DEFAULT or MAXVALUE partition. Its latest partition (orders_2026_10) covers values up to '2026-10-08': once timestamps reach '2026-10-08', INSERTs into public.orders will fail with a partition-routing error.
impact: ended 2026-10-08
metric: Partitions
metric: Days until upper bound
fix: Create the next range partition(s) on public.orders covering values from '2026-10-08' onward, and check the scheduled job or partition manager that pre-creates future partitions.
cost: Creating a new empty partition is fast metadata DDL.
verify: Re-scan after creating future partitions: public.orders's highest partition bound should be more than 7 days ahead.

id: table-bloat:expiring-partition:public.events
title: events's last partition (events_2026_10) ends on 2026-10-15 (in 3.5 days)
summary: public.events has 2 range partitions and no DEFAULT or MAXVALUE partition. Its latest partition (events_2026_10) covers values up to '2026-10-15': once timestamps reach '2026-10-15', INSERTs into public.events will fail with a partition-routing error.
impact: ends in 3.5 days
metric: Partitions
metric: Days until upper bound
fix: Create the next range partition(s) on public.events covering values from '2026-10-15' onward, and check the scheduled job or partition manager that pre-creates future partitions.
cost: Creating a new empty partition is fast metadata DDL.
verify: Re-scan after creating future partitions: public.events's highest partition bound should be more than 7 days ahead."#,
    );
}

#[test]
fn a_brin_index_for_an_append_only_time_column() {
    const GB: f64 = 1024.0 * MB;
    let mut s = fixture("postgres-demo");
    let t = s.tables.iter_mut().find(|t| t.name == "customers").unwrap();
    (t.rows, t.data_bytes, t.dead_rows) = (Some(6_000_000.0), Some(600.0 * MB), Some(0.0));
    (t.inserts, t.updates, t.deletes) = (Some(1_000_000.0), Some(0.0), Some(0.0));
    let (schema, table) = (t.schema.clone(), t.name.clone());
    let mut index = s.indexes.iter().find(|i| i.table == table).unwrap().clone();
    index.name = "customers_created_at_idx".into();
    index.columns = vec!["created_at".into()];
    (index.is_unique, index.is_primary_key, index.is_disabled) = (false, false, false);
    (index.filter, index.opclasses, index.partition_of) = (None, None, None);
    index.include.clear();
    (index.kind, index.reads, index.size_bytes) = ("btree".into(), Some(100.0), Some(2.0 * GB));
    s.indexes.push(index);
    let mut small = s.indexes.last().unwrap().clone();
    (small.name, small.size_bytes) = ("customers_created_at_small_idx".into(), Some(200.0 * MB));
    s.indexes.push(small);
    let _ = schema;
    says(
        &run("table-bloat", &s),
        r#"id: table-bloat:brin-candidate:public.customers.customers_created_at_idx
title: customers_created_at_idx on append-only customers takes 2.0 GB as a B-tree (a BRIN index would take < 1%)
summary: customers holds 6.0M rows (600 MB) and its recorded writes are 100% inserts (1.0M inserts vs 0 updates/deletes). customers_created_at_idx is a 2.0 GB B-tree index on `created_at`: because rows in an append-only table are stored on heap pages in roughly `created_at` order, a Block Range Index (BRIN) on `created_at` stores only the minimum and maximum timestamp per page range — taking kilobytes instead of 2.0 GB while still skipping non-matching heap blocks on time-range queries.
impact: 2.0 GB B-tree on append-only time column
metric: B-tree index size
metric: Table rows
metric: Inserts
fix: If `created_at` is queried by time ranges rather than point lookups and rows are inserted in timestamp order, build a BRIN index concurrently and verify with `EXPLAIN` that range queries use a Bitmap Index Scan on it before retiring `customers_created_at_idx`.
cost: CONCURRENTLY scans the table without blocking reads or writes; the resulting BRIN index is tiny.
sql: CREATE INDEX CONCURRENTLY ix_customers_created_at_brin ON public.customers USING brin (created_at);
verify: Re-scan after building `ix_customers_created_at_brin`: verify time-range queries use it before dropping `customers_created_at_idx`.

id: table-bloat:brin-candidate:public.customers.customers_created_at_small_idx
title: customers_created_at_small_idx on append-only customers takes 200 MB as a B-tree (a BRIN index would take < 1%)
summary: customers holds 6.0M rows (600 MB) and its recorded writes are 100% inserts (1.0M inserts vs 0 updates/deletes). customers_created_at_small_idx is a 200 MB B-tree index on `created_at`: because rows in an append-only table are stored on heap pages in roughly `created_at` order, a Block Range Index (BRIN) on `created_at` stores only the minimum and maximum timestamp per page range — taking kilobytes instead of 200 MB while still skipping non-matching heap blocks on time-range queries.
impact: 200 MB B-tree on append-only time column
metric: B-tree index size
metric: Table rows
metric: Inserts
fix: If `created_at` is queried by time ranges rather than point lookups and rows are inserted in timestamp order, build a BRIN index concurrently and verify with `EXPLAIN` that range queries use a Bitmap Index Scan on it before retiring `customers_created_at_small_idx`.
cost: CONCURRENTLY scans the table without blocking reads or writes; the resulting BRIN index is tiny.
sql: CREATE INDEX CONCURRENTLY ix_customers_created_at_brin ON public.customers USING brin (created_at);
verify: Re-scan after building `ix_customers_created_at_brin`: verify time-range queries use it before dropping `customers_created_at_small_idx`."#,
    );
}

// ── sequence-capacity ──────────────────────────────────────────────────────────────────────────────

#[test]
fn sequences_near_their_bound() {
    use culprit_core::operations::SequenceCapacity;
    let sequence = |name: &str, current: &str, allocated: bool, table: Option<&str>| SequenceCapacity {
        name: name.into(),
        table: table.map(str::to_string),
        current: current.into(),
        increment: "1".into(),
        minimum: "1".into(),
        maximum: "2147483647".into(),
        cycles: false,
        allocated,
    };
    let mut s = fixture("mssql-demo");
    s.operations.sequences = vec![
        sequence("dbo.order_numbers", "2147480000", true, Some("shop.orders")),
        sequence("dbo.ticket_numbers", "2127483647", false, None),
    ];
    says(
        &run("sequence-capacity", &s),
        r#"id: sequence-capacity:dbo.order_numbers
title: dbo.order_numbers is near its numeric limit (3.6K recorded steps remain)
summary: The recorded value is 2147480000, increment 1, with bounds 1 to 2147483647 and cycling off. The source may record reserved cache space or the next auto-increment value rather than the last value returned to a caller. Caching, skipped values and per-connection allocation rules mean this is not the number of application requests left or a time-to-failure estimate. Reaching the bound can reject inserts or allocations; reusing old numbers can violate uniqueness.
impact: 3.6K steps to bound
metric: Recorded value = 2147480000
metric: Minimum = 1
metric: Maximum = 2147483647
metric: Steps after recorded allocation
metric: Table = shop.orders
fix: Plan a migration to a larger numeric range, including referencing columns, constraints, indexes and application types. Validate cache/increment behavior and rollout locking first. Do not reseed, restart or enable cycling without proving values cannot collide.
cost: Changing column types and dependent keys may rewrite data and require locks; plan and test the migration.
verify: Check the new type and allocation bounds, confirm foreign keys and application values remain compatible, and verify inserts or sequence allocations under representative load.

id: sequence-capacity:dbo.ticket_numbers
title: dbo.ticket_numbers is near its numeric limit (20.0M recorded steps remain)
summary: The recorded value is 2127483647, increment 1, with bounds 1 to 2147483647 and cycling off. This is the allocation state observed by the source. Caching, skipped values and per-connection allocation rules mean this is not the number of application requests left or a time-to-failure estimate. Reaching the bound can reject inserts or allocations; reusing old numbers can violate uniqueness.
impact: 20.0M steps to bound
metric: Recorded value = 2127483647
metric: Minimum = 1
metric: Maximum = 2147483647
metric: Steps after recorded allocation
fix: Plan a migration to a larger numeric range, including referencing columns, constraints, indexes and application types. Validate cache/increment behavior and rollout locking first. Do not reseed, restart or enable cycling without proving values cannot collide.
cost: Changing column types and dependent keys may rewrite data and require locks; plan and test the migration.
verify: Check the new type and allocation bounds, confirm foreign keys and application values remain compatible, and verify inserts or sequence allocations under representative load."#,
    );
}

#[test]
fn int32_primary_keys_on_huge_tables() {
    let mut s = fixture("postgres-demo");
    for (table, rows) in [("orders", 150_000_000.0), ("customers", 600_000_000.0)] {
        s.tables.iter_mut().find(|t| t.name == table).unwrap().rows = Some(rows);
        let pk = s.indexes.iter().find(|i| i.table == table && i.is_primary_key).expect("a primary key").clone();
        let column = culprit_core::strip_direction(&pk.columns[0]).to_string();
        s.columns.retain(|c| !(c.table == table && c.name == column));
        s.columns.push(culprit_core::ColumnInfo {
            schema: "public".into(),
            table: table.into(),
            name: column,
            data_type: "integer".into(),
            nullable: false,
            default: None,
            generated: false,
            ordinal: 1,
        });
    }
    says(
        &run("sequence-capacity", &s),
        r#"id: sequence-capacity:int32-pk:public.orders.id
title: orders.id is a 32-bit integer primary key on a 150.0M-row table
summary: Primary key public.orders.id is declared as `integer` (signed 32-bit maximum 2,147,483,647) and public.orders already holds 150.0M live rows. Once the sequence or auto-increment counter reaches 2,147,483,647 — which happens earlier than the live row count when rows have been deleted or sequence caches skipped values — every new INSERT into public.orders fails with an integer overflow error.
impact: 150.0M rows on 32-bit PK
metric: Rows in table
metric: Primary key column = orders.id (integer)
metric: Signed 32-bit limit = 2,147,483,647
fix: Plan a multi-step migration of public.orders.id (and any foreign-key columns referencing it) to 64-bit `bigint` before the 32-bit range is exhausted.
cost: Widening a primary key column to bigint rewrites the table or requires a dual-column backfill and swap.
verify: Re-scan after migrating public.orders.id to `bigint`: its type is `bigint` and this finding is gone.

id: sequence-capacity:int32-pk:public.customers.id
title: customers.id is a 32-bit integer primary key on a 600.0M-row table
summary: Primary key public.customers.id is declared as `integer` (signed 32-bit maximum 2,147,483,647) and public.customers already holds 600.0M live rows. Once the sequence or auto-increment counter reaches 2,147,483,647 — which happens earlier than the live row count when rows have been deleted or sequence caches skipped values — every new INSERT into public.customers fails with an integer overflow error.
impact: 600.0M rows on 32-bit PK
metric: Rows in table
metric: Primary key column = customers.id (integer)
metric: Signed 32-bit limit = 2,147,483,647
fix: Plan a multi-step migration of public.customers.id (and any foreign-key columns referencing it) to 64-bit `bigint` before the 32-bit range is exhausted.
cost: Widening a primary key column to bigint rewrites the table or requires a dual-column backfill and swap.
verify: Re-scan after migrating public.customers.id to `bigint`: its type is `bigint` and this finding is gone."#,
    );
}

#[test]
fn range_partitions_about_to_end_on_mysql() {
    let mut s = fixture("mysql-demo");
    s.tables[0].storage = storage(&["partition_bound=VALUES LESS THAN ('2026-10-13'):p202610"]);
    s.tables[1].storage = storage(&[
        "partition_bound=VALUES LESS THAN ('2026-10-01'):p202609",
        "partition_bound=VALUES LESS THAN ('2026-10-16'):p202610",
    ]);
    says(
        &run("sequence-capacity", &s),
        r#"id: table-bloat:expiring-partition:shop.customers
title: customers's last partition (p202610) ends on 2026-10-13 (in 1.4 days)
summary: shop.customers has 1 range partition and no DEFAULT or MAXVALUE partition. Its latest partition (p202610) covers values up to '2026-10-13': once timestamps reach '2026-10-13', INSERTs into shop.customers will fail with a partition-routing error.
impact: ends in 1.4 days
metric: Partitions
metric: Days until upper bound
fix: Create the next range partition(s) on shop.customers covering values from '2026-10-13' onward, and check the scheduled job or partition manager that pre-creates future partitions.
cost: Creating a new empty partition is fast metadata DDL.
verify: Re-scan after creating future partitions: shop.customers's highest partition bound should be more than 7 days ahead.

id: table-bloat:expiring-partition:shop.products
title: products's last partition (p202610) ends on 2026-10-16 (in 4.4 days)
summary: shop.products has 2 range partitions and no DEFAULT or MAXVALUE partition. Its latest partition (p202610) covers values up to '2026-10-16': once timestamps reach '2026-10-16', INSERTs into shop.products will fail with a partition-routing error.
impact: ends in 4.4 days
metric: Partitions
metric: Days until upper bound
fix: Create the next range partition(s) on shop.products covering values from '2026-10-16' onward, and check the scheduled job or partition manager that pre-creates future partitions.
cost: Creating a new empty partition is fast metadata DDL.
verify: Re-scan after creating future partitions: shop.products's highest partition bound should be more than 7 days ahead."#,
    );
}

// ── lock-escalations ───────────────────────────────────────────────────────────────────────────────

/// The SQL Server demo with escalations (and lock waits) only on `table`'s first index, the server started at
/// `started`, and a query history of only `SELECT 1`.
fn escalated(table: &str, escalations: f64, lock_wait_ms: f64, started: Option<&str>) -> Snapshot {
    let mut s = fixture("mssql-demo");
    s.indexes.iter_mut().for_each(|i| i.contention = None);
    let i = s.indexes.iter_mut().find(|i| i.table == table).expect("an index");
    i.contention = Some(culprit_core::IndexContention {
        lock_escalations: escalations,
        row_lock_waits: if lock_wait_ms > 0.0 { 10.0 } else { 0.0 },
        row_lock_wait_ms: lock_wait_ms,
        ..Default::default()
    });
    s.server.started_at = started.map(str::to_string);
    if let Some(p) = s.pressure.as_mut() {
        p.uptime_hours = None;
    }
    let q = only(&mut s, "SELECT 1");
    (q.first_seen, q.last_seen) = (None, None);
    s
}

/// A statement added to `s`'s history: `runs` runs of `rows` rows each.
fn ran(s: &mut Snapshot, id: &str, text: &str, runs: f64, rows: f64) {
    let w = s.workload.as_mut().expect("workload");
    let mut q = w.queries[0].clone();
    (q.id, q.text, q.executions, q.rows) = (id.into(), text.into(), runs, Some(rows * runs));
    w.queries.push(q);
}

/// An earlier scan of `s`, `hours` before it, that counted `escalations` on `table` (and the server started at
/// `started`, when it says).
fn scanned_before(s: &mut Snapshot, hours: f64, table: &str, escalations: f64, started: Option<&str>) {
    use culprit_core::format::{epoch_seconds, iso_from_epoch};
    let mut before = s.clone();
    before.taken_at = iso_from_epoch(epoch_seconds(&s.taken_at).unwrap() - hours * 3600.0);
    if let Some(at) = started {
        before.server.started_at = Some(at.into());
    }
    for i in before.indexes.iter_mut().filter(|i| i.table == table) {
        if let Some(c) = i.contention.as_mut() {
            c.lock_escalations = escalations;
        }
    }
    s.escalations_before = culprit_core::EscalationBaseline::from_scan(&before, s);
    assert!(s.escalations_before.is_some(), "a comparable earlier scan");
}

#[test]
fn lock_escalations_on_a_first_scan() {
    let mut findings = Vec::new();
    // Once a day or more on average, with a write that changes thousands of rows a run.
    let mut s = escalated("support_tickets", 60.0, 30_000.0, Some("2026-09-11T12:13:47Z"));
    ran(&mut s, "71", "DELETE FROM shop.support_tickets WHERE created_at < @P1", 10.0, 5_000.0);
    findings.extend(run("lock-escalations", &s));
    // Once a week or more, with writes of a few rows a run.
    let mut s = escalated("orders", 6.0, 0.0, Some("2026-09-11T12:13:47Z"));
    ran(&mut s, "72", "UPDATE shop.orders SET status = @P1 WHERE id = @P2", 100.0, 5.0);
    ran(&mut s, "73", "UPDATE shop.orders SET note = @P1 WHERE id = @P2", 300.0, 0.4);
    findings.extend(run("lock-escalations", &s));
    // Less than once a week, with a read that asks for a locking hint.
    let mut s = escalated("customers", 2.0, 0.0, Some("2025-12-15T12:13:47Z"));
    set(&mut s, "is_read_committed_snapshot_on", "0");
    ran(&mut s, "74", "SELECT * FROM shop.customers WITH (HOLDLOCK) WHERE region = @P1", 50.0, 3_000.0);
    ran(&mut s, "75", "SELECT id FROM shop.customers WHERE email = @P1", 50.0, 1.0);
    findings.extend(run("lock-escalations", &s));
    // Counting for three hours, then for twenty minutes and a minute; and since an unknown start.
    for started in ["2026-10-11T09:13:47Z", "2026-10-11T11:53:47Z", "2026-10-11T12:13:17Z"] {
        let mut s = escalated("products", 1.0, 0.0, Some(started));
        set(&mut s, "is_read_committed_snapshot_on", "1");
        ran(&mut s, "76", "SELECT * FROM shop.products WITH (UPDLOCK) WHERE sku = @P1", 50.0, 2.0);
        findings.extend(run("lock-escalations", &s));
    }
    findings.extend(run("lock-escalations", &escalated("order_items", 3.0, 0.0, None)));
    says(
        &findings,
        r#"id: lock-escalations:shop.support_tickets
title: Row locks on support_tickets escalated to a table lock 60 times since SQL Server started (Sep 11)
summary: SQL Server replaced the row and page locks on support_tickets with one lock on the whole table 60 times since it started, on Sep 11 at 12:13 UTC. It keeps no time for each, and Culprit has no earlier scan to compare with, so they may be old: the next scan compares with this one and says whether any happened since. That is once a day or more on average over the counter history; it does not date individual events. SQL Server can attempt escalation around 5,000 locks on one table reference, or under lock-memory pressure. A successful table lock can block incompatible lock requests until the transaction ends; the escalation counter alone does not establish that other sessions were blocked. SQL Server doesn't record which statement it was; the likeliest is a delete from support_tickets, which changes about 5.0K rows a run.
impact: 60 table locks
metric: Escalations since SQL Server started
metric: Lock waits on its indexes
metric: Time waiting
note: Changes about 5.0K rows a run.
fix: Change the statement that touches thousands of rows of support_tickets into smaller batches, for example up to 1,000 changed rows per statement, after checking its access path and lock footprint. A row limit does not bound all locks acquired or prevent escalation under lock-memory pressure. Use smaller transactions only where partial commits preserve the application's required atomicity. An UPDATE loop's filter must leave out rows already changed. A single enclosing transaction can retain locks until commit, so batching is no guarantee against a table lock. To be sure it is the one, start the event session below (culprit_lock_escalation): it keeps each escalation with its statement, in memory, at almost no cost.
sql: -- Example only: check lock footprint and required atomicity before batching
-- WHILE 1 = 1
-- BEGIN
--     DELETE TOP (1000) FROM [shop].[support_tickets] WHERE /* the same filter */;
--     IF @@ROWCOUNT = 0 BREAK;
-- END;

-- SQL Server doesn't record which statement escalated: this session keeps the next ones with their statement.
-- It catches them on every table, so another table's finding starts this same session: once is enough.
IF NOT EXISTS (SELECT 1 FROM sys.server_event_sessions WHERE name = N'culprit_lock_escalation')
    CREATE EVENT SESSION culprit_lock_escalation ON SERVER
    ADD EVENT sqlserver.lock_escalation (
        SET collect_statement = 1
        ACTION (sqlserver.sql_text, sqlserver.client_app_name)
        WHERE sqlserver.database_name = N'culprit_demo')
    ADD TARGET package0.ring_buffer
    WITH (STARTUP_STATE = ON);
IF NOT EXISTS (SELECT 1 FROM sys.dm_xe_sessions WHERE name = N'culprit_lock_escalation')
    ALTER EVENT SESSION culprit_lock_escalation ON SERVER STATE = START;

-- Then read what it caught
SELECT e.x.value('@timestamp', 'datetime2') AS at_utc,
       OBJECT_NAME(e.x.value('(data[@name="object_id"]/value)[1]', 'int')) AS table_name,
       e.x.value('(data[@name="escalated_lock_count"]/value)[1]', 'int') AS locks,
       e.x.value('(data[@name="statement"]/value)[1]', 'nvarchar(max)') AS statement,
       e.x.value('(action[@name="client_app_name"]/value)[1]', 'nvarchar(256)') AS app
  FROM (SELECT CAST(t.target_data AS xml) AS d
          FROM sys.dm_xe_session_targets t
          JOIN sys.dm_xe_sessions s ON s.address = t.event_session_address
         WHERE s.name = N'culprit_lock_escalation' AND t.target_name = N'ring_buffer') r
 CROSS APPLY r.d.nodes('RingBufferTarget/event') e(x);

-- Once it has caught what you need, stop and remove it: DROP EVENT SESSION culprit_lock_escalation ON SERVER;
verify: Compare new escalations on support_tickets and observed blocking under comparable workload before and after a justified change. No new events means none were recorded in that interval; an idle interval alone does not verify a fix.
caveat: SQL Server counts escalations since the table's indexes were loaded into memory, usually since it started, and keeps no time for each.

id: lock-escalations:shop.orders
title: Row locks on orders escalated to a table lock 6 times since SQL Server started (Sep 11)
summary: SQL Server replaced the row and page locks on orders with one lock on the whole table 6 times since it started, on Sep 11 at 12:13 UTC. It keeps no time for each, and Culprit has no earlier scan to compare with, so they may be old: the next scan compares with this one and says whether any happened since. That is once a week or more on average over the counter history; it does not establish current recurrence. SQL Server can attempt escalation around 5,000 locks on one table reference, or under lock-memory pressure. A successful table lock can block incompatible lock requests until the transaction ends; the escalation counter alone does not establish that other sessions were blocked. SQL Server doesn't record which statement it was; the writes to it are listed below, and the one to look for changes thousands of rows in one run: a purge, a cascade, a backfill.
impact: 6 table locks
metric: Escalations since SQL Server started
note: Changes about 5 rows a run.
note: Changes about 0.4 rows a run.
fix: None of the statements in the query history changes thousands of rows of orders at once, so the one that escalated is likely a job, a migration or maintenance. Once the event session below has caught it, change it into smaller batches, for example up to 1,000 changed rows per statement, after checking its access path and lock footprint. A row limit does not bound all locks acquired or prevent escalation under lock-memory pressure. Use smaller transactions only where partial commits preserve the application's required atomicity. An UPDATE loop's filter must leave out rows already changed. A single enclosing transaction can retain locks until commit, so batching is no guarantee against a table lock. Start the event session below (culprit_lock_escalation): it keeps each escalation with its statement, in memory, at almost no cost.
sql: -- SQL Server doesn't record which statement escalated: this session keeps the next ones with their statement.
-- It catches them on every table, so another table's finding starts this same session: once is enough.
IF NOT EXISTS (SELECT 1 FROM sys.server_event_sessions WHERE name = N'culprit_lock_escalation')
    CREATE EVENT SESSION culprit_lock_escalation ON SERVER
    ADD EVENT sqlserver.lock_escalation (
        SET collect_statement = 1
        ACTION (sqlserver.sql_text, sqlserver.client_app_name)
        WHERE sqlserver.database_name = N'culprit_demo')
    ADD TARGET package0.ring_buffer
    WITH (STARTUP_STATE = ON);
IF NOT EXISTS (SELECT 1 FROM sys.dm_xe_sessions WHERE name = N'culprit_lock_escalation')
    ALTER EVENT SESSION culprit_lock_escalation ON SERVER STATE = START;

-- Then read what it caught
SELECT e.x.value('@timestamp', 'datetime2') AS at_utc,
       OBJECT_NAME(e.x.value('(data[@name="object_id"]/value)[1]', 'int')) AS table_name,
       e.x.value('(data[@name="escalated_lock_count"]/value)[1]', 'int') AS locks,
       e.x.value('(data[@name="statement"]/value)[1]', 'nvarchar(max)') AS statement,
       e.x.value('(action[@name="client_app_name"]/value)[1]', 'nvarchar(256)') AS app
  FROM (SELECT CAST(t.target_data AS xml) AS d
          FROM sys.dm_xe_session_targets t
          JOIN sys.dm_xe_sessions s ON s.address = t.event_session_address
         WHERE s.name = N'culprit_lock_escalation' AND t.target_name = N'ring_buffer') r
 CROSS APPLY r.d.nodes('RingBufferTarget/event') e(x);

-- Once it has caught what you need, stop and remove it: DROP EVENT SESSION culprit_lock_escalation ON SERVER;
verify: Compare new escalations on orders and observed blocking under comparable workload before and after a justified change. No new events means none were recorded in that interval; an idle interval alone does not verify a fix.
caveat: SQL Server counts escalations since the table's indexes were loaded into memory, usually since it started, and keeps no time for each.

id: lock-escalations:shop.customers
title: Row locks on customers escalated to a table lock 2 times since SQL Server started (Dec 15, 2025)
summary: SQL Server replaced the row and page locks on customers with one lock on the whole table 2 times since it started, on Dec 15 at 12:13 UTC. It keeps no time for each, and Culprit has no earlier scan to compare with, so they may be old: the next scan compares with this one and says whether any happened since. That is less than once a week on average since then: they may all be old. SQL Server can attempt escalation around 5,000 locks on one table reference, or under lock-memory pressure. A successful table lock can block incompatible lock requests until the transaction ends; the escalation counter alone does not establish that other sessions were blocked. SQL Server doesn't record which statement it was, and none of the statements in the query history writes to customers. A read can escalate too: an ordinary read under read committed usually releases row locks as it advances, but prefetching key lookups can retain locks until the statement ends. Locking hints (HOLDLOCK, UPDLOCK), REPEATABLE READ and SERIALIZABLE can retain them until the transaction ends. Reading many rows can acquire enough locks for an escalation attempt. The likeliest is a read from customers, which returns about 3.0K rows a run on average: it asks for a locking hint, which can retain row locks until its transaction ends.
impact: 2 table locks
metric: Escalations since SQL Server started
note: Returns about 3.0K rows a run. It asks for a locking hint, which can retain row locks until its transaction ends.
note: Returns about 1 row a run.
note: Returns about 1 row a run.
fix: Start the event session below (culprit_lock_escalation) to learn which statement escalates: it keeps each escalation with its statement, in memory, at almost no cost. If it is a read, it escalates because it keeps its row locks until it ends: Without locking hints, READ_COMMITTED_SNAPSHOT lets ordinary reads under read committed avoid row locks (it changes what readers see and uses tempdb, so test it first); or drop the locking hint where the read doesn't need it, or consider reading fewer rows per run after checking the access path; row counts do not bound all locks or prevent escalation under lock-memory pressure. If it is a statement outside the query history that changes many rows at once (a job, a migration, maintenance), consider smaller batches. Use separate transactions only if partial commits preserve required atomicity; an enclosing transaction may retain locks.
sql: -- SQL Server doesn't record which statement escalated: this session keeps the next ones with their statement.
-- It catches them on every table, so another table's finding starts this same session: once is enough.
IF NOT EXISTS (SELECT 1 FROM sys.server_event_sessions WHERE name = N'culprit_lock_escalation')
    CREATE EVENT SESSION culprit_lock_escalation ON SERVER
    ADD EVENT sqlserver.lock_escalation (
        SET collect_statement = 1
        ACTION (sqlserver.sql_text, sqlserver.client_app_name)
        WHERE sqlserver.database_name = N'culprit_demo')
    ADD TARGET package0.ring_buffer
    WITH (STARTUP_STATE = ON);
IF NOT EXISTS (SELECT 1 FROM sys.dm_xe_sessions WHERE name = N'culprit_lock_escalation')
    ALTER EVENT SESSION culprit_lock_escalation ON SERVER STATE = START;

-- Then read what it caught
SELECT e.x.value('@timestamp', 'datetime2') AS at_utc,
       OBJECT_NAME(e.x.value('(data[@name="object_id"]/value)[1]', 'int')) AS table_name,
       e.x.value('(data[@name="escalated_lock_count"]/value)[1]', 'int') AS locks,
       e.x.value('(data[@name="statement"]/value)[1]', 'nvarchar(max)') AS statement,
       e.x.value('(action[@name="client_app_name"]/value)[1]', 'nvarchar(256)') AS app
  FROM (SELECT CAST(t.target_data AS xml) AS d
          FROM sys.dm_xe_session_targets t
          JOIN sys.dm_xe_sessions s ON s.address = t.event_session_address
         WHERE s.name = N'culprit_lock_escalation' AND t.target_name = N'ring_buffer') r
 CROSS APPLY r.d.nodes('RingBufferTarget/event') e(x);

-- Once it has caught what you need, stop and remove it: DROP EVENT SESSION culprit_lock_escalation ON SERVER;
verify: Compare new escalations on customers and observed blocking under comparable workload before and after a justified change. No new events means none were recorded in that interval; an idle interval alone does not verify a fix.
caveat: These totals do not establish a new increase or continuing blocking in the selected window. Capture dated events or compare counters wholly within that period before treating this as current pressure.
caveat: SQL Server counts escalations since the table's indexes were loaded into memory, usually since it started, and keeps no time for each.

id: lock-escalations:shop.products
title: Row locks on products escalated to a table lock once since SQL Server started (Oct 11)
summary: SQL Server replaced the row and page locks on products with one lock on the whole table once since it started, on Oct 11 at 09:13 UTC, 3 hours before this scan, so none is older than that. The next scan compares with this one and says whether any happened since. That is once a day or more on average over the counter history; it does not date individual events. SQL Server can attempt escalation around 5,000 locks on one table reference, or under lock-memory pressure. A successful table lock can block incompatible lock requests until the transaction ends; the escalation counter alone does not establish that other sessions were blocked. SQL Server doesn't record which statement it was, and none of the statements in the query history writes to it: look at jobs, migrations and maintenance that change many rows at once.
impact: 1 table lock
metric: Escalations since SQL Server started
fix: None of the statements in the query history changes thousands of rows of products at once, so the one that escalated is likely a job, a migration or maintenance. Once the event session below has caught it, change it into smaller batches, for example up to 1,000 changed rows per statement, after checking its access path and lock footprint. A row limit does not bound all locks acquired or prevent escalation under lock-memory pressure. Use smaller transactions only where partial commits preserve the application's required atomicity. An UPDATE loop's filter must leave out rows already changed. A single enclosing transaction can retain locks until commit, so batching is no guarantee against a table lock. Start the event session below (culprit_lock_escalation): it keeps each escalation with its statement, in memory, at almost no cost.
sql: -- SQL Server doesn't record which statement escalated: this session keeps the next ones with their statement.
-- It catches them on every table, so another table's finding starts this same session: once is enough.
IF NOT EXISTS (SELECT 1 FROM sys.server_event_sessions WHERE name = N'culprit_lock_escalation')
    CREATE EVENT SESSION culprit_lock_escalation ON SERVER
    ADD EVENT sqlserver.lock_escalation (
        SET collect_statement = 1
        ACTION (sqlserver.sql_text, sqlserver.client_app_name)
        WHERE sqlserver.database_name = N'culprit_demo')
    ADD TARGET package0.ring_buffer
    WITH (STARTUP_STATE = ON);
IF NOT EXISTS (SELECT 1 FROM sys.dm_xe_sessions WHERE name = N'culprit_lock_escalation')
    ALTER EVENT SESSION culprit_lock_escalation ON SERVER STATE = START;

-- Then read what it caught
SELECT e.x.value('@timestamp', 'datetime2') AS at_utc,
       OBJECT_NAME(e.x.value('(data[@name="object_id"]/value)[1]', 'int')) AS table_name,
       e.x.value('(data[@name="escalated_lock_count"]/value)[1]', 'int') AS locks,
       e.x.value('(data[@name="statement"]/value)[1]', 'nvarchar(max)') AS statement,
       e.x.value('(action[@name="client_app_name"]/value)[1]', 'nvarchar(256)') AS app
  FROM (SELECT CAST(t.target_data AS xml) AS d
          FROM sys.dm_xe_session_targets t
          JOIN sys.dm_xe_sessions s ON s.address = t.event_session_address
         WHERE s.name = N'culprit_lock_escalation' AND t.target_name = N'ring_buffer') r
 CROSS APPLY r.d.nodes('RingBufferTarget/event') e(x);

-- Once it has caught what you need, stop and remove it: DROP EVENT SESSION culprit_lock_escalation ON SERVER;
verify: Compare new escalations on products and observed blocking under comparable workload before and after a justified change. No new events means none were recorded in that interval; an idle interval alone does not verify a fix.
caveat: SQL Server counts escalations since the table's indexes were loaded into memory, usually since it started, and keeps no time for each.

id: lock-escalations:shop.products
title: Row locks on products escalated to a table lock once since SQL Server started (Oct 11)
summary: SQL Server replaced the row and page locks on products with one lock on the whole table once since it started, on Oct 11 at 11:53 UTC, 20 minutes before this scan, so none is older than that. The next scan compares with this one and says whether any happened since. SQL Server started too recently to tell how often it happens. SQL Server can attempt escalation around 5,000 locks on one table reference, or under lock-memory pressure. A successful table lock can block incompatible lock requests until the transaction ends; the escalation counter alone does not establish that other sessions were blocked. SQL Server doesn't record which statement it was, and none of the statements in the query history writes to it: look at jobs, migrations and maintenance that change many rows at once.
impact: 1 table lock
metric: Escalations since SQL Server started
fix: None of the statements in the query history changes thousands of rows of products at once, so the one that escalated is likely a job, a migration or maintenance. Once the event session below has caught it, change it into smaller batches, for example up to 1,000 changed rows per statement, after checking its access path and lock footprint. A row limit does not bound all locks acquired or prevent escalation under lock-memory pressure. Use smaller transactions only where partial commits preserve the application's required atomicity. An UPDATE loop's filter must leave out rows already changed. A single enclosing transaction can retain locks until commit, so batching is no guarantee against a table lock. Start the event session below (culprit_lock_escalation): it keeps each escalation with its statement, in memory, at almost no cost.
sql: -- SQL Server doesn't record which statement escalated: this session keeps the next ones with their statement.
-- It catches them on every table, so another table's finding starts this same session: once is enough.
IF NOT EXISTS (SELECT 1 FROM sys.server_event_sessions WHERE name = N'culprit_lock_escalation')
    CREATE EVENT SESSION culprit_lock_escalation ON SERVER
    ADD EVENT sqlserver.lock_escalation (
        SET collect_statement = 1
        ACTION (sqlserver.sql_text, sqlserver.client_app_name)
        WHERE sqlserver.database_name = N'culprit_demo')
    ADD TARGET package0.ring_buffer
    WITH (STARTUP_STATE = ON);
IF NOT EXISTS (SELECT 1 FROM sys.dm_xe_sessions WHERE name = N'culprit_lock_escalation')
    ALTER EVENT SESSION culprit_lock_escalation ON SERVER STATE = START;

-- Then read what it caught
SELECT e.x.value('@timestamp', 'datetime2') AS at_utc,
       OBJECT_NAME(e.x.value('(data[@name="object_id"]/value)[1]', 'int')) AS table_name,
       e.x.value('(data[@name="escalated_lock_count"]/value)[1]', 'int') AS locks,
       e.x.value('(data[@name="statement"]/value)[1]', 'nvarchar(max)') AS statement,
       e.x.value('(action[@name="client_app_name"]/value)[1]', 'nvarchar(256)') AS app
  FROM (SELECT CAST(t.target_data AS xml) AS d
          FROM sys.dm_xe_session_targets t
          JOIN sys.dm_xe_sessions s ON s.address = t.event_session_address
         WHERE s.name = N'culprit_lock_escalation' AND t.target_name = N'ring_buffer') r
 CROSS APPLY r.d.nodes('RingBufferTarget/event') e(x);

-- Once it has caught what you need, stop and remove it: DROP EVENT SESSION culprit_lock_escalation ON SERVER;
verify: Compare new escalations on products and observed blocking under comparable workload before and after a justified change. No new events means none were recorded in that interval; an idle interval alone does not verify a fix.
caveat: These totals do not establish a new increase or continuing blocking in the selected window. Capture dated events or compare counters wholly within that period before treating this as current pressure.
caveat: SQL Server counts escalations since the table's indexes were loaded into memory, usually since it started, and keeps no time for each.

id: lock-escalations:shop.products
title: Row locks on products escalated to a table lock once since SQL Server started (Oct 11)
summary: SQL Server replaced the row and page locks on products with one lock on the whole table once since it started, on Oct 11 at 12:13 UTC, a minute before this scan, so none is older than that. The next scan compares with this one and says whether any happened since. SQL Server started too recently to tell how often it happens. SQL Server can attempt escalation around 5,000 locks on one table reference, or under lock-memory pressure. A successful table lock can block incompatible lock requests until the transaction ends; the escalation counter alone does not establish that other sessions were blocked. SQL Server doesn't record which statement it was, and none of the statements in the query history writes to it: look at jobs, migrations and maintenance that change many rows at once.
impact: 1 table lock
metric: Escalations since SQL Server started
fix: None of the statements in the query history changes thousands of rows of products at once, so the one that escalated is likely a job, a migration or maintenance. Once the event session below has caught it, change it into smaller batches, for example up to 1,000 changed rows per statement, after checking its access path and lock footprint. A row limit does not bound all locks acquired or prevent escalation under lock-memory pressure. Use smaller transactions only where partial commits preserve the application's required atomicity. An UPDATE loop's filter must leave out rows already changed. A single enclosing transaction can retain locks until commit, so batching is no guarantee against a table lock. Start the event session below (culprit_lock_escalation): it keeps each escalation with its statement, in memory, at almost no cost.
sql: -- SQL Server doesn't record which statement escalated: this session keeps the next ones with their statement.
-- It catches them on every table, so another table's finding starts this same session: once is enough.
IF NOT EXISTS (SELECT 1 FROM sys.server_event_sessions WHERE name = N'culprit_lock_escalation')
    CREATE EVENT SESSION culprit_lock_escalation ON SERVER
    ADD EVENT sqlserver.lock_escalation (
        SET collect_statement = 1
        ACTION (sqlserver.sql_text, sqlserver.client_app_name)
        WHERE sqlserver.database_name = N'culprit_demo')
    ADD TARGET package0.ring_buffer
    WITH (STARTUP_STATE = ON);
IF NOT EXISTS (SELECT 1 FROM sys.dm_xe_sessions WHERE name = N'culprit_lock_escalation')
    ALTER EVENT SESSION culprit_lock_escalation ON SERVER STATE = START;

-- Then read what it caught
SELECT e.x.value('@timestamp', 'datetime2') AS at_utc,
       OBJECT_NAME(e.x.value('(data[@name="object_id"]/value)[1]', 'int')) AS table_name,
       e.x.value('(data[@name="escalated_lock_count"]/value)[1]', 'int') AS locks,
       e.x.value('(data[@name="statement"]/value)[1]', 'nvarchar(max)') AS statement,
       e.x.value('(action[@name="client_app_name"]/value)[1]', 'nvarchar(256)') AS app
  FROM (SELECT CAST(t.target_data AS xml) AS d
          FROM sys.dm_xe_session_targets t
          JOIN sys.dm_xe_sessions s ON s.address = t.event_session_address
         WHERE s.name = N'culprit_lock_escalation' AND t.target_name = N'ring_buffer') r
 CROSS APPLY r.d.nodes('RingBufferTarget/event') e(x);

-- Once it has caught what you need, stop and remove it: DROP EVENT SESSION culprit_lock_escalation ON SERVER;
verify: Compare new escalations on products and observed blocking under comparable workload before and after a justified change. No new events means none were recorded in that interval; an idle interval alone does not verify a fix.
caveat: These totals do not establish a new increase or continuing blocking in the selected window. Capture dated events or compare counters wholly within that period before treating this as current pressure.
caveat: SQL Server counts escalations since the table's indexes were loaded into memory, usually since it started, and keeps no time for each.

id: lock-escalations:shop.order_items
title: Row locks on order_items escalated to a table lock 3 times since its indexes were loaded into memory
summary: SQL Server replaced the row and page locks on order_items with one lock on the whole table 3 times since its indexes were loaded into memory. It keeps no time for each, and Culprit has no earlier scan to compare with, so they may be old: the next scan compares with this one and says whether any happened since. SQL Server started too recently to tell how often it happens. SQL Server can attempt escalation around 5,000 locks on one table reference, or under lock-memory pressure. A successful table lock can block incompatible lock requests until the transaction ends; the escalation counter alone does not establish that other sessions were blocked. SQL Server doesn't record which statement it was, and none of the statements in the query history writes to it: look at jobs, migrations and maintenance that change many rows at once.
impact: 3 table locks
metric: Escalations since SQL Server started
fix: None of the statements in the query history changes thousands of rows of order_items at once, so the one that escalated is likely a job, a migration or maintenance. Once the event session below has caught it, change it into smaller batches, for example up to 1,000 changed rows per statement, after checking its access path and lock footprint. A row limit does not bound all locks acquired or prevent escalation under lock-memory pressure. Use smaller transactions only where partial commits preserve the application's required atomicity. An UPDATE loop's filter must leave out rows already changed. A single enclosing transaction can retain locks until commit, so batching is no guarantee against a table lock. Start the event session below (culprit_lock_escalation): it keeps each escalation with its statement, in memory, at almost no cost.
sql: -- SQL Server doesn't record which statement escalated: this session keeps the next ones with their statement.
-- It catches them on every table, so another table's finding starts this same session: once is enough.
IF NOT EXISTS (SELECT 1 FROM sys.server_event_sessions WHERE name = N'culprit_lock_escalation')
    CREATE EVENT SESSION culprit_lock_escalation ON SERVER
    ADD EVENT sqlserver.lock_escalation (
        SET collect_statement = 1
        ACTION (sqlserver.sql_text, sqlserver.client_app_name)
        WHERE sqlserver.database_name = N'culprit_demo')
    ADD TARGET package0.ring_buffer
    WITH (STARTUP_STATE = ON);
IF NOT EXISTS (SELECT 1 FROM sys.dm_xe_sessions WHERE name = N'culprit_lock_escalation')
    ALTER EVENT SESSION culprit_lock_escalation ON SERVER STATE = START;

-- Then read what it caught
SELECT e.x.value('@timestamp', 'datetime2') AS at_utc,
       OBJECT_NAME(e.x.value('(data[@name="object_id"]/value)[1]', 'int')) AS table_name,
       e.x.value('(data[@name="escalated_lock_count"]/value)[1]', 'int') AS locks,
       e.x.value('(data[@name="statement"]/value)[1]', 'nvarchar(max)') AS statement,
       e.x.value('(action[@name="client_app_name"]/value)[1]', 'nvarchar(256)') AS app
  FROM (SELECT CAST(t.target_data AS xml) AS d
          FROM sys.dm_xe_session_targets t
          JOIN sys.dm_xe_sessions s ON s.address = t.event_session_address
         WHERE s.name = N'culprit_lock_escalation' AND t.target_name = N'ring_buffer') r
 CROSS APPLY r.d.nodes('RingBufferTarget/event') e(x);

-- Once it has caught what you need, stop and remove it: DROP EVENT SESSION culprit_lock_escalation ON SERVER;
verify: Compare new escalations on order_items and observed blocking under comparable workload before and after a justified change. No new events means none were recorded in that interval; an idle interval alone does not verify a fix.
caveat: These totals do not establish a new increase or continuing blocking in the selected window. Capture dated events or compare counters wholly within that period before treating this as current pressure.
caveat: SQL Server counts escalations since the table's indexes were loaded into memory, usually since it started, and keeps no time for each."#,
    );
}

#[test]
fn lock_escalations_since_an_earlier_scan() {
    let started = Some("2026-09-11T12:13:47Z");
    let mut findings = Vec::new();
    // New since the earlier scan, some of all it counted; a bulk update.
    let mut s = escalated("support_tickets", 5.0, 0.0, started);
    ran(&mut s, "81", "UPDATE shop.support_tickets SET status = @P1 WHERE created_at < @P2", 10.0, 4_000.0);
    scanned_before(&mut s, 6.0, "support_tickets", 2.0, None);
    findings.extend(run("lock-escalations", &s));
    // All it counted came after the earlier scan, one of them.
    let mut s = escalated("orders", 1.0, 0.0, started);
    scanned_before(&mut s, 6.0, "orders", 0.0, None);
    findings.extend(run("lock-escalations", &s));
    // The server restarted since.
    let mut s = escalated("orders", 4.0, 0.0, Some("2026-10-11T08:13:47Z"));
    scanned_before(&mut s, 6.0, "orders", 9.0, started);
    findings.extend(run("lock-escalations", &s));
    // None since: history, with writes listed and without.
    let mut s = escalated("customers", 3.0, 0.0, started);
    ran(&mut s, "82", "UPDATE shop.customers SET name = @P1 WHERE id = @P2", 100.0, 1.0);
    scanned_before(&mut s, 150.0, "customers", 3.0, None);
    findings.extend(run("lock-escalations", &s));
    let mut s = escalated("customers", 3.0, 0.0, started);
    scanned_before(&mut s, 150.0, "customers", 3.0, None);
    findings.extend(run("lock-escalations", &s));
    // An earlier scan beyond the selected window.
    let mut s = escalated("products", 4.0, 0.0, started);
    scanned_before(&mut s, 300.0, "products", 1.0, None);
    findings.extend(run("lock-escalations", &s));
    says(
        &findings,
        r#"id: lock-escalations:shop.support_tickets
title: Row locks on support_tickets escalated to a table lock 3 times since Oct 11, 06:13 UTC
summary: SQL Server replaced the row and page locks on support_tickets with one lock on the whole table 3 times since the previous scan, on Oct 11 at 06:13 UTC. It counted 5 in all since SQL Server started (Sep 11). SQL Server can attempt escalation around 5,000 locks on one table reference, or under lock-memory pressure. A successful table lock can block incompatible lock requests until the transaction ends; the escalation counter alone does not establish that other sessions were blocked. SQL Server doesn't record which statement it was; the likeliest is an update of support_tickets, which changes about 4.0K rows a run.
impact: 3 new table locks
metric: Escalations since SQL Server started
metric: Escalations since the previous scan
note: Changes about 4.0K rows a run.
fix: Change the statement that touches thousands of rows of support_tickets into smaller batches, for example up to 1,000 changed rows per statement, after checking its access path and lock footprint. A row limit does not bound all locks acquired or prevent escalation under lock-memory pressure. Use smaller transactions only where partial commits preserve the application's required atomicity. An UPDATE loop's filter must leave out rows already changed. A single enclosing transaction can retain locks until commit, so batching is no guarantee against a table lock. To be sure it is the one, start the event session below (culprit_lock_escalation): it keeps each escalation with its statement, in memory, at almost no cost.
sql: -- Example only: check lock footprint and required atomicity before batching
-- WHILE 1 = 1
-- BEGIN
--     UPDATE TOP (1000) [shop].[support_tickets] SET /* the same changes */ WHERE /* the same filter, leaving out rows already changed */;
--     IF @@ROWCOUNT = 0 BREAK;
-- END;

-- SQL Server doesn't record which statement escalated: this session keeps the next ones with their statement.
-- It catches them on every table, so another table's finding starts this same session: once is enough.
IF NOT EXISTS (SELECT 1 FROM sys.server_event_sessions WHERE name = N'culprit_lock_escalation')
    CREATE EVENT SESSION culprit_lock_escalation ON SERVER
    ADD EVENT sqlserver.lock_escalation (
        SET collect_statement = 1
        ACTION (sqlserver.sql_text, sqlserver.client_app_name)
        WHERE sqlserver.database_name = N'culprit_demo')
    ADD TARGET package0.ring_buffer
    WITH (STARTUP_STATE = ON);
IF NOT EXISTS (SELECT 1 FROM sys.dm_xe_sessions WHERE name = N'culprit_lock_escalation')
    ALTER EVENT SESSION culprit_lock_escalation ON SERVER STATE = START;

-- Then read what it caught
SELECT e.x.value('@timestamp', 'datetime2') AS at_utc,
       OBJECT_NAME(e.x.value('(data[@name="object_id"]/value)[1]', 'int')) AS table_name,
       e.x.value('(data[@name="escalated_lock_count"]/value)[1]', 'int') AS locks,
       e.x.value('(data[@name="statement"]/value)[1]', 'nvarchar(max)') AS statement,
       e.x.value('(action[@name="client_app_name"]/value)[1]', 'nvarchar(256)') AS app
  FROM (SELECT CAST(t.target_data AS xml) AS d
          FROM sys.dm_xe_session_targets t
          JOIN sys.dm_xe_sessions s ON s.address = t.event_session_address
         WHERE s.name = N'culprit_lock_escalation' AND t.target_name = N'ring_buffer') r
 CROSS APPLY r.d.nodes('RingBufferTarget/event') e(x);

-- Once it has caught what you need, stop and remove it: DROP EVENT SESSION culprit_lock_escalation ON SERVER;
verify: Compare new escalations on support_tickets and observed blocking under comparable workload before and after a justified change. No new events means none were recorded in that interval; an idle interval alone does not verify a fix.
caveat: SQL Server counts escalations since the table's indexes were loaded into memory, usually since it started, and keeps no time for each.

id: lock-escalations:shop.orders
title: Row locks on orders escalated to a table lock once since Oct 11, 06:13 UTC
summary: SQL Server replaced the row and page locks on orders with one lock on the whole table once since the previous scan, on Oct 11 at 06:13 UTC. All 1 it counted since SQL Server started (Sep 11) came after that scan. SQL Server can attempt escalation around 5,000 locks on one table reference, or under lock-memory pressure. A successful table lock can block incompatible lock requests until the transaction ends; the escalation counter alone does not establish that other sessions were blocked. SQL Server doesn't record which statement it was, and none of the statements in the query history writes to it: look at jobs, migrations and maintenance that change many rows at once.
impact: 1 new table lock
metric: Escalations since SQL Server started
metric: Escalations since the previous scan
fix: None of the statements in the query history changes thousands of rows of orders at once, so the one that escalated is likely a job, a migration or maintenance. Once the event session below has caught it, change it into smaller batches, for example up to 1,000 changed rows per statement, after checking its access path and lock footprint. A row limit does not bound all locks acquired or prevent escalation under lock-memory pressure. Use smaller transactions only where partial commits preserve the application's required atomicity. An UPDATE loop's filter must leave out rows already changed. A single enclosing transaction can retain locks until commit, so batching is no guarantee against a table lock. Start the event session below (culprit_lock_escalation): it keeps each escalation with its statement, in memory, at almost no cost.
sql: -- SQL Server doesn't record which statement escalated: this session keeps the next ones with their statement.
-- It catches them on every table, so another table's finding starts this same session: once is enough.
IF NOT EXISTS (SELECT 1 FROM sys.server_event_sessions WHERE name = N'culprit_lock_escalation')
    CREATE EVENT SESSION culprit_lock_escalation ON SERVER
    ADD EVENT sqlserver.lock_escalation (
        SET collect_statement = 1
        ACTION (sqlserver.sql_text, sqlserver.client_app_name)
        WHERE sqlserver.database_name = N'culprit_demo')
    ADD TARGET package0.ring_buffer
    WITH (STARTUP_STATE = ON);
IF NOT EXISTS (SELECT 1 FROM sys.dm_xe_sessions WHERE name = N'culprit_lock_escalation')
    ALTER EVENT SESSION culprit_lock_escalation ON SERVER STATE = START;

-- Then read what it caught
SELECT e.x.value('@timestamp', 'datetime2') AS at_utc,
       OBJECT_NAME(e.x.value('(data[@name="object_id"]/value)[1]', 'int')) AS table_name,
       e.x.value('(data[@name="escalated_lock_count"]/value)[1]', 'int') AS locks,
       e.x.value('(data[@name="statement"]/value)[1]', 'nvarchar(max)') AS statement,
       e.x.value('(action[@name="client_app_name"]/value)[1]', 'nvarchar(256)') AS app
  FROM (SELECT CAST(t.target_data AS xml) AS d
          FROM sys.dm_xe_session_targets t
          JOIN sys.dm_xe_sessions s ON s.address = t.event_session_address
         WHERE s.name = N'culprit_lock_escalation' AND t.target_name = N'ring_buffer') r
 CROSS APPLY r.d.nodes('RingBufferTarget/event') e(x);

-- Once it has caught what you need, stop and remove it: DROP EVENT SESSION culprit_lock_escalation ON SERVER;
verify: Compare new escalations on orders and observed blocking under comparable workload before and after a justified change. No new events means none were recorded in that interval; an idle interval alone does not verify a fix.
caveat: SQL Server counts escalations since the table's indexes were loaded into memory, usually since it started, and keeps no time for each.

id: lock-escalations:shop.orders
title: Row locks on orders escalated to a table lock 4 times since Oct 11, 06:13 UTC
summary: SQL Server replaced the row and page locks on orders with one lock on the whole table 4 times since the previous scan, on Oct 11 at 06:13 UTC. SQL Server restarted since then (on Oct 11 at 08:13 UTC), and these are all it counted since. SQL Server can attempt escalation around 5,000 locks on one table reference, or under lock-memory pressure. A successful table lock can block incompatible lock requests until the transaction ends; the escalation counter alone does not establish that other sessions were blocked. SQL Server doesn't record which statement it was, and none of the statements in the query history writes to it: look at jobs, migrations and maintenance that change many rows at once.
impact: 4 new table locks
metric: Escalations since SQL Server started
metric: Escalations since the previous scan
fix: None of the statements in the query history changes thousands of rows of orders at once, so the one that escalated is likely a job, a migration or maintenance. Once the event session below has caught it, change it into smaller batches, for example up to 1,000 changed rows per statement, after checking its access path and lock footprint. A row limit does not bound all locks acquired or prevent escalation under lock-memory pressure. Use smaller transactions only where partial commits preserve the application's required atomicity. An UPDATE loop's filter must leave out rows already changed. A single enclosing transaction can retain locks until commit, so batching is no guarantee against a table lock. Start the event session below (culprit_lock_escalation): it keeps each escalation with its statement, in memory, at almost no cost.
sql: -- SQL Server doesn't record which statement escalated: this session keeps the next ones with their statement.
-- It catches them on every table, so another table's finding starts this same session: once is enough.
IF NOT EXISTS (SELECT 1 FROM sys.server_event_sessions WHERE name = N'culprit_lock_escalation')
    CREATE EVENT SESSION culprit_lock_escalation ON SERVER
    ADD EVENT sqlserver.lock_escalation (
        SET collect_statement = 1
        ACTION (sqlserver.sql_text, sqlserver.client_app_name)
        WHERE sqlserver.database_name = N'culprit_demo')
    ADD TARGET package0.ring_buffer
    WITH (STARTUP_STATE = ON);
IF NOT EXISTS (SELECT 1 FROM sys.dm_xe_sessions WHERE name = N'culprit_lock_escalation')
    ALTER EVENT SESSION culprit_lock_escalation ON SERVER STATE = START;

-- Then read what it caught
SELECT e.x.value('@timestamp', 'datetime2') AS at_utc,
       OBJECT_NAME(e.x.value('(data[@name="object_id"]/value)[1]', 'int')) AS table_name,
       e.x.value('(data[@name="escalated_lock_count"]/value)[1]', 'int') AS locks,
       e.x.value('(data[@name="statement"]/value)[1]', 'nvarchar(max)') AS statement,
       e.x.value('(action[@name="client_app_name"]/value)[1]', 'nvarchar(256)') AS app
  FROM (SELECT CAST(t.target_data AS xml) AS d
          FROM sys.dm_xe_session_targets t
          JOIN sys.dm_xe_sessions s ON s.address = t.event_session_address
         WHERE s.name = N'culprit_lock_escalation' AND t.target_name = N'ring_buffer') r
 CROSS APPLY r.d.nodes('RingBufferTarget/event') e(x);

-- Once it has caught what you need, stop and remove it: DROP EVENT SESSION culprit_lock_escalation ON SERVER;
verify: Compare new escalations on orders and observed blocking under comparable workload before and after a justified change. No new events means none were recorded in that interval; an idle interval alone does not verify a fix.
caveat: SQL Server counts escalations since the table's indexes were loaded into memory, usually since it started, and keeps no time for each.

id: lock-escalations:shop.customers
title: Row locks on customers escalated to a table lock 3 times since SQL Server started (Sep 11), none since Oct 5, 06:13 UTC
summary: These escalations are history, not something happening now: SQL Server counts them from when it started, on Sep 11 at 12:13 UTC, and the count on customers hasn't grown since the previous scan, on Oct 5 at 06:13 UTC. It doesn't record when each one happened. SQL Server can attempt escalation around 5,000 locks on one table reference, or under lock-memory pressure. A successful table lock can block incompatible lock requests until the transaction ends; the escalation counter alone does not establish that other sessions were blocked. The writes to it in the query history are listed below for reference; none of them has escalated since the previous scan.
impact: 3 table locks, none new
metric: Escalations since SQL Server started
metric: Escalations since the previous scan
note: Changes about 1 row a run.
fix: If a statement still changes thousands of rows of customers at once, change it into smaller batches, for example up to 1,000 changed rows per statement, after checking its access path and lock footprint. A row limit does not bound all locks acquired or prevent escalation under lock-memory pressure. Use smaller transactions only where partial commits preserve the application's required atomicity. An UPDATE loop's filter must leave out rows already changed. A single enclosing transaction can retain locks until commit, so batching is no guarantee against a table lock. To learn which statement it is, start the event session below (culprit_lock_escalation): it keeps each escalation with its statement, in memory, at almost no cost.
sql: -- SQL Server doesn't record which statement escalated: this session keeps the next ones with their statement.
-- It catches them on every table, so another table's finding starts this same session: once is enough.
IF NOT EXISTS (SELECT 1 FROM sys.server_event_sessions WHERE name = N'culprit_lock_escalation')
    CREATE EVENT SESSION culprit_lock_escalation ON SERVER
    ADD EVENT sqlserver.lock_escalation (
        SET collect_statement = 1
        ACTION (sqlserver.sql_text, sqlserver.client_app_name)
        WHERE sqlserver.database_name = N'culprit_demo')
    ADD TARGET package0.ring_buffer
    WITH (STARTUP_STATE = ON);
IF NOT EXISTS (SELECT 1 FROM sys.dm_xe_sessions WHERE name = N'culprit_lock_escalation')
    ALTER EVENT SESSION culprit_lock_escalation ON SERVER STATE = START;

-- Then read what it caught
SELECT e.x.value('@timestamp', 'datetime2') AS at_utc,
       OBJECT_NAME(e.x.value('(data[@name="object_id"]/value)[1]', 'int')) AS table_name,
       e.x.value('(data[@name="escalated_lock_count"]/value)[1]', 'int') AS locks,
       e.x.value('(data[@name="statement"]/value)[1]', 'nvarchar(max)') AS statement,
       e.x.value('(action[@name="client_app_name"]/value)[1]', 'nvarchar(256)') AS app
  FROM (SELECT CAST(t.target_data AS xml) AS d
          FROM sys.dm_xe_session_targets t
          JOIN sys.dm_xe_sessions s ON s.address = t.event_session_address
         WHERE s.name = N'culprit_lock_escalation' AND t.target_name = N'ring_buffer') r
 CROSS APPLY r.d.nodes('RingBufferTarget/event') e(x);

-- Once it has caught what you need, stop and remove it: DROP EVENT SESSION culprit_lock_escalation ON SERVER;
verify: Compare new escalations on customers and observed blocking under comparable workload before and after a justified change. No new events means none were recorded in that interval; an idle interval alone does not verify a fix.
caveat: SQL Server counts escalations since the table's indexes were loaded into memory, usually since it started, and keeps no time for each.

id: lock-escalations:shop.customers
title: Row locks on customers escalated to a table lock 3 times since SQL Server started (Sep 11), none since Oct 5, 06:13 UTC
summary: These escalations are history, not something happening now: SQL Server counts them from when it started, on Sep 11 at 12:13 UTC, and the count on customers hasn't grown since the previous scan, on Oct 5 at 06:13 UTC. It doesn't record when each one happened. SQL Server can attempt escalation around 5,000 locks on one table reference, or under lock-memory pressure. A successful table lock can block incompatible lock requests until the transaction ends; the escalation counter alone does not establish that other sessions were blocked. None of the statements in the query history writes to it now.
impact: 3 table locks, none new
metric: Escalations since SQL Server started
metric: Escalations since the previous scan
fix: If a statement still changes thousands of rows of customers at once, change it into smaller batches, for example up to 1,000 changed rows per statement, after checking its access path and lock footprint. A row limit does not bound all locks acquired or prevent escalation under lock-memory pressure. Use smaller transactions only where partial commits preserve the application's required atomicity. An UPDATE loop's filter must leave out rows already changed. A single enclosing transaction can retain locks until commit, so batching is no guarantee against a table lock. To learn which statement it is, start the event session below (culprit_lock_escalation): it keeps each escalation with its statement, in memory, at almost no cost.
sql: -- SQL Server doesn't record which statement escalated: this session keeps the next ones with their statement.
-- It catches them on every table, so another table's finding starts this same session: once is enough.
IF NOT EXISTS (SELECT 1 FROM sys.server_event_sessions WHERE name = N'culprit_lock_escalation')
    CREATE EVENT SESSION culprit_lock_escalation ON SERVER
    ADD EVENT sqlserver.lock_escalation (
        SET collect_statement = 1
        ACTION (sqlserver.sql_text, sqlserver.client_app_name)
        WHERE sqlserver.database_name = N'culprit_demo')
    ADD TARGET package0.ring_buffer
    WITH (STARTUP_STATE = ON);
IF NOT EXISTS (SELECT 1 FROM sys.dm_xe_sessions WHERE name = N'culprit_lock_escalation')
    ALTER EVENT SESSION culprit_lock_escalation ON SERVER STATE = START;

-- Then read what it caught
SELECT e.x.value('@timestamp', 'datetime2') AS at_utc,
       OBJECT_NAME(e.x.value('(data[@name="object_id"]/value)[1]', 'int')) AS table_name,
       e.x.value('(data[@name="escalated_lock_count"]/value)[1]', 'int') AS locks,
       e.x.value('(data[@name="statement"]/value)[1]', 'nvarchar(max)') AS statement,
       e.x.value('(action[@name="client_app_name"]/value)[1]', 'nvarchar(256)') AS app
  FROM (SELECT CAST(t.target_data AS xml) AS d
          FROM sys.dm_xe_session_targets t
          JOIN sys.dm_xe_sessions s ON s.address = t.event_session_address
         WHERE s.name = N'culprit_lock_escalation' AND t.target_name = N'ring_buffer') r
 CROSS APPLY r.d.nodes('RingBufferTarget/event') e(x);

-- Once it has caught what you need, stop and remove it: DROP EVENT SESSION culprit_lock_escalation ON SERVER;
verify: Compare new escalations on customers and observed blocking under comparable workload before and after a justified change. No new events means none were recorded in that interval; an idle interval alone does not verify a fix.
caveat: SQL Server counts escalations since the table's indexes were loaded into memory, usually since it started, and keeps no time for each.

id: lock-escalations:shop.products
title: Row locks on products escalated to a table lock 3 times since Sep 29, 00:13 UTC
summary: SQL Server replaced the row and page locks on products with one lock on the whole table 3 times since the previous scan, on Sep 29 at 00:13 UTC. It counted 4 in all since SQL Server started (Sep 11). SQL Server can attempt escalation around 5,000 locks on one table reference, or under lock-memory pressure. A successful table lock can block incompatible lock requests until the transaction ends; the escalation counter alone does not establish that other sessions were blocked. SQL Server doesn't record which statement it was, and none of the statements in the query history writes to it: look at jobs, migrations and maintenance that change many rows at once.
impact: 3 new table locks
metric: Escalations since SQL Server started
metric: Escalations since the previous scan
fix: None of the statements in the query history changes thousands of rows of products at once, so the one that escalated is likely a job, a migration or maintenance. Once the event session below has caught it, change it into smaller batches, for example up to 1,000 changed rows per statement, after checking its access path and lock footprint. A row limit does not bound all locks acquired or prevent escalation under lock-memory pressure. Use smaller transactions only where partial commits preserve the application's required atomicity. An UPDATE loop's filter must leave out rows already changed. A single enclosing transaction can retain locks until commit, so batching is no guarantee against a table lock. Start the event session below (culprit_lock_escalation): it keeps each escalation with its statement, in memory, at almost no cost.
sql: -- SQL Server doesn't record which statement escalated: this session keeps the next ones with their statement.
-- It catches them on every table, so another table's finding starts this same session: once is enough.
IF NOT EXISTS (SELECT 1 FROM sys.server_event_sessions WHERE name = N'culprit_lock_escalation')
    CREATE EVENT SESSION culprit_lock_escalation ON SERVER
    ADD EVENT sqlserver.lock_escalation (
        SET collect_statement = 1
        ACTION (sqlserver.sql_text, sqlserver.client_app_name)
        WHERE sqlserver.database_name = N'culprit_demo')
    ADD TARGET package0.ring_buffer
    WITH (STARTUP_STATE = ON);
IF NOT EXISTS (SELECT 1 FROM sys.dm_xe_sessions WHERE name = N'culprit_lock_escalation')
    ALTER EVENT SESSION culprit_lock_escalation ON SERVER STATE = START;

-- Then read what it caught
SELECT e.x.value('@timestamp', 'datetime2') AS at_utc,
       OBJECT_NAME(e.x.value('(data[@name="object_id"]/value)[1]', 'int')) AS table_name,
       e.x.value('(data[@name="escalated_lock_count"]/value)[1]', 'int') AS locks,
       e.x.value('(data[@name="statement"]/value)[1]', 'nvarchar(max)') AS statement,
       e.x.value('(action[@name="client_app_name"]/value)[1]', 'nvarchar(256)') AS app
  FROM (SELECT CAST(t.target_data AS xml) AS d
          FROM sys.dm_xe_session_targets t
          JOIN sys.dm_xe_sessions s ON s.address = t.event_session_address
         WHERE s.name = N'culprit_lock_escalation' AND t.target_name = N'ring_buffer') r
 CROSS APPLY r.d.nodes('RingBufferTarget/event') e(x);

-- Once it has caught what you need, stop and remove it: DROP EVENT SESSION culprit_lock_escalation ON SERVER;
verify: Compare new escalations on products and observed blocking under comparable workload before and after a justified change. No new events means none were recorded in that interval; an idle interval alone does not verify a fix.
caveat: These totals do not establish a new increase or continuing blocking in the selected window. Capture dated events or compare counters wholly within that period before treating this as current pressure.
caveat: SQL Server counts escalations since the table's indexes were loaded into memory, usually since it started, and keeps no time for each."#,
    );
}

#[test]
fn lock_escalations_of_reads_with_row_versioning_on() {
    let mut s = escalated("customers", 2.0, 0.0, Some("2026-09-11T12:13:47Z"));
    set(&mut s, "is_read_committed_snapshot_on", "1");
    ran(&mut s, "91", "SELECT * FROM shop.customers WHERE region = @P1", 50.0, 3_000.0);
    says(
        &run("lock-escalations", &s),
        r#"id: lock-escalations:shop.customers
title: Row locks on customers escalated to a table lock 2 times since SQL Server started (Sep 11)
summary: SQL Server replaced the row and page locks on customers with one lock on the whole table 2 times since it started, on Sep 11 at 12:13 UTC. It keeps no time for each, and Culprit has no earlier scan to compare with, so they may be old: the next scan compares with this one and says whether any happened since. That is less than once a week on average since then: they may all be old. SQL Server can attempt escalation around 5,000 locks on one table reference, or under lock-memory pressure. A successful table lock can block incompatible lock requests until the transaction ends; the escalation counter alone does not establish that other sessions were blocked. SQL Server doesn't record which statement it was, and none of the statements in the query history writes to customers. With READ_COMMITTED_SNAPSHOT on, ordinary reads under read committed avoid row locks. Locking hints (HOLDLOCK, UPDLOCK) or stronger isolation can still retain row locks until the transaction ends. The reads of it are listed below, most rows a run first; the one to look for reads thousands of rows in one run while it keeps its locks.
impact: 2 table locks
metric: Escalations since SQL Server started
note: Returns about 3.0K rows a run.
note: Returns about 1 row a run.
fix: Start the event session below (culprit_lock_escalation) to learn which statement escalates: it keeps each escalation with its statement, in memory, at almost no cost. If it is a read, it escalates because it keeps its row locks until it ends: consider reading fewer rows per run after checking the access path; row counts do not bound all locks or prevent escalation under lock-memory pressure. If it is a statement outside the query history that changes many rows at once (a job, a migration, maintenance), consider smaller batches. Use separate transactions only if partial commits preserve required atomicity; an enclosing transaction may retain locks.
sql: -- SQL Server doesn't record which statement escalated: this session keeps the next ones with their statement.
-- It catches them on every table, so another table's finding starts this same session: once is enough.
IF NOT EXISTS (SELECT 1 FROM sys.server_event_sessions WHERE name = N'culprit_lock_escalation')
    CREATE EVENT SESSION culprit_lock_escalation ON SERVER
    ADD EVENT sqlserver.lock_escalation (
        SET collect_statement = 1
        ACTION (sqlserver.sql_text, sqlserver.client_app_name)
        WHERE sqlserver.database_name = N'culprit_demo')
    ADD TARGET package0.ring_buffer
    WITH (STARTUP_STATE = ON);
IF NOT EXISTS (SELECT 1 FROM sys.dm_xe_sessions WHERE name = N'culprit_lock_escalation')
    ALTER EVENT SESSION culprit_lock_escalation ON SERVER STATE = START;

-- Then read what it caught
SELECT e.x.value('@timestamp', 'datetime2') AS at_utc,
       OBJECT_NAME(e.x.value('(data[@name="object_id"]/value)[1]', 'int')) AS table_name,
       e.x.value('(data[@name="escalated_lock_count"]/value)[1]', 'int') AS locks,
       e.x.value('(data[@name="statement"]/value)[1]', 'nvarchar(max)') AS statement,
       e.x.value('(action[@name="client_app_name"]/value)[1]', 'nvarchar(256)') AS app
  FROM (SELECT CAST(t.target_data AS xml) AS d
          FROM sys.dm_xe_session_targets t
          JOIN sys.dm_xe_sessions s ON s.address = t.event_session_address
         WHERE s.name = N'culprit_lock_escalation' AND t.target_name = N'ring_buffer') r
 CROSS APPLY r.d.nodes('RingBufferTarget/event') e(x);

-- Once it has caught what you need, stop and remove it: DROP EVENT SESSION culprit_lock_escalation ON SERVER;
verify: Compare new escalations on customers and observed blocking under comparable workload before and after a justified change. No new events means none were recorded in that interval; an idle interval alone does not verify a fix.
caveat: These totals do not establish a new increase or continuing blocking in the selected window. Capture dated events or compare counters wholly within that period before treating this as current pressure.
caveat: SQL Server counts escalations since the table's indexes were loaded into memory, usually since it started, and keeps no time for each."#,
    );
}
