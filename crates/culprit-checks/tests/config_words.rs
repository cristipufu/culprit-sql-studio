//! What the config check says, word for word, per engine and per setting: its messages come from locales/en.json.

use crate::pins::{fixture, info, pin, pin_text, run, set};
use culprit_core::{
    Deadlock, DeadlockFix, DeadlockReport, Platform, Pressure, Signal, SignalScope, SignalUnit, Snapshot, WaitStat,
};

fn config(s: &Snapshot) -> Vec<culprit_core::Finding> {
    run("config", s)
}

/// The findings `config` makes of `s` whose id is `id`.
fn only(s: &Snapshot, id: &str) -> Vec<culprit_core::Finding> {
    config(s).into_iter().filter(|f| f.id == id).collect()
}

// ── SQL Server ───────────────────────────────────────────────────────────────────────────────────────────────────

#[test]
fn sql_server_settings_say_what_they_cost() {
    pin_text(
        "config_info",
        &info("config"),
        r#"title: Configuration
description: Database and server settings that commonly hurt performance or hide the data needed to diagnose it."#,
    );
    // The demo as it is: capture mode ALL, the 1990s cost threshold, read committed snapshot off.
    pin(
        "config_mssql_demo",
        &config(&fixture("mssql-demo.json")),
        r#"id: config:read-committed-snapshot · Low · Medium · score 6.4 · share None
title: READ_COMMITTED_SNAPSHOT is off: reads would wait for writes on the same rows
summary: With row versioning off for read committed, a SELECT waits for any write holding a lock on the rows it reads, and vice versa. In the last 5 hours no read waited on locks, so it costs nothing now. It matters more as reads and writes meet on the same rows more often. Most OLTP applications run with it on; Azure SQL Database enables it by default.
impact: Setting
fix: Turn on row versioning for read committed. It needs a moment with no other connections, so schedule it.
sql: ALTER DATABASE [culprit_demo] SET READ_COMMITTED_SNAPSHOT ON WITH ROLLBACK IMMEDIATE;
cost: Row versions live in tempdb (or the database with ADR), which adds some write overhead.
verify: Re-scan once the change has taken effect: this finding is gone.
caveat: Readers then see the last committed version instead of waiting. Code that relies on blocking reads for correctness (rare) must use locking hints.

id: config:cost-threshold-for-parallelism · Low · Medium · score 6.4 · share None
title: Cost threshold for parallelism is still the 1990s default of 5
summary: Even cheap queries go parallel, which wastes CPU on coordination and shows up as CXPACKET/CXCONSUMER waits.
impact: Setting
fix: A common starting point is 50; adjust with your workload. Set cost threshold for parallelism to 50.
sql: EXEC sp_configure 'show advanced options', 1;
RECONFIGURE;
EXEC sp_configure 'cost threshold for parallelism', 50;
RECONFIGURE;
verify: Re-scan once the change has taken effect: this finding is gone.

id: config:query-store-capture · Low · Medium · score 6.4 · share None
title: Query Store records every statement, including one-off ones
summary: Capture mode ALL keeps statements that ran once or cost nothing. With 7 statements in the last 5 hours it costs little now; once one-off statements arrive (ad-hoc queries, generated SQL), they crowd the store, shorten how far back it can look and add overhead. AUTO keeps what matters.
impact: Setting
fix: Switch to automatic capture.
sql: ALTER DATABASE [culprit_demo] SET QUERY_STORE (QUERY_CAPTURE_MODE = AUTO);
verify: Re-scan once the change has taken effect: this finding is gone."#,
    );

    // Everything else off or behind, on a server you manage.
    let mut s = fixture("mssql-demo.json");
    set(&mut s, "query_store_options.actual_state_desc", "OFF");
    set(&mut s, "is_auto_create_stats_on", "0");
    set(&mut s, "is_auto_update_stats_on", "0");
    set(&mut s, "compatibility_level", "130");
    set(&mut s, "LEGACY_CARDINALITY_ESTIMATION", "1");
    s.pressure = Some(Pressure { cpus: Some(24.0), numa_nodes: Some(2.0), ..Pressure::default() });
    pin(
        "config_mssql_off",
        &config(&s),
        r#"id: config:query-store · Medium · Medium · score 17.6 · share None
title: Query Store is off, so query history is lost on every restart
summary: Without Query Store, Culprit (and you) only see what is still in the plan cache: no history, no plan regressions, no per-query waits. It costs little and is on by default in Azure SQL and SQL Server 2022.
impact: Setting
fix: Turn it on with automatic capture so ad-hoc noise isn't recorded.
sql: ALTER DATABASE [culprit_demo] SET QUERY_STORE = ON (OPERATION_MODE = READ_WRITE, QUERY_CAPTURE_MODE = AUTO);
cost: Uses some space in the database (default cap 1 GB) and a little CPU.
verify: Re-scan once the change has taken effect: this finding is gone.

id: config:read-committed-snapshot · Low · Medium · score 6.4 · share None
title: READ_COMMITTED_SNAPSHOT is off: reads would wait for writes on the same rows
summary: With row versioning off for read committed, a SELECT waits for any write holding a lock on the rows it reads, and vice versa. In the last 5 hours no read waited on locks, so it costs nothing now. It matters more as reads and writes meet on the same rows more often. Most OLTP applications run with it on; Azure SQL Database enables it by default.
impact: Setting
fix: Turn on row versioning for read committed. It needs a moment with no other connections, so schedule it.
sql: ALTER DATABASE [culprit_demo] SET READ_COMMITTED_SNAPSHOT ON WITH ROLLBACK IMMEDIATE;
cost: Row versions live in tempdb (or the database with ADR), which adds some write overhead.
verify: Re-scan once the change has taken effect: this finding is gone.
caveat: Readers then see the last committed version instead of waiting. Code that relies on blocking reads for correctness (rare) must use locking hints.

id: config:auto_create_statistics · Medium · Medium · score 17.6 · share None
title: AUTO_CREATE_STATISTICS is off
summary: The optimizer relies on statistics to estimate row counts. With this off, estimates go stale or missing and plans degrade over time.
impact: Setting
fix: Turn it back on.
sql: ALTER DATABASE [culprit_demo] SET AUTO_CREATE_STATISTICS ON;
verify: Re-scan once the change has taken effect: this finding is gone.
caveat: Some products require it off and maintain statistics themselves (SharePoint content databases do): check the vendor's guidance before turning it on.

id: config:auto_update_statistics · High · Medium · score 36 · share None
title: AUTO_UPDATE_STATISTICS is off
summary: The optimizer relies on statistics to estimate row counts. With this off, estimates go stale or missing and plans degrade over time.
impact: Setting
fix: Turn it back on.
sql: ALTER DATABASE [culprit_demo] SET AUTO_UPDATE_STATISTICS ON;
verify: Re-scan once the change has taken effect: this finding is gone.

id: config:compatibility-level · Low · Low · score 6.4 · share None
title: The database runs at compatibility level 130
summary: The server supports level 160, but the optimizer behaves as it did at 130. Newer levels bring adaptive joins, memory grant feedback and parameter-sensitive plans.
impact: Setting
metric: Current level: 130
fix: Raise it after testing; Query Store makes this safe because you can force the old plans for any query that regresses.
sql: ALTER DATABASE [culprit_demo] SET COMPATIBILITY_LEVEL = 160;
verify: Re-scan once the change has taken effect: this finding is gone.

id: config:cost-threshold-for-parallelism · Low · Medium · score 6.4 · share None
title: Cost threshold for parallelism is still the 1990s default of 5
summary: Even cheap queries go parallel, which wastes CPU on coordination and shows up as CXPACKET/CXCONSUMER waits.
impact: Setting
fix: A common starting point is 50; adjust with your workload. Set cost threshold for parallelism to 50.
sql: EXEC sp_configure 'show advanced options', 1;
RECONFIGURE;
EXEC sp_configure 'cost threshold for parallelism', 50;
RECONFIGURE;
verify: Re-scan once the change has taken effect: this finding is gone.

id: config:legacy-cardinality-estimation · Low · Low · score 6.4 · share None
title: The database estimates row counts the pre-2014 way
summary: LEGACY_CARDINALITY_ESTIMATION is on, so the optimizer uses the old estimator regardless of the compatibility level. It was often set as a quick fix after an upgrade and then forgotten.
impact: Setting
fix: Turn it off after testing, and use Query Store to force the old plan for any query that regresses (or a per-query hint).
sql: ALTER DATABASE SCOPED CONFIGURATION SET LEGACY_CARDINALITY_ESTIMATION = OFF;
verify: Re-scan once the change has taken effect: this finding is gone.

id: config:maxdop · Low · Low · score 6.4 · share None
title: One query may use all 24 processors (MAXDOP 0)
summary: With no limit on parallelism, a single large query can occupy all 24 schedulers and make everything else queue behind it. Microsoft's guidance is at most 8, and no more than the processors in one NUMA node (12 here).
impact: Setting
metric: Processors: 24 Count
fix: Cap it for this database (or for the whole server).
sql: ALTER DATABASE SCOPED CONFIGURATION SET MAXDOP = 8;
verify: Re-scan once the change has taken effect: this finding is gone."#,
    );

    // Query Store on, nearly full, recording no waits, on a busy database; one NUMA node of twelve processors.
    let mut s = fixture("mssql-demo.json");
    set(&mut s, "query_store_options.current_storage_size_mb", "950");
    set(&mut s, "query_store_options.size_based_cleanup_mode_desc", "OFF");
    set(&mut s, "query_store_options.wait_stats_capture_mode_desc", "OFF");
    s.workload.as_mut().unwrap().query_count = 6000.0;
    s.pressure = Some(Pressure { cpus: Some(12.0), numa_nodes: Some(1.0), ..Pressure::default() });
    pin(
        "config_mssql_query_store",
        &config(&s),
        r#"id: config:read-committed-snapshot · Low · Medium · score 6.4 · share None
title: READ_COMMITTED_SNAPSHOT is off: reads would wait for writes on the same rows
summary: With row versioning off for read committed, a SELECT waits for any write holding a lock on the rows it reads, and vice versa. In the last 5 hours no read waited on locks, so it costs nothing now. It matters more as reads and writes meet on the same rows more often. Most OLTP applications run with it on; Azure SQL Database enables it by default.
impact: Setting
fix: Turn on row versioning for read committed. It needs a moment with no other connections, so schedule it.
sql: ALTER DATABASE [culprit_demo] SET READ_COMMITTED_SNAPSHOT ON WITH ROLLBACK IMMEDIATE;
cost: Row versions live in tempdb (or the database with ADR), which adds some write overhead.
verify: Re-scan once the change has taken effect: this finding is gone.
caveat: Readers then see the last committed version instead of waiting. Code that relies on blocking reads for correctness (rare) must use locking hints.

id: config:cost-threshold-for-parallelism · Low · Medium · score 6.4 · share None
title: Cost threshold for parallelism is still the 1990s default of 5
summary: Even cheap queries go parallel, which wastes CPU on coordination and shows up as CXPACKET/CXCONSUMER waits.
impact: Setting
fix: A common starting point is 50; adjust with your workload. Set cost threshold for parallelism to 50.
sql: EXEC sp_configure 'show advanced options', 1;
RECONFIGURE;
EXEC sp_configure 'cost threshold for parallelism', 50;
RECONFIGURE;
verify: Re-scan once the change has taken effect: this finding is gone.

id: config:query-store-size · Medium · High · score 17.6 · share None
title: Query Store is 95% full and stops recording when it fills
summary: It holds 950 MB of its 1000 MB limit. At the limit it turns read-only: new queries, plan changes and regressions stop being captured until someone makes room.
impact: Setting
metric: Used: 0.95 Ratio
fix: Raise the limit and let it clean up old data on its own.
sql: ALTER DATABASE [culprit_demo] SET QUERY_STORE (MAX_STORAGE_SIZE_MB = 2048, SIZE_BASED_CLEANUP_MODE = AUTO);
cost: Space in the database itself.
verify: Re-scan once the change has taken effect: this finding is gone.

id: config:query-store-capture · Medium · Medium · score 17.6 · share None
title: Query Store records every statement, including one-off ones
summary: Capture mode ALL keeps statements that ran once or cost nothing (6.0K statements in the last 5 hours). They crowd the store, shorten how far back it can look and add overhead; AUTO keeps what matters.
impact: Setting
fix: Switch to automatic capture.
sql: ALTER DATABASE [culprit_demo] SET QUERY_STORE (QUERY_CAPTURE_MODE = AUTO);
verify: Re-scan once the change has taken effect: this finding is gone.

id: config:query-store-waits · Low · Medium · score 6.4 · share None
title: Query Store isn't recording waits, so waits can't be tied to queries
summary: With wait capture off, Culprit can say the server waits on locks or disk but not which queries do the waiting.
impact: Setting
fix: Turn wait capture on; the overhead is small.
sql: ALTER DATABASE [culprit_demo] SET QUERY_STORE (WAIT_STATS_CAPTURE_MODE = ON);
verify: Re-scan once the change has taken effect: this finding is gone.

id: config:maxdop · Low · Low · score 6.4 · share None
title: One query may use all 12 processors (MAXDOP 0)
summary: With no limit on parallelism, a single large query can occupy all 12 schedulers and make everything else queue behind it. Microsoft's guidance is at most 8, and no more than the processors in one NUMA node.
impact: Setting
metric: Processors: 12 Count
fix: Cap it for this database (or for the whole server).
sql: ALTER DATABASE SCOPED CONFIGURATION SET MAXDOP = 8;
verify: Re-scan once the change has taken effect: this finding is gone."#,
    );

    // Azure SQL Database: its level comes from the service; capture mode ALL with one statement, and with none.
    let mut s = fixture("mssql-demo.json");
    s.server.set_platform(Some(Platform::AzureSqlDatabase));
    s.server.version = "12.0.2000.8".into();
    set(&mut s, "compatibility_level", "130");
    s.workload.as_mut().unwrap().query_count = 1.0;
    let mut all = only(&s, "config:compatibility-level");
    all.extend(only(&s, "config:query-store-capture"));
    s.workload.as_mut().unwrap().query_count = 0.0;
    all.extend(only(&s, "config:query-store-capture"));
    pin(
        "config_mssql_azure",
        &all,
        r#"id: config:compatibility-level · Low · Low · score 6.4 · share None
title: The database runs at compatibility level 130
summary: Azure SQL Database supports level 160, but the optimizer behaves as it did at 130. Newer levels bring adaptive joins, memory grant feedback and parameter-sensitive plans.
impact: Setting
metric: Current level: 130
fix: Raise it after testing; Query Store makes this safe because you can force the old plans for any query that regresses.
sql: ALTER DATABASE [culprit_demo] SET COMPATIBILITY_LEVEL = 160;
verify: Re-scan once the change has taken effect: this finding is gone.

id: config:query-store-capture · Low · Medium · score 6.4 · share None
title: Query Store records every statement, including one-off ones
summary: Capture mode ALL keeps statements that ran once or cost nothing. With 1 statement in the last 5 hours it costs little now; once one-off statements arrive (ad-hoc queries, generated SQL), they crowd the store, shorten how far back it can look and add overhead. AUTO keeps what matters.
impact: Setting
fix: Switch to automatic capture.
sql: ALTER DATABASE [culprit_demo] SET QUERY_STORE (QUERY_CAPTURE_MODE = AUTO);
verify: Re-scan once the change has taken effect: this finding is gone.

id: config:query-store-capture · Low · Medium · score 6.4 · share None
title: Query Store records every statement, including one-off ones
summary: Capture mode ALL keeps statements that ran once or cost nothing. On a busy database they crowd the store, shorten how far back it can look and add overhead; AUTO keeps what matters.
impact: Setting
fix: Switch to automatic capture.
sql: ALTER DATABASE [culprit_demo] SET QUERY_STORE (QUERY_CAPTURE_MODE = AUTO);
verify: Re-scan once the change has taken effect: this finding is gone."#,
    );

    // RDS: server options in its parameter group; an old version with no per-database MAXDOP.
    let mut s = fixture("mssql-demo.json");
    s.server.set_platform(Some(Platform::AmazonRds));
    s.server.version = "12.0.6024.0".into();
    s.pressure = Some(Pressure { cpus: Some(16.0), numa_nodes: Some(1.0), ..Pressure::default() });
    let mut all = only(&s, "config:cost-threshold-for-parallelism");
    all.extend(only(&s, "config:maxdop"));
    s.server.set_platform(None);
    all.extend(only(&s, "config:maxdop"));
    pin(
        "config_mssql_server_options",
        &all,
        r#"id: config:cost-threshold-for-parallelism · Low · Medium · score 6.4 · share None
title: Cost threshold for parallelism is still the 1990s default of 5
summary: Even cheap queries go parallel, which wastes CPU on coordination and shows up as CXPACKET/CXCONSUMER waits.
impact: Setting
fix: A common starting point is 50; adjust with your workload. Set cost threshold for parallelism to 50 in the instance's DB parameter group.
verify: Re-scan once the change has taken effect: this finding is gone.

id: config:maxdop · Low · Low · score 6.4 · share None
title: One query may use all 16 processors (MAXDOP 0)
summary: With no limit on parallelism, a single large query can occupy all 16 schedulers and make everything else queue behind it. Microsoft's guidance is at most 8, and no more than the processors in one NUMA node.
impact: Setting
metric: Processors: 16 Count
fix: Cap it for the server: this version has no per-database setting (SQL Server 2016 added it). Set max degree of parallelism to 8 in the instance's DB parameter group.
verify: Re-scan once the change has taken effect: this finding is gone.

id: config:maxdop · Low · Low · score 6.4 · share None
title: One query may use all 16 processors (MAXDOP 0)
summary: With no limit on parallelism, a single large query can occupy all 16 schedulers and make everything else queue behind it. Microsoft's guidance is at most 8, and no more than the processors in one NUMA node.
impact: Setting
metric: Processors: 16 Count
fix: Cap it for the server: this version has no per-database setting (SQL Server 2016 added it). Set max degree of parallelism to 8.
sql: EXEC sp_configure 'max degree of parallelism', 8;
RECONFIGURE;
verify: Re-scan once the change has taken effect: this finding is gone."#,
    );
}

#[test]
fn a_read_only_query_store_says_why() {
    let mut all = Vec::new();
    for (reason, max) in [
        ("65536", Some("4096")),
        ("65536", None),
        ("131072", Some("1000")),
        ("524288", Some("1000")),
        ("262144", Some("1000")),
        ("196608", Some("1000")),
        ("16", Some("1000")),
    ] {
        let mut s = fixture("mssql-demo.json");
        set(&mut s, "query_store_options.actual_state_desc", "READ_ONLY");
        set(&mut s, "query_store_options.readonly_reason", reason);
        s.settings.retain(|x| x.name != "query_store_options.max_storage_size_mb");
        if let Some(max) = max {
            set(&mut s, "query_store_options.max_storage_size_mb", max);
        }
        all.extend(only(&s, "config:query-store"));
    }
    pin(
        "config_mssql_query_store_read_only",
        &all,
        r#"id: config:query-store · Medium · Medium · score 17.6 · share None
title: Query Store stopped recording
summary: Query Store is read-only because it reached its size limit (MAX_STORAGE_SIZE_MB). New queries and plan changes aren't being captured.
impact: Setting
fix: Give it more room and let it clean up old data by itself, then turn recording back on.
sql: ALTER DATABASE [culprit_demo] SET QUERY_STORE (MAX_STORAGE_SIZE_MB = 8192, SIZE_BASED_CLEANUP_MODE = AUTO, OPERATION_MODE = READ_WRITE);
verify: Re-scan once the change has taken effect: this finding is gone.

id: config:query-store · Medium · Medium · score 17.6 · share None
title: Query Store stopped recording
summary: Query Store is read-only because it reached its size limit (MAX_STORAGE_SIZE_MB). New queries and plan changes aren't being captured.
impact: Setting
fix: Give it more room and let it clean up old data by itself, then turn recording back on.
sql: ALTER DATABASE [culprit_demo] SET QUERY_STORE (MAX_STORAGE_SIZE_MB = 2048, SIZE_BASED_CLEANUP_MODE = AUTO, OPERATION_MODE = READ_WRITE);
verify: Re-scan once the change has taken effect: this finding is gone.

id: config:query-store · Medium · Medium · score 17.6 · share None
title: Query Store stopped recording
summary: Query Store is read-only because it holds as many different statements as its memory allows. New queries and plan changes aren't being captured.
impact: Setting
fix: Record only statements that matter (automatic capture), then turn recording back on.
sql: ALTER DATABASE [culprit_demo] SET QUERY_STORE (QUERY_CAPTURE_MODE = AUTO, OPERATION_MODE = READ_WRITE);
verify: Re-scan once the change has taken effect: this finding is gone.

id: config:query-store · Medium · Medium · score 17.6 · share None
title: Query Store stopped recording
summary: Query Store is read-only because the database is out of disk space. New queries and plan changes aren't being captured.
impact: Setting
fix: Free or add space in the database's data files, then turn recording back on.
sql: ALTER DATABASE [culprit_demo] SET QUERY_STORE (OPERATION_MODE = READ_WRITE);
verify: Re-scan once the change has taken effect: this finding is gone.

id: config:query-store · Low · Medium · score 6.4 · share None
title: Query Store stopped recording
summary: Query Store is read-only because data waiting to be written to disk reached its memory limit. New queries and plan changes aren't being captured.
impact: Setting
fix: It usually returns to recording once the data is written; if it stays read-only, turn recording back on.
sql: ALTER DATABASE [culprit_demo] SET QUERY_STORE (OPERATION_MODE = READ_WRITE);
verify: Re-scan once the change has taken effect: this finding is gone.

id: config:query-store · Medium · Medium · score 17.6 · share None
title: Query Store stopped recording
summary: Query Store is read-only because it reached its size limit (MAX_STORAGE_SIZE_MB) and it holds as many different statements as its memory allows. New queries and plan changes aren't being captured.
impact: Setting
fix: Give it more room and let it clean up old data by itself, then turn recording back on.
sql: ALTER DATABASE [culprit_demo] SET QUERY_STORE (MAX_STORAGE_SIZE_MB = 2048, SIZE_BASED_CLEANUP_MODE = AUTO, OPERATION_MODE = READ_WRITE);
verify: Re-scan once the change has taken effect: this finding is gone.

id: config:query-store · Low · Medium · score 6.4 · share None
title: Query Store stopped recording
summary: Query Store is read-only because reason code 16. New queries and plan changes aren't being captured.
impact: Setting
fix: It usually returns to recording once the data is written; if it stays read-only, turn recording back on.
sql: ALTER DATABASE [culprit_demo] SET QUERY_STORE (OPERATION_MODE = READ_WRITE);
verify: Re-scan once the change has taken effect: this finding is gone."#,
    );
}

/// The demo with read `id` waiting `ms` on locks, its database time grown by as much.
fn lock_wait(s: &mut Snapshot, id: &str, ms: f64) {
    let w = s.workload.as_mut().unwrap();
    let q = w.queries.iter_mut().find(|q| q.id == id).unwrap();
    q.waits.push(WaitStat { category: "Lock".into(), wait_type: None, wait_ms: ms, tasks: None });
    w.total_time_ms += ms;
}

/// Read `id` ran only between `from` and `to` (and `runs` times).
fn burst(s: &mut Snapshot, id: &str, from: &str, to: &str, runs: Option<f64>) {
    let q = s.workload.as_mut().unwrap().queries.iter_mut().find(|q| q.id == id).unwrap();
    (q.first_seen, q.last_seen, q.trend) = (Some(from.into()), Some(to.into()), None);
    if let Some(runs) = runs {
        q.executions = runs;
    }
}

fn rcsi(s: &Snapshot) -> Vec<culprit_core::Finding> {
    only(s, "config:read-committed-snapshot")
}

#[test]
fn read_committed_snapshot_says_what_reads_waited() {
    let mut all = Vec::new();
    // No statement recorded any wait: nothing says the reads didn't wait. With snapshot isolation allowed already.
    let mut s = fixture("mssql-demo.json");
    for q in &mut s.workload.as_mut().unwrap().queries {
        q.waits.clear();
    }
    set(&mut s, "snapshot_isolation_state_desc", "ON");
    all.extend(rcsi(&s));
    // A little, now and then.
    let mut s = fixture("mssql-demo.json");
    lock_wait(&mut s, "7", 500.0);
    all.extend(rcsi(&s));
    // A little, in one burst of a read that ran once.
    burst(&mut s, "7", "2026-10-11T08:00:00Z", "2026-10-11T08:20:00Z", Some(1.0));
    all.extend(rcsi(&s));
    // A little, from reads that each ran once, spread over the window.
    let mut s = fixture("mssql-demo.json");
    lock_wait(&mut s, "7", 300.0);
    lock_wait(&mut s, "10", 200.0);
    for id in ["7", "10"] {
        s.workload.as_mut().unwrap().queries.iter_mut().find(|q| q.id == id).unwrap().executions = 1.0;
    }
    all.extend(rcsi(&s));
    // Most of it one read's, little time in all.
    let mut s = fixture("mssql-demo.json");
    lock_wait(&mut s, "7", 25_000.0);
    lock_wait(&mut s, "10", 15_000.0);
    all.extend(rcsi(&s));
    // A share of database time, all of it in one burst.
    let mut s = fixture("mssql-demo.json");
    lock_wait(&mut s, "7", 2_000.0);
    burst(&mut s, "7", "2026-10-11T08:00:00Z", "2026-10-11T08:20:00Z", None);
    all.extend(rcsi(&s));
    // Demos MS-12: a minute in six days, nearly all of it one read's: a rate a day.
    let mut s = fixture("mssql-demo.json");
    let w = s.workload.as_mut().unwrap();
    w.window_hours = 146.66;
    let read = w.queries.iter_mut().find(|q| q.text.starts_with("SELECT")).unwrap();
    read.waits.push(WaitStat { category: "Lock".into(), wait_type: None, wait_ms: 71_692.0, tasks: None });
    (read.first_seen, read.last_seen, read.trend) = (None, None, None);
    w.total_time_ms = 335_668.0;
    all.extend(rcsi(&s));
    pin(
        "config_rcsi_low",
        &all,
        r#"id: config:read-committed-snapshot · Low · Medium · score 6.4 · share None
title: READ_COMMITTED_SNAPSHOT is off: reads would wait for writes on the same rows
summary: With row versioning off for read committed, a SELECT waits for any write holding a lock on the rows it reads, and vice versa. Whether reads wait on locks here isn't known: the query history records no waits. It matters more as reads and writes meet on the same rows more often. Most OLTP applications run with it on; Azure SQL Database enables it by default.
impact: Setting
fix: Turn on row versioning for read committed. It needs a moment with no other connections, so schedule it.
sql: ALTER DATABASE [culprit_demo] SET READ_COMMITTED_SNAPSHOT ON WITH ROLLBACK IMMEDIATE;
cost: ALLOW_SNAPSHOT_ISOLATION is already on, so SQL Server already keeps row versions for this database: little extra overhead.
verify: Re-scan once the change has taken effect: this finding is gone.
caveat: Readers then see the last committed version instead of waiting. Code that relies on blocking reads for correctness (rare) must use locking hints.
caveat: ALLOW_SNAPSHOT_ISOLATION is on: code can already opt into SNAPSHOT isolation per transaction; RCSI makes every read-committed read use versions.

id: config:read-committed-snapshot · Low · Medium · score 6.4 · share None
title: READ_COMMITTED_SNAPSHOT is off: reads would wait for writes on the same rows
summary: With row versioning off for read committed, a SELECT waits for any write holding a lock on the rows it reads, and vice versa. In the last 5 hours reads waited 500 ms on locks (1% of database time), so it costs little now. It matters more as reads and writes meet on the same rows more often. Most OLTP applications run with it on; Azure SQL Database enables it by default.
impact: Setting
fix: Turn on row versioning for read committed. It needs a moment with no other connections, so schedule it.
sql: ALTER DATABASE [culprit_demo] SET READ_COMMITTED_SNAPSHOT ON WITH ROLLBACK IMMEDIATE;
cost: Row versions live in tempdb (or the database with ADR), which adds some write overhead.
verify: Re-scan once the change has taken effect: this finding is gone.
caveat: Readers then see the last committed version instead of waiting. Code that relies on blocking reads for correctness (rare) must use locking hints.

id: config:read-committed-snapshot · Low · Medium · score 6.4 · share None
title: READ_COMMITTED_SNAPSHOT is off: reads would wait for writes on the same rows
summary: With row versioning off for read committed, a SELECT waits for any write holding a lock on the rows it reads, and vice versa. In the last 5 hours reads waited 500 ms on locks (1% of database time), all of it on Oct 11, 08:00–08:20 UTC, and that read hasn't run since, so it costs little now. It matters more as reads and writes meet on the same rows more often. Most OLTP applications run with it on; Azure SQL Database enables it by default.
impact: Setting
fix: Turn on row versioning for read committed. It needs a moment with no other connections, so schedule it.
sql: ALTER DATABASE [culprit_demo] SET READ_COMMITTED_SNAPSHOT ON WITH ROLLBACK IMMEDIATE;
cost: Row versions live in tempdb (or the database with ADR), which adds some write overhead.
verify: Re-scan once the change has taken effect: this finding is gone.
caveat: Readers then see the last committed version instead of waiting. Code that relies on blocking reads for correctness (rare) must use locking hints.

id: config:read-committed-snapshot · Low · Medium · score 6.4 · share None
title: READ_COMMITTED_SNAPSHOT is off: reads would wait for writes on the same rows
summary: With row versioning off for read committed, a SELECT waits for any write holding a lock on the rows it reads, and vice versa. In the last 5 hours reads waited 500 ms on locks (1% of database time), and those reads haven't run since, so it costs little now. It matters more as reads and writes meet on the same rows more often. Most OLTP applications run with it on; Azure SQL Database enables it by default.
impact: Setting
fix: Turn on row versioning for read committed. It needs a moment with no other connections, so schedule it.
sql: ALTER DATABASE [culprit_demo] SET READ_COMMITTED_SNAPSHOT ON WITH ROLLBACK IMMEDIATE;
cost: Row versions live in tempdb (or the database with ADR), which adds some write overhead.
verify: Re-scan once the change has taken effect: this finding is gone.
caveat: Readers then see the last committed version instead of waiting. Code that relies on blocking reads for correctness (rare) must use locking hints.

id: config:read-committed-snapshot · Low · Medium · score 6.4 · share None
title: READ_COMMITTED_SNAPSHOT is off: reads would wait for writes on the same rows
summary: With row versioning off for read committed, a SELECT waits for any write holding a lock on the rows it reads, and vice versa. In the last 5 hours reads waited 40.0 s on locks held by writes (most of it a read from customers, 25.0 s): 50% of database time, but little time in all. With row versioning on, those reads would read the last committed version of the rows instead of waiting. It matters more as reads and writes meet on the same rows more often. Most OLTP applications run with it on; Azure SQL Database enables it by default.
impact: Setting
fix: Turn on row versioning for read committed. It needs a moment with no other connections, so schedule it.
sql: ALTER DATABASE [culprit_demo] SET READ_COMMITTED_SNAPSHOT ON WITH ROLLBACK IMMEDIATE;
cost: Row versions live in tempdb (or the database with ADR), which adds some write overhead.
verify: Re-scan once the change has taken effect: this finding is gone.
caveat: Readers then see the last committed version instead of waiting. Code that relies on blocking reads for correctness (rare) must use locking hints.

id: config:read-committed-snapshot · Low · Medium · score 6.4 · share None
title: READ_COMMITTED_SNAPSHOT is off: reads would wait for writes on the same rows
summary: With row versioning off for read committed, a SELECT waits for any write holding a lock on the rows it reads, and vice versa. In the last 5 hours reads waited 2.0 s on locks held by writes (nearly all of it a read from customers): 5% of database time, but all of it on Oct 11, 08:00–08:20 UTC. With row versioning on, those reads would read the last committed version of the rows instead of waiting. It matters more as reads and writes meet on the same rows more often. Most OLTP applications run with it on; Azure SQL Database enables it by default.
impact: Setting
fix: Turn on row versioning for read committed. It needs a moment with no other connections, so schedule it.
sql: ALTER DATABASE [culprit_demo] SET READ_COMMITTED_SNAPSHOT ON WITH ROLLBACK IMMEDIATE;
cost: Row versions live in tempdb (or the database with ADR), which adds some write overhead.
verify: Re-scan once the change has taken effect: this finding is gone.
caveat: Readers then see the last committed version instead of waiting. Code that relies on blocking reads for correctness (rare) must use locking hints.

id: config:read-committed-snapshot · Low · Medium · score 6.4 · share None
title: READ_COMMITTED_SNAPSHOT is off: reads would wait for writes on the same rows
summary: With row versioning off for read committed, a SELECT waits for any write holding a lock on the rows it reads, and vice versa. In the last 6 days reads waited 1 min on locks held by writes (nearly all of it a read from customers): 21% of database time, but only about 11.7 s a day. With row versioning on, those reads would read the last committed version of the rows instead of waiting. It matters more as reads and writes meet on the same rows more often. Most OLTP applications run with it on; Azure SQL Database enables it by default.
impact: Setting
fix: Turn on row versioning for read committed. It needs a moment with no other connections, so schedule it.
sql: ALTER DATABASE [culprit_demo] SET READ_COMMITTED_SNAPSHOT ON WITH ROLLBACK IMMEDIATE;
cost: Row versions live in tempdb (or the database with ADR), which adds some write overhead.
verify: Re-scan once the change has taken effect: this finding is gone.
caveat: Readers then see the last committed version instead of waiting. Code that relies on blocking reads for correctness (rare) must use locking hints."#,
    );
}

fn read_deadlock(at: &str) -> Deadlock {
    let mut d = Deadlock::new("system_health", Some(at.into()), Vec::new(), Vec::new());
    d.fixes = vec![DeadlockFix {
        title: "Let reads stop taking shared locks".into(),
        detail: "Row versions".into(),
        sql: None,
        finding: Some("config:read-committed-snapshot".into()),
    }];
    d
}

#[test]
fn read_committed_snapshot_says_when_reads_and_writes_block() {
    let mut all = Vec::new();
    // Five minutes of reads waiting on writes.
    let mut s = fixture("mssql-demo.json");
    lock_wait(&mut s, "7", 300_000.0);
    all.extend(rcsi(&s));
    // All of it in one burst; and a deadlock with a read in it.
    burst(&mut s, "7", "2026-10-11T08:00:00Z", "2026-10-11T08:20:00Z", None);
    s.deadlocks = Some(DeadlockReport {
        deadlocks: vec![read_deadlock("2026-10-11T09:00:00Z")],
        since_hours: 24.0,
        ..Default::default()
    });
    all.extend(rcsi(&s));
    // Deadlocks alone, with snapshot isolation allowed.
    let mut s = fixture("mssql-demo.json");
    set(&mut s, "snapshot_isolation_state_desc", "ON");
    s.deadlocks = Some(DeadlockReport {
        deadlocks: vec![read_deadlock("2026-10-11T09:00:00Z"), read_deadlock("2026-10-11T10:00:00Z")],
        since_hours: 24.0,
        ..Default::default()
    });
    all.extend(rcsi(&s));
    pin(
        "config_rcsi_blocking",
        &all,
        r#"id: config:read-committed-snapshot · Medium · Medium · score 17.6 · share None
title: Readers and writers block each other (READ_COMMITTED_SNAPSHOT is off)
summary: With row versioning off for read committed, a SELECT waits for any write holding a lock on the rows it reads, and vice versa. In the last 5 hours, reads waited 5 min on locks held by writes (88% of database time). Most OLTP applications run with it on; Azure SQL Database enables it by default.
impact: Setting
metric: Reads waiting on locks: 300000 Ms
fix: Turn on row versioning for read committed. It needs a moment with no other connections, so schedule it.
sql: ALTER DATABASE [culprit_demo] SET READ_COMMITTED_SNAPSHOT ON WITH ROLLBACK IMMEDIATE;
cost: Row versions live in tempdb (or the database with ADR), which adds some write overhead.
verify: Re-scan once the change has taken effect: this finding is gone.
caveat: Readers then see the last committed version instead of waiting. Code that relies on blocking reads for correctness (rare) must use locking hints.

id: config:read-committed-snapshot · Medium · Medium · score 17.6 · share None
title: Readers and writers block each other (READ_COMMITTED_SNAPSHOT is off)
summary: With row versioning off for read committed, a SELECT waits for any write holding a lock on the rows it reads, and vice versa. In the last 5 hours, reads waited 5 min on locks held by writes (88% of database time), all of it on Oct 11, 08:00–08:20 UTC. 1 deadlock in this database had a read in it. Most OLTP applications run with it on; Azure SQL Database enables it by default.
impact: Setting
metric: Reads waiting on locks: 300000 Ms
fix: Turn on row versioning for read committed. It needs a moment with no other connections, so schedule it.
sql: ALTER DATABASE [culprit_demo] SET READ_COMMITTED_SNAPSHOT ON WITH ROLLBACK IMMEDIATE;
cost: Row versions live in tempdb (or the database with ADR), which adds some write overhead.
verify: Re-scan once the change has taken effect: this finding is gone.
caveat: Readers then see the last committed version instead of waiting. Code that relies on blocking reads for correctness (rare) must use locking hints.

id: config:read-committed-snapshot · Medium · Medium · score 17.6 · share None
title: Readers and writers block each other (READ_COMMITTED_SNAPSHOT is off)
summary: With row versioning off for read committed, a SELECT waits for any write holding a lock on the rows it reads, and vice versa. 2 deadlocks in this database had a read in them. Most OLTP applications run with it on; Azure SQL Database enables it by default.
impact: Setting
fix: Turn on row versioning for read committed. It needs a moment with no other connections, so schedule it.
sql: ALTER DATABASE [culprit_demo] SET READ_COMMITTED_SNAPSHOT ON WITH ROLLBACK IMMEDIATE;
cost: ALLOW_SNAPSHOT_ISOLATION is already on, so SQL Server already keeps row versions for this database: little extra overhead.
verify: Re-scan once the change has taken effect: this finding is gone.
caveat: Readers then see the last committed version instead of waiting. Code that relies on blocking reads for correctness (rare) must use locking hints.
caveat: ALLOW_SNAPSHOT_ISOLATION is on: code can already opt into SNAPSHOT isolation per transaction; RCSI makes every read-committed read use versions."#,
    );
}

// ── PostgreSQL ───────────────────────────────────────────────────────────────────────────────────────────────────

#[test]
fn postgres_settings_say_what_they_cost() {
    let mut s = fixture("postgres-demo.json");
    set(&mut s, "track_io_timing", "off");
    set(&mut s, "autovacuum", "off");
    set(&mut s, "pg_stat_statements.track", "none");
    pin(
        "config_postgres_off",
        &config(&s),
        r#"id: config:track_io_timing · Low · Medium · score 4 · share None
title: I/O timing isn't tracked
summary: With track_io_timing off, pg_stat_statements and EXPLAIN (BUFFERS) can't tell how much of a query's time is spent waiting on disk.
impact: Setting
fix: Turn it on; the overhead is negligible on modern systems. Set track_io_timing to on and reload the configuration (settings marked as needing a restart take effect after one).
sql: ALTER SYSTEM SET track_io_timing = 'on';
SELECT pg_reload_conf();
verify: Re-scan once the change has taken effect: this finding is gone.

id: config:autovacuum · High · High · score 36 · share None
title: Autovacuum is off
summary: Dead rows from updates and deletes are never cleaned up automatically, tables and indexes keep growing, planner statistics go stale, and transaction-ID wraparound becomes a real risk.
impact: Setting
fix: Turn it back on; tune it per table instead of disabling it. Set autovacuum to on and reload the configuration (settings marked as needing a restart take effect after one).
sql: ALTER SYSTEM SET autovacuum = 'on';
SELECT pg_reload_conf();
verify: Re-scan once the change has taken effect: this finding is gone.

id: config:pg_stat_statements.track · Medium · Medium · score 17.6 · share None
title: pg_stat_statements is installed but not recording
summary: pg_stat_statements.track is none, so no statement statistics accumulate: Culprit can't see which queries use the time.
impact: Setting
fix: Track top-level statements. Set pg_stat_statements.track to top and reload the configuration (settings marked as needing a restart take effect after one).
sql: ALTER SYSTEM SET pg_stat_statements.track = 'top';
SELECT pg_reload_conf();
verify: Re-scan once the change has taken effect: this finding is gone."#,
    );
    // The same on a managed service.
    s.server.set_platform(Some(Platform::AzureDatabase));
    pin(
        "config_postgres_off_managed",
        &config(&s),
        r#"id: config:track_io_timing · Low · Medium · score 4 · share None
title: I/O timing isn't tracked
summary: With track_io_timing off, pg_stat_statements and EXPLAIN (BUFFERS) can't tell how much of a query's time is spent waiting on disk.
impact: Setting
fix: Turn it on; the overhead is negligible on modern systems. Set track_io_timing to on in the server parameters (Azure portal or `az … server parameter set`).
verify: Re-scan once the change has taken effect: this finding is gone.

id: config:autovacuum · High · High · score 36 · share None
title: Autovacuum is off
summary: Dead rows from updates and deletes are never cleaned up automatically, tables and indexes keep growing, planner statistics go stale, and transaction-ID wraparound becomes a real risk.
impact: Setting
fix: Turn it back on; tune it per table instead of disabling it. Set autovacuum to on in the server parameters (Azure portal or `az … server parameter set`).
verify: Re-scan once the change has taken effect: this finding is gone.

id: config:pg_stat_statements.track · Medium · Medium · score 17.6 · share None
title: pg_stat_statements is installed but not recording
summary: pg_stat_statements.track is none, so no statement statistics accumulate: Culprit can't see which queries use the time.
impact: Setting
fix: Track top-level statements. Set pg_stat_statements.track to top in the server parameters (Azure portal or `az … server parameter set`).
verify: Re-scan once the change has taken effect: this finding is gone."#,
    );
}

#[test]
fn autovacuum_scale_names_the_biggest_table() {
    let mut all = Vec::new();
    for big in [1, 7] {
        let mut s = fixture("postgres-demo.json");
        set(&mut s, "autovacuum_vacuum_scale_factor", "0.2");
        for (i, t) in s.tables.iter_mut().enumerate() {
            t.rows = Some(if i < big { 20_000_000.0 + i as f64 * 1_000_000.0 } else { 1_000.0 });
            (t.updates, t.deletes) = (Some(2_000_000.0), None);
        }
        while s.tables.len() < big {
            let mut t = s.tables[0].clone();
            t.name = format!("{}_{}", t.name, s.tables.len());
            t.rows = Some(30_000_000.0 + s.tables.len() as f64);
            s.tables.push(t);
        }
        all.extend(only(&s, "config:autovacuum-scale"));
    }
    pin(
        "config_postgres_autovacuum_scale",
        &all,
        r#"id: config:autovacuum-scale · Low · Medium · score 6.4 · share None
title: Autovacuum waits for 20% of orders to change before cleaning it
summary: autovacuum_vacuum_scale_factor is 0.2: public.orders (20.0M rows, with updates and deletes) is only vacuumed after about 4.0M rows change, so dead rows pile up between runs and each run is long.
impact: Setting
fix: Lower it for the big tables only.
sql: ALTER TABLE public.orders SET (autovacuum_vacuum_scale_factor = 0.01, autovacuum_analyze_scale_factor = 0.02);
verify: Re-scan: the tables have their own scale factor and this finding is gone.

id: config:autovacuum-scale · Low · Medium · score 6.4 · share None
title: Autovacuum waits for 20% of orders_6 to change before cleaning it
summary: autovacuum_vacuum_scale_factor is 0.2: public.orders_6 (30.0M rows, with updates and deletes) is only vacuumed after about 6.0M rows change, so dead rows pile up between runs and each run is long.
impact: Setting
fix: Lower it for the big tables only.
sql: ALTER TABLE public.orders SET (autovacuum_vacuum_scale_factor = 0.01, autovacuum_analyze_scale_factor = 0.02);
ALTER TABLE public.customers SET (autovacuum_vacuum_scale_factor = 0.01, autovacuum_analyze_scale_factor = 0.02);
ALTER TABLE public.order_items SET (autovacuum_vacuum_scale_factor = 0.01, autovacuum_analyze_scale_factor = 0.02);
ALTER TABLE public.products SET (autovacuum_vacuum_scale_factor = 0.01, autovacuum_analyze_scale_factor = 0.02);
ALTER TABLE public.events SET (autovacuum_vacuum_scale_factor = 0.01, autovacuum_analyze_scale_factor = 0.02);
verify: Re-scan: the tables the SQL changed have their own scale factor and drop out of this finding; the next ones it names need the same change."#,
    );
}

#[test]
fn postgres_memory_settings_say_what_they_hold() {
    let mut all = Vec::new();
    // The 128 MB default beside 40 GB of data, without and with measured I/O time; then a smaller one.
    let mut s = fixture("postgres-demo.json");
    for t in &mut s.tables {
        t.total_bytes = Some(40.0 * 1024.0 * 1024.0 * 1024.0 / 4.0);
    }
    set(&mut s, "shared_buffers", "128MB");
    all.extend(only(&s, "config:shared_buffers"));
    let io = s.workload.as_ref().unwrap().total_time_ms * 0.5;
    s.waits.push(WaitStat { category: "IO".into(), wait_type: None, wait_ms: io, tasks: None });
    s.workload.as_mut().unwrap().total_time_ms += io;
    all.extend(only(&s, "config:shared_buffers"));
    set(&mut s, "shared_buffers", "64MB");
    all.extend(only(&s, "config:shared_buffers"));
    // WAL buffers filled, at their default and set.
    let mut s = fixture("postgres-demo.json");
    s.pressure.get_or_insert_with(Default::default).uptime_hours = Some(24.0);
    s.pressure.as_mut().unwrap().signals.push(Signal::new(
        "wal-buffers-full",
        "WAL buffer full writes",
        30_000.0,
        SignalUnit::Count,
        SignalScope::SinceStartup,
    ));
    s.settings.retain(|x| x.name != "wal_buffers");
    all.extend(only(&s, "config:wal_buffers"));
    set(&mut s, "wal_buffers", "16MB");
    s.server.set_platform(Some(Platform::AmazonRds));
    all.extend(only(&s, "config:wal_buffers"));
    pin(
        "config_postgres_memory",
        &all,
        r#"id: config:shared_buffers · Low · Medium · score 6.4 · share None
title: shared_buffers is still the 128 MB default
summary: PostgreSQL keeps only 128 MB of this database's 50.0 GB in its own cache and leans on the operating system for the rest. Operating-system cache hits can still be fast; this size and the cache-hit ratio alone do not establish a memory shortage.
impact: Setting
fix: Measure this database's read latency and I/O time during normal load before changing shared_buffers. If they confirm substantial I/O, a common starting point is a quarter of the server's memory, with room left for the operating system and query memory.
cost: That much memory is reserved for PostgreSQL.
verify: Re-scan after the server has restarted with the change: this finding is gone.

id: config:shared_buffers · Medium · Medium · score 17.6 · share None
title: shared_buffers is still the 128 MB default
summary: PostgreSQL keeps only 128 MB of this database's 50.0 GB in its own cache and leans on the operating system for the rest. Operating-system cache hits can still be fast; this size and the cache-hit ratio alone do not establish a memory shortage.
impact: Setting
fix: Measure this database's read latency and I/O time during normal load before changing shared_buffers. If they confirm substantial I/O, a common starting point is a quarter of the server's memory, with room left for the operating system and query memory.
sql: -- A quarter of the server's memory (4GB on a 16 GB server); takes effect after a restart.
ALTER SYSTEM SET shared_buffers = '<a quarter of RAM>';
cost: That much memory is reserved for PostgreSQL.
verify: Re-scan after the server has restarted with the change: this finding is gone.

id: config:shared_buffers · Medium · Medium · score 17.6 · share None
title: shared_buffers is only 64.0 MB
summary: PostgreSQL keeps only 64.0 MB of this database's 50.0 GB in its own cache and leans on the operating system for the rest. Operating-system cache hits can still be fast; this size and the cache-hit ratio alone do not establish a memory shortage.
impact: Setting
fix: Measure this database's read latency and I/O time during normal load before changing shared_buffers. If they confirm substantial I/O, a common starting point is a quarter of the server's memory, with room left for the operating system and query memory.
sql: -- A quarter of the server's memory (4GB on a 16 GB server); takes effect after a restart.
ALTER SYSTEM SET shared_buffers = '<a quarter of RAM>';
cost: That much memory is reserved for PostgreSQL.
verify: Re-scan after the server has restarted with the change: this finding is gone.

id: config:wal_buffers · Low · Medium · score 6.4 · share None
title: WAL buffers filled 30K times in the 24 hours since its statistics were last reset
summary: wal_buffers is -1 (auto-sized, up to 16 MB), and backends had to flush WAL buffer pages themselves 30K times in the 24 hours since its statistics were last reset (pg_stat_wal.wal_buffers_full) because the buffers filled before walwriter wrote them out. Each full-buffer write makes the backend wait on WAL I/O during bursts of writes.
impact: Setting
metric: WAL buffer full writes: 30000 Count
fix: Give walwriter more buffer room (64 MB is four WAL segments). Set wal_buffers to 64MB and reload the configuration (settings marked as needing a restart take effect after one). It takes effect after a restart.
sql: ALTER SYSTEM SET wal_buffers = '64MB';
SELECT pg_reload_conf();
cost: 64 MB of shared memory.
verify: Re-scan after the server has restarted with the change: this finding is gone.

id: config:wal_buffers · Low · Medium · score 6.4 · share None
title: WAL buffers filled 30K times in the 24 hours since its statistics were last reset
summary: wal_buffers is 16MB, and backends had to flush WAL buffer pages themselves 30K times in the 24 hours since its statistics were last reset (pg_stat_wal.wal_buffers_full) because the buffers filled before walwriter wrote them out. Each full-buffer write makes the backend wait on WAL I/O during bursts of writes.
impact: Setting
metric: WAL buffer full writes: 30000 Count
fix: Give walwriter more buffer room (64 MB is four WAL segments). Set wal_buffers to 64MB in the instance's DB parameter group. It takes effect after a restart.
cost: 64 MB of shared memory.
verify: Re-scan after the server has restarted with the change: this finding is gone."#,
    );
}

fn evicting(dealloc: &str, max: Option<(&str, Option<&str>)>, statements: f64) -> Snapshot {
    let mut s = fixture("postgres-demo.json");
    set(&mut s, "pg_stat_statements.dealloc", dealloc);
    s.settings.retain(|x| x.name != "pg_stat_statements.max");
    if let Some((value, default)) = max {
        s.settings.push(culprit_core::Setting {
            name: "pg_stat_statements.max".into(),
            value: value.into(),
            default_value: default.map(str::to_string),
            scope: "server".into(),
        });
    }
    s.workload.as_mut().unwrap().query_count = statements;
    s
}

fn evictions(s: &Snapshot) -> Vec<culprit_core::Finding> {
    only(s, "config:pg_stat_statements.max")
}

#[test]
fn pg_stat_statements_evictions_say_how_many_and_whose() {
    let mut all = Vec::new();
    // Another database's statements, its reset dated; this database's; no limit known.
    let mut s = evicting("15397", Some(("5000", None)), 22.0);
    s.workload.as_mut().unwrap().window_start = Some("2023-04-17T10:00:21Z".into());
    all.extend(evictions(&s));
    all.extend(evictions(&evicting("15397", Some(("5000", None)), 4_000.0)));
    all.extend(evictions(&evicting("15397", None, 4_000.0)));
    // At its default, and raised from it.
    all.extend(evictions(&evicting("15397", Some(("20000", Some("20000"))), 6_000.0)));
    all.extend(evictions(&evicting("15397", Some(("20000", Some("5000"))), 6_000.0)));
    // A few rounds in a long window: a rate a day.
    let mut s = evicting("40", Some(("5000", None)), 3_000.0);
    s.workload.as_mut().unwrap().window_hours = 240.0;
    all.extend(evictions(&s));
    // Rounds that fill the added room in a few days.
    let mut s = evicting("200", Some(("5000", None)), 3_000.0);
    s.workload.as_mut().unwrap().window_hours = 240.0;
    all.extend(evictions(&s));
    // A window too short for a rate, its reset undated.
    let mut s = evicting("15397", Some(("5000", None)), 3_000.0);
    let w = s.workload.as_mut().unwrap();
    (w.window_start, w.window_hours) = (None, 0.5);
    all.extend(evictions(&s));
    pin(
        "config_postgres_evictions",
        &all,
        r#"id: config:pg_stat_statements.max · Low · High · score 6.4 · share None
title: pg_stat_statements ran out of room 15K times, evicting about 3.8M entries
summary: It keeps at most 5000 distinct statements for the whole server. Since its statistics were reset on Apr 17, 2023, it has run out of room 15K times, and each time it evicts the least-used 5% of them, 250 entries at a limit of 5000: about 3.8M entries in all. The numbers for anything that isn't constantly running are incomplete. This database has only 22 of them, so the statements filling it likely come from other databases on the server, or tools connected to them; they can still push this database's rarely run statements out.
impact: Setting
metric: Eviction rounds: 15397 Count
metric: Entries evicted (estimate): 3849250 Count
fix: Keep more entries, and fix the statement families that create them (pg_stat_statements without a database filter shows whose they are). Set pg_stat_statements.max to 20000 and reload the configuration (settings marked as needing a restart take effect after one). It takes effect after a restart.
sql: ALTER SYSTEM SET pg_stat_statements.max = 20000;
SELECT pg_reload_conf();
cost: Under half a kilobyte of shared memory per entry, set aside when the server starts: at most about 9.8 MB for 20000 entries. The statements' text is kept in a file on disk, not in shared memory.
verify: Re-scan once the change has taken effect, then a day later: the eviction rounds (15397 now) should not grow between those two scans, and with none between them this finding is gone. The count itself only starts over when pg_stat_statements' statistics are reset.
caveat: The count covers everything since its statistics were reset on Apr 17, 2023, and neither a restart nor a higher limit clears it. If pg_stat_statements.max was raised after these evictions, compare the count between two scans before raising it again.
caveat: The entries are estimated at today's limit: rounds counted while the limit was different evicted 5% of that one.

id: config:pg_stat_statements.max · Medium · High · score 17.6 · share None
title: pg_stat_statements ran out of room 15K times, evicting about 3.8M entries
summary: It keeps at most 5000 distinct statements for the whole server. Since its statistics were reset on Oct 11, it has run out of room 15K times, and each time it evicts the least-used 5% of them, 250 entries at a limit of 5000: about 3.8M entries in all. The numbers for anything that isn't constantly running are incomplete. This usually means the app sends many distinct statements (growing IN lists, generated SQL).
impact: Setting
metric: Eviction rounds: 15397 Count
metric: Entries evicted (estimate): 3849250 Count
fix: Keep more entries, and fix the statement families that create them. Set pg_stat_statements.max to 20000 and reload the configuration (settings marked as needing a restart take effect after one). It takes effect after a restart.
sql: ALTER SYSTEM SET pg_stat_statements.max = 20000;
SELECT pg_reload_conf();
cost: Under half a kilobyte of shared memory per entry, set aside when the server starts: at most about 9.8 MB for 20000 entries. The statements' text is kept in a file on disk, not in shared memory.
verify: Re-scan once the change has taken effect, then a day later: the eviction rounds (15397 now) should not grow between those two scans, and with none between them this finding is gone. The count itself only starts over when pg_stat_statements' statistics are reset.
caveat: The count covers everything since its statistics were reset on Oct 11, and neither a restart nor a higher limit clears it. If pg_stat_statements.max was raised after these evictions, compare the count between two scans before raising it again.
caveat: The entries are estimated at today's limit: rounds counted while the limit was different evicted 5% of that one.

id: config:pg_stat_statements.max · Low · High · score 6.4 · share None
title: pg_stat_statements ran out of room 15K times, evicting at least 154K entries
summary: It keeps at most its limit of distinct statements for the whole server. Since its statistics were reset on Oct 11, it has run out of room 15K times, and each time it evicts the least-used 5% of them, at least 10 entries: at least 154K entries in all. The numbers for anything that isn't constantly running are incomplete.
impact: Setting
metric: Eviction rounds: 15397 Count
metric: Entries evicted (at least): 153970 Count
fix: Keep more entries, and fix the statement families that create them. Set pg_stat_statements.max to 20000 and reload the configuration (settings marked as needing a restart take effect after one). It takes effect after a restart.
sql: ALTER SYSTEM SET pg_stat_statements.max = 20000;
SELECT pg_reload_conf();
cost: Under half a kilobyte of shared memory per entry, set aside when the server starts: at most about 9.8 MB for 20000 entries. The statements' text is kept in a file on disk, not in shared memory.
verify: Re-scan once the change has taken effect, then a day later: the eviction rounds (15397 now) should not grow between those two scans, and with none between them this finding is gone. The count itself only starts over when pg_stat_statements' statistics are reset.
caveat: The count covers everything since its statistics were reset on Oct 11, and neither a restart nor a higher limit clears it. If pg_stat_statements.max was raised after these evictions, compare the count between two scans before raising it again.

id: config:pg_stat_statements.max · Medium · High · score 17.6 · share None
title: pg_stat_statements ran out of room 15K times, evicting about 15.4M entries
summary: It keeps at most 20000 distinct statements for the whole server. Since its statistics were reset on Oct 11, it has run out of room 15K times, and each time it evicts the least-used 5% of them, 1.0K entries at a limit of 20000: about 15.4M entries in all. The numbers for anything that isn't constantly running are incomplete. This usually means the app sends many distinct statements (growing IN lists, generated SQL).
impact: Setting
metric: Eviction rounds: 15397 Count
metric: Entries evicted (estimate): 15397000 Count
fix: Keep more entries, and fix the statement families that create them. Set pg_stat_statements.max to 40000 and reload the configuration (settings marked as needing a restart take effect after one). It takes effect after a restart.
sql: ALTER SYSTEM SET pg_stat_statements.max = 40000;
SELECT pg_reload_conf();
cost: Under half a kilobyte of shared memory per entry, set aside when the server starts: at most about 19.5 MB for 40000 entries. The statements' text is kept in a file on disk, not in shared memory.
verify: Re-scan once the change has taken effect, then a day later: the eviction rounds (15397 now) should not grow between those two scans, and with none between them this finding is gone. The count itself only starts over when pg_stat_statements' statistics are reset.
caveat: The count covers everything since its statistics were reset on Oct 11, and neither a restart nor a higher limit clears it.

id: config:pg_stat_statements.max · Low · High · score 6.4 · share None
title: pg_stat_statements ran out of room 15K times, evicting at least 3.8M entries
summary: It keeps at most 20000 distinct statements for the whole server. Since its statistics were reset on Oct 11, it has run out of room 15K times, and each time it evicts the least-used 5% of them: 1.0K entries at today's limit of 20000, 250 at its default of 5000: at least 3.8M entries in all. The numbers for anything that isn't constantly running are incomplete. This usually means the app sends many distinct statements (growing IN lists, generated SQL).
impact: Setting
metric: Eviction rounds: 15397 Count
metric: Entries evicted (at least): 3849250 Count
fix: The limit is already above its default of 5000, and the count includes any rounds from before it was raised: compare the eviction rounds between two scans first. If they still grow, keep more entries, and fix the statement families that create them. Set pg_stat_statements.max to 40000 and reload the configuration (settings marked as needing a restart take effect after one). It takes effect after a restart.
sql: ALTER SYSTEM SET pg_stat_statements.max = 40000;
SELECT pg_reload_conf();
cost: Under half a kilobyte of shared memory per entry, set aside when the server starts: at most about 19.5 MB for 40000 entries. The statements' text is kept in a file on disk, not in shared memory.
verify: Re-scan once the change has taken effect, then a day later: the eviction rounds (15397 now) should not grow between those two scans, and with none between them this finding is gone. The count itself only starts over when pg_stat_statements' statistics are reset.
caveat: The count covers everything since its statistics were reset on Oct 11, and neither a restart nor a higher limit clears it.
caveat: The entries are counted at the default's 250 a round, a floor: rounds after the limit was raised to 20000 evicted 1.0K each.

id: config:pg_stat_statements.max · Medium · High · score 17.6 · share None
title: pg_stat_statements ran out of room 40 times, evicting about 10K entries
summary: It keeps at most 5000 distinct statements for the whole server. Since its statistics were reset on Oct 11, it has run out of room 40 times, and each time it evicts the least-used 5% of them, 250 entries at a limit of 5000: about 10K entries in all, about 1.0K a day. The numbers for anything that isn't constantly running are incomplete. This usually means the app sends many distinct statements (growing IN lists, generated SQL).
impact: Setting
metric: Eviction rounds: 40 Count
metric: Entries evicted (estimate): 10000 Count
fix: Keep more entries, and fix the statement families that create them. Set pg_stat_statements.max to 20000 and reload the configuration (settings marked as needing a restart take effect after one). It takes effect after a restart.
sql: ALTER SYSTEM SET pg_stat_statements.max = 20000;
SELECT pg_reload_conf();
cost: Under half a kilobyte of shared memory per entry, set aside when the server starts: at most about 9.8 MB for 20000 entries. The statements' text is kept in a file on disk, not in shared memory.
verify: Re-scan once the change has taken effect, then a week later: the eviction rounds (40 now) should not grow between those two scans, and with none between them this finding is gone. The room the change adds (at most 15K entries) fills first, at about 1.0K new entries a day (the rate it has been evicting them), and nothing is evicted until it is full: a shorter quiet span proves nothing. A round after the week reopens this finding. The count itself only starts over when pg_stat_statements' statistics are reset.
caveat: The count covers everything since its statistics were reset on Oct 11, and neither a restart nor a higher limit clears it. If pg_stat_statements.max was raised after these evictions, compare the count between two scans before raising it again.
caveat: The entries are estimated at today's limit: rounds counted while the limit was different evicted 5% of that one.

id: config:pg_stat_statements.max · Medium · High · score 17.6 · share None
title: pg_stat_statements ran out of room 200 times, evicting about 50K entries
summary: It keeps at most 5000 distinct statements for the whole server. Since its statistics were reset on Oct 11, it has run out of room 200 times, and each time it evicts the least-used 5% of them, 250 entries at a limit of 5000: about 50K entries in all, about 5.0K a day. The numbers for anything that isn't constantly running are incomplete. This usually means the app sends many distinct statements (growing IN lists, generated SQL).
impact: Setting
metric: Eviction rounds: 200 Count
metric: Entries evicted (estimate): 50000 Count
fix: Keep more entries, and fix the statement families that create them. Set pg_stat_statements.max to 20000 and reload the configuration (settings marked as needing a restart take effect after one). It takes effect after a restart.
sql: ALTER SYSTEM SET pg_stat_statements.max = 20000;
SELECT pg_reload_conf();
cost: Under half a kilobyte of shared memory per entry, set aside when the server starts: at most about 9.8 MB for 20000 entries. The statements' text is kept in a file on disk, not in shared memory.
verify: Re-scan once the change has taken effect, then 3 days later: the eviction rounds (200 now) should not grow between those two scans, and with none between them this finding is gone. The room the change adds (at most 15K entries) fills first, at about 5.0K new entries a day (the rate it has been evicting them), and nothing is evicted until it is full: a shorter quiet span proves nothing. The count itself only starts over when pg_stat_statements' statistics are reset.
caveat: The count covers everything since its statistics were reset on Oct 11, and neither a restart nor a higher limit clears it. If pg_stat_statements.max was raised after these evictions, compare the count between two scans before raising it again.
caveat: The entries are estimated at today's limit: rounds counted while the limit was different evicted 5% of that one.

id: config:pg_stat_statements.max · Low · High · score 6.4 · share None
title: pg_stat_statements ran out of room 15K times, evicting about 3.8M entries
summary: It keeps at most 5000 distinct statements for the whole server. Since its statistics were reset, it has run out of room 15K times, and each time it evicts the least-used 5% of them, 250 entries at a limit of 5000: about 3.8M entries in all. The numbers for anything that isn't constantly running are incomplete. This usually means the app sends many distinct statements (growing IN lists, generated SQL).
impact: Setting
metric: Eviction rounds: 15397 Count
metric: Entries evicted (estimate): 3849250 Count
fix: Keep more entries, and fix the statement families that create them. Set pg_stat_statements.max to 20000 and reload the configuration (settings marked as needing a restart take effect after one). It takes effect after a restart.
sql: ALTER SYSTEM SET pg_stat_statements.max = 20000;
SELECT pg_reload_conf();
cost: Under half a kilobyte of shared memory per entry, set aside when the server starts: at most about 9.8 MB for 20000 entries. The statements' text is kept in a file on disk, not in shared memory.
verify: Re-scan once the change has taken effect, then a week later: the eviction rounds (15397 now) should not grow between those two scans, and with none between them this finding is gone. How fast new statements arrive isn't known, and nothing is evicted until the room the change adds is full: a shorter quiet span proves nothing. The count itself only starts over when pg_stat_statements' statistics are reset.
caveat: The count covers everything since its statistics were reset, and neither a restart nor a higher limit clears it. If pg_stat_statements.max was raised after these evictions, compare the count between two scans before raising it again.
caveat: The entries are estimated at today's limit: rounds counted while the limit was different evicted 5% of that one."#,
    );
}

#[test]
fn pg_stat_statements_evictions_since_the_previous_scan() {
    let mut all = Vec::new();
    let base = |max: (&str, Option<&str>)| {
        let mut s = evicting("15397", Some(max), 4_000.0);
        s.workload.as_mut().unwrap().window_start = Some("2023-04-17T10:00:21Z".into());
        s
    };
    let with_before = |s: &Snapshot, taken_at: &str, dealloc: &str| {
        let mut before = s.clone();
        before.taken_at = taken_at.into();
        set(&mut before, "pg_stat_statements.dealloc", dealloc);
        let mut s = s.clone();
        s.evictions_before = culprit_core::CountBaseline::from_setting(&before, &s, "pg_stat_statements.dealloc");
        s
    };
    // Twenty rounds in the two days since the previous scan; and in the five hours since one.
    let s = base(("5000", None));
    all.extend(evictions(&with_before(&s, "2026-10-09T11:54:21Z", "15377")));
    all.extend(evictions(&with_before(&s, "2026-10-11T06:54:21Z", "15377")));
    // Rounds since a raise from the default; a slow rate since the reset beside a fast one now.
    let s = base(("20000", Some("5000")));
    all.extend(evictions(&with_before(&s, "2026-10-09T11:54:21Z", "15387")));
    let mut s = base(("5000", None));
    s.workload.as_mut().unwrap().window_start = Some("2026-10-09T11:00:00Z".into());
    s.workload.as_mut().unwrap().window_hours = 48.9;
    all.extend(evictions(&with_before(&s, "2026-10-10T11:54:21Z", "15000")));
    pin(
        "config_postgres_evictions_since_scan",
        &all,
        r#"id: config:pg_stat_statements.max · Medium · High · score 17.6 · share None
title: pg_stat_statements ran out of room 20 times since Culprit's scan on Oct 9 at 11:54 UTC, evicting about 5.0K entries
summary: It keeps at most 5000 distinct statements for the whole server. Since its statistics were reset on Apr 17, 2023, it has run out of room 15K times, and each time it evicts the least-used 5% of them, 250 entries at a limit of 5000: about 3.8M entries in all. 20 of those rounds came since Culprit's scan on Oct 9 at 11:54 UTC: about 5.0K entries, about 2.5K a day (about 18.9M a day since its statistics were reset). The numbers for anything that isn't constantly running are incomplete. This usually means the app sends many distinct statements (growing IN lists, generated SQL).
impact: Setting
metric: Eviction rounds: 15397 Count
metric: Eviction rounds since Culprit's scan on Oct 9 at 11:54 UTC: 20 Count
metric: Entries evicted (estimate): 3849250 Count
fix: Keep more entries, and fix the statement families that create them. Set pg_stat_statements.max to 20000 and reload the configuration (settings marked as needing a restart take effect after one). It takes effect after a restart.
sql: ALTER SYSTEM SET pg_stat_statements.max = 20000;
SELECT pg_reload_conf();
cost: Under half a kilobyte of shared memory per entry, set aside when the server starts: at most about 9.8 MB for 20000 entries. The statements' text is kept in a file on disk, not in shared memory.
verify: Re-scan once the change has taken effect, then 6 days later: the eviction rounds (15397 now) should not grow between those two scans, and with none between them this finding is gone. The room the change adds (at most 15K entries) fills first, at about 2.5K new entries a day (the rate Culprit's scans measured it evicting them), and nothing is evicted until it is full: a shorter quiet span proves nothing. The count itself only starts over when pg_stat_statements' statistics are reset.
caveat: The count covers everything since its statistics were reset on Apr 17, 2023, and neither a restart nor a higher limit clears it. If pg_stat_statements.max was raised after these evictions, compare the count between two scans before raising it again.
caveat: The entries are estimated at today's limit: rounds counted while the limit was different evicted 5% of that one.

id: config:pg_stat_statements.max · Medium · High · score 17.6 · share None
title: pg_stat_statements ran out of room 20 times since Culprit's scan on Oct 11 at 06:54 UTC, evicting about 5.0K entries
summary: It keeps at most 5000 distinct statements for the whole server. Since its statistics were reset on Apr 17, 2023, it has run out of room 15K times, and each time it evicts the least-used 5% of them, 250 entries at a limit of 5000: about 3.8M entries in all. 20 of those rounds came since Culprit's scan on Oct 11 at 06:54 UTC: about 5.0K entries in 5 hours. The numbers for anything that isn't constantly running are incomplete. This usually means the app sends many distinct statements (growing IN lists, generated SQL).
impact: Setting
metric: Eviction rounds: 15397 Count
metric: Eviction rounds since Culprit's scan on Oct 11 at 06:54 UTC: 20 Count
metric: Entries evicted (estimate): 3849250 Count
fix: Keep more entries, and fix the statement families that create them. Set pg_stat_statements.max to 20000 and reload the configuration (settings marked as needing a restart take effect after one). It takes effect after a restart.
sql: ALTER SYSTEM SET pg_stat_statements.max = 20000;
SELECT pg_reload_conf();
cost: Under half a kilobyte of shared memory per entry, set aside when the server starts: at most about 9.8 MB for 20000 entries. The statements' text is kept in a file on disk, not in shared memory.
verify: Re-scan once the change has taken effect, then a day later: the eviction rounds (15397 now) should not grow between those two scans, and with none between them this finding is gone. The count itself only starts over when pg_stat_statements' statistics are reset.
caveat: The count covers everything since its statistics were reset on Apr 17, 2023, and neither a restart nor a higher limit clears it. If pg_stat_statements.max was raised after these evictions, compare the count between two scans before raising it again.
caveat: The entries are estimated at today's limit: rounds counted while the limit was different evicted 5% of that one.

id: config:pg_stat_statements.max · Low · High · score 6.4 · share None
title: pg_stat_statements ran out of room 10 times since Culprit's scan on Oct 9 at 11:54 UTC, evicting at least 2.5K entries
summary: It keeps at most 20000 distinct statements for the whole server. Since its statistics were reset on Apr 17, 2023, it has run out of room 15K times, and each time it evicts the least-used 5% of them: 1.0K entries at today's limit of 20000, 250 at its default of 5000: at least 3.8M entries in all. 10 of those rounds came since Culprit's scan on Oct 9 at 11:54 UTC: at least 2.5K entries, at least 1.2K a day (at least 18.9M a day since its statistics were reset). The numbers for anything that isn't constantly running are incomplete. This database has only 4.0K of them, so the statements filling it likely come from other databases on the server, or tools connected to them; they can still push this database's rarely run statements out.
impact: Setting
metric: Eviction rounds: 15397 Count
metric: Eviction rounds since Culprit's scan on Oct 9 at 11:54 UTC: 10 Count
metric: Entries evicted (at least): 3849250 Count
fix: The limit is already above its default of 5000, and the eviction rounds still grow between scans: keep more entries, and fix the statement families that create them (pg_stat_statements without a database filter shows whose they are). Set pg_stat_statements.max to 40000 and reload the configuration (settings marked as needing a restart take effect after one). It takes effect after a restart.
sql: ALTER SYSTEM SET pg_stat_statements.max = 40000;
SELECT pg_reload_conf();
cost: Under half a kilobyte of shared memory per entry, set aside when the server starts: at most about 19.5 MB for 40000 entries. The statements' text is kept in a file on disk, not in shared memory.
verify: Re-scan once the change has taken effect, then a week later: the eviction rounds (15397 now) should not grow between those two scans, and with none between them this finding is gone. The room the change adds (at most 35K entries) fills first, at about 1.2K new entries a day (the rate Culprit's scans measured it evicting them), and nothing is evicted until it is full: a shorter quiet span proves nothing. A round after the week reopens this finding. The count itself only starts over when pg_stat_statements' statistics are reset.
caveat: The count covers everything since its statistics were reset on Apr 17, 2023, and neither a restart nor a higher limit clears it.
caveat: The entries are counted at the default's 250 a round, a floor: rounds after the limit was raised to 20000 evicted 1.0K each.

id: config:pg_stat_statements.max · Medium · High · score 17.6 · share None
title: pg_stat_statements ran out of room 397 times since Culprit's scan on Oct 10 at 11:54 UTC, evicting about 99K entries
summary: It keeps at most 5000 distinct statements for the whole server. Since its statistics were reset on Oct 9, it has run out of room 15K times, and each time it evicts the least-used 5% of them, 250 entries at a limit of 5000: about 3.8M entries in all. 397 of those rounds came since Culprit's scan on Oct 10 at 11:54 UTC: about 99K entries, about 99K a day (about 1.9M a day since its statistics were reset). The numbers for anything that isn't constantly running are incomplete. This usually means the app sends many distinct statements (growing IN lists, generated SQL).
impact: Setting
metric: Eviction rounds: 15397 Count
metric: Eviction rounds since Culprit's scan on Oct 10 at 11:54 UTC: 397 Count
metric: Entries evicted (estimate): 3849250 Count
fix: Keep more entries, and fix the statement families that create them. Set pg_stat_statements.max to 20000 and reload the configuration (settings marked as needing a restart take effect after one). It takes effect after a restart.
sql: ALTER SYSTEM SET pg_stat_statements.max = 20000;
SELECT pg_reload_conf();
cost: Under half a kilobyte of shared memory per entry, set aside when the server starts: at most about 9.8 MB for 20000 entries. The statements' text is kept in a file on disk, not in shared memory.
verify: Re-scan once the change has taken effect, then a day later: the eviction rounds (15397 now) should not grow between those two scans, and with none between them this finding is gone. The count itself only starts over when pg_stat_statements' statistics are reset.
caveat: The count covers everything since its statistics were reset on Oct 9, and neither a restart nor a higher limit clears it. If pg_stat_statements.max was raised after these evictions, compare the count between two scans before raising it again.
caveat: The entries are estimated at today's limit: rounds counted while the limit was different evicted 5% of that one."#,
    );
}

// ── MySQL and MariaDB ────────────────────────────────────────────────────────────────────────────────────────────

fn buffer_pool(hit: f64, reads: Option<f64>, platform: Option<Platform>) -> Snapshot {
    let mut s = fixture("mysql-demo.json");
    s.server.set_platform(platform);
    for t in &mut s.tables {
        t.total_bytes = Some(40.0 * 1024.0 * 1024.0 * 1024.0);
    }
    set(&mut s, "innodb_buffer_pool_size", &(8u64 * 1024 * 1024 * 1024).to_string());
    let mut signals = vec![Signal::new("buffer-pool-hit", "hit", hit, SignalUnit::Percent, SignalScope::SinceStartup)];
    if let Some(reads) = reads {
        signals.push(Signal::new(
            "buffer-pool-reads",
            "reads",
            reads,
            SignalUnit::PerSecond,
            SignalScope::SinceStartup,
        ));
    }
    s.pressure = Some(Pressure { uptime_hours: Some(72.0), signals, ..Pressure::default() });
    s
}

#[test]
fn mysql_settings_say_what_they_cost() {
    pin(
        "config_mysql_demo",
        &config(&fixture("mysql-demo.json")),
        r#"id: config:slow_query_log · Low · Medium · score 4 · share None
title: The slow query log is off
summary: Without it there is no record of individual slow statements with their real parameter values, only the aggregated digests. Logged to the mysql.slow_log table, Culprit also uses those statements to plan digests that have no sample text.
impact: Setting
fix: Turn it on with a threshold that matches your latency budget. Set slow_query_log to ON, long_query_time to 1 and log_output to FILE,TABLE; SET PERSIST keeps it across restarts. The new long_query_time applies to new connections: reopen the application's existing connections, or have the application set the session variable on them.
sql: SET PERSIST slow_query_log = 'ON';
SET PERSIST long_query_time = 1;
SET PERSIST log_output = 'FILE,TABLE';
verify: Re-scan once the change has taken effect: this finding is gone."#,
    );
    let mut all = Vec::new();
    all.extend(only(&buffer_pool(90.0, Some(400.0), None), "config:innodb_buffer_pool_size"));
    all.extend(only(&buffer_pool(95.0, None, Some(Platform::AmazonRds)), "config:innodb_buffer_pool_size"));
    // The slow query log, off: a threshold already low and the table already written; on MariaDB; on a managed one.
    let mut s = fixture("mysql-demo.json");
    set(&mut s, "slow_query_log", "OFF");
    set(&mut s, "long_query_time", "0.2");
    set(&mut s, "log_output", "TABLE");
    all.extend(only(&s, "config:slow_query_log"));
    set(&mut s, "long_query_time", "10");
    set(&mut s, "log_output", "NONE");
    s.server.product = "MariaDB".into();
    all.extend(only(&s, "config:slow_query_log"));
    s.server.product = "MySQL".into();
    s.server.set_platform(Some(Platform::GoogleCloudSql));
    all.extend(only(&s, "config:slow_query_log"));
    pin(
        "config_mysql_settings",
        &all,
        r#"id: config:innodb_buffer_pool_size · Low · Medium · score 6.4 · share None
title: The server's InnoDB buffer pool records 10% of reads missing memory
summary: innodb_buffer_pool_size is 8.0 GB, and this database has 160 GB of InnoDB tables and indexes. Since MySQL started, 90% of page reads across the whole server were served from its buffer pool (about 400 pages a second from disk). The rest may have been served by the operating system cache; this does not establish slow reads in this database.
impact: Setting
metric: Reads served from memory: 0.9 Ratio
metric: Pages read from disk per second: 400 Count
fix: Check measured read latency and this database's workload before resizing the server's buffer pool. If they confirm a memory shortage, allow more memory for the pool while leaving room for connections and the operating system; it can be resized online with SET GLOBAL innodb_buffer_pool_size.
cost: Uses that much RAM; a pool larger than the free memory makes the server swap or be killed.
verify: Re-scan after a day of normal load: reads served from memory should be at 99% or more (90% now). The share counts since MySQL started, so after an online resize it rises slowly; after a restart it counts afresh.

id: config:innodb_buffer_pool_size · Low · Medium · score 6.4 · share None
title: The server's InnoDB buffer pool records 5% of reads missing memory
summary: innodb_buffer_pool_size is 8.0 GB, and this database has 160 GB of InnoDB tables and indexes. Since MySQL started, 95% of page reads across the whole server were served from its buffer pool. The rest may have been served by the operating system cache; this does not establish slow reads in this database.
impact: Setting
metric: Reads served from memory: 0.95 Ratio
fix: If measured read latency and this database's workload confirm a memory shortage, consider giving the buffer pool more of the instance's memory in the instance's DB parameter group (Amazon RDS sizes it from the instance's memory), or move to an instance with more memory. Leave room for connections and the operating system.
cost: Uses that much RAM; a pool larger than the free memory makes the server swap or be killed.
verify: Re-scan after a day of normal load: reads served from memory should be at 99% or more (95% now). The share counts since MySQL started, so after an online resize it rises slowly; after a restart it counts afresh.

id: config:slow_query_log · Low · Medium · score 4 · share None
title: The slow query log is off
summary: Without it there is no record of individual slow statements with their real parameter values, only the aggregated digests. Logged to the mysql.slow_log table, Culprit also uses those statements to plan digests that have no sample text.
impact: Setting
fix: Turn it on with a threshold that matches your latency budget. Set slow_query_log to ON; SET PERSIST keeps it across restarts.
sql: SET PERSIST slow_query_log = 'ON';
verify: Re-scan once the change has taken effect: this finding is gone.

id: config:slow_query_log · Low · Medium · score 4 · share None
title: The slow query log is off
summary: Without it there is no record of individual slow statements with their real parameter values, only the aggregated digests. Logged to the mysql.slow_log table, Culprit also uses those statements to plan digests that have no sample text.
impact: Setting
fix: Turn it on with a threshold that matches your latency budget. Set slow_query_log to ON, long_query_time to 1 and log_output to TABLE now, and in the server's option file (my.cnf) so it survives a restart. The new long_query_time applies to new connections: reopen the application's existing connections, or have the application set the session variable on them.
sql: SET GLOBAL slow_query_log = 'ON';
SET GLOBAL long_query_time = 1;
SET GLOBAL log_output = 'TABLE';
cost: Written to a table (TABLE in log_output), MariaDB counts the whole run of each statement it logs as that statement's lock time, so their lock time says nothing (Culprit reads it as unknown); written to a file (FILE), it is the real wait. Leaving TABLE out keeps lock time real, but Culprit then can't use the logged statements to plan digests without sample text.
verify: Re-scan once the change has taken effect: this finding is gone.

id: config:slow_query_log · Low · Medium · score 4 · share None
title: The slow query log is off
summary: Without it there is no record of individual slow statements with their real parameter values, only the aggregated digests. Logged to the mysql.slow_log table, Culprit also uses those statements to plan digests that have no sample text.
impact: Setting
fix: Turn it on with a threshold that matches your latency budget. Set slow_query_log to ON, long_query_time to 1 and log_output to TABLE in the instance's database flags (Cloud SQL console or `gcloud sql instances patch --database-flags`). The new long_query_time applies to new connections: reopen the application's existing connections, or have the application set the session variable on them.
verify: Re-scan once the change has taken effect: this finding is gone."#,
    );
}

#[test]
fn mysql_performance_schema_says_what_is_missing() {
    let mut all = Vec::new();
    // Off, with the slow log off too: start there; then on RDS; then with the slow log read already.
    let mut s = fixture("mysql-demo.json");
    set(&mut s, "performance_schema", "OFF");
    set(&mut s, "slow_query_log", "OFF");
    set(&mut s, "userstat", "OFF");
    set(&mut s, "log_output", "FILE");
    all.extend(only(&s, "config:performance_schema"));
    s.server.set_platform(Some(Platform::AmazonRds));
    all.extend(only(&s, "config:performance_schema"));
    s.server.set_platform(None);
    set(&mut s, "slow_query_log", "ON");
    set(&mut s, "log_output", "TABLE");
    set(&mut s, "long_query_time", "0.1");
    s.settings.retain(|x| x.name != "userstat");
    all.extend(only(&s, "config:performance_schema"));
    s.workload.as_mut().unwrap().kind = Some(culprit_core::WorkloadKind::SlowLog);
    all.extend(only(&s, "config:performance_schema"));
    pin(
        "config_mysql_performance_schema",
        &all,
        r#"id: config:performance_schema · High · High · score 36 · share None
title: performance_schema is off
summary: Without it MySQL keeps no statement digests, index usage or wait statistics: there is nothing to tell which queries are slow or which indexes are used.
impact: Setting
fix: Start with the slow query log, which needs no restart: Culprit reads it from the mysql.slow_log table. Set userstat to ON, slow_query_log to ON, log_output to FILE,TABLE and long_query_time to 0.1; SET PERSIST keeps it across restarts. Turn performance_schema on at the next restart for statistics on every statement: set it in the server configuration (my.cnf) and restart.
sql: -- Now, no restart (connections opened earlier keep the old threshold):
SET PERSIST userstat = 'ON';
SET PERSIST slow_query_log = 'ON';
SET PERSIST log_output = 'FILE,TABLE';
SET PERSIST long_query_time = 0.1;

-- In the server configuration (my.cnf), then restart:
[mysqld]
performance_schema = ON
cost: A few hundred MB of memory and a small CPU overhead.
verify: Re-scan after the server has restarted with the change: this finding is gone.

id: config:performance_schema · High · High · score 36 · share None
title: performance_schema is off
summary: Without it MySQL keeps no statement digests, index usage or wait statistics: there is nothing to tell which queries are slow or which indexes are used.
impact: Setting
fix: Start with the slow query log, which needs no restart: Culprit reads it from the mysql.slow_log table. Set userstat to ON, slow_query_log to ON, log_output to FILE,TABLE and long_query_time to 0.1 in the instance's DB parameter group. Turn performance_schema on at the next restart for statistics on every statement: set performance_schema to ON in the instance's DB parameter group. It takes effect after a reboot.
cost: A few hundred MB of memory and a small CPU overhead.
verify: Re-scan after the server has restarted with the change: this finding is gone.

id: config:performance_schema · High · High · score 36 · share None
title: performance_schema is off
summary: Without it MySQL keeps no statement digests, index usage or wait statistics: there is nothing to tell which queries are slow or which indexes are used.
impact: Setting
fix: Turn it on (it can't be changed at runtime). Set it in the server configuration (my.cnf) and restart.
sql: -- In the server configuration (my.cnf), then restart:
[mysqld]
performance_schema = ON
cost: A few hundred MB of memory and a small CPU overhead.
verify: Re-scan after the server has restarted with the change: this finding is gone.

id: config:performance_schema · Medium · High · score 17.6 · share None
title: performance_schema is off
summary: Culprit reads the slow query log, so statements slower than long_query_time are covered. Without performance_schema, MySQL keeps no statistics for faster statements and no wait statistics.
impact: Setting
fix: Turn it on (it can't be changed at runtime). Set it in the server configuration (my.cnf) and restart.
sql: -- In the server configuration (my.cnf), then restart:
[mysqld]
performance_schema = ON
cost: A few hundred MB of memory and a small CPU overhead.
verify: Re-scan after the server has restarted with the change: this finding is gone."#,
    );
}

#[test]
fn mysql_digests_say_what_they_lose() {
    let mut s = fixture("mysql-demo.json");
    set(&mut s, "performance_schema.consumer.statements_digest", "NO");
    set(&mut s, "performance_schema_digest_lost", "1200");
    set(&mut s, "performance_schema.untracked_ms", "0");
    set(&mut s, "max_digest_length", "1024");
    for q in s.workload.as_mut().unwrap().queries.iter_mut().take(3) {
        q.text = format!("{} ...", q.text);
    }
    s.pressure.get_or_insert_with(Default::default).uptime_hours = Some(24.0);
    s.pressure.as_mut().unwrap().signals.push(Signal::new(
        "buffer-pool-wait-free",
        "Buffer pool free-page waits",
        500.0,
        SignalUnit::Count,
        SignalScope::SinceStartup,
    ));
    let mut all = config(&s);
    // Untracked time in the anonymous row, on a managed server; and waits that make it Medium.
    let total = s.workload.as_ref().unwrap().total_time_ms;
    set(&mut s, "performance_schema.untracked_ms", &format!("{}", total * 0.5));
    set(&mut s, "performance_schema_digests_size", "-1");
    s.server.set_platform(Some(Platform::AmazonRds));
    all.extend(only(&s, "config:performance_schema_digests_size"));
    all.extend(only(&s, "config:max_digest_length"));
    pin(
        "config_mysql_digests",
        &all,
        r#"id: config:slow_query_log · Low · Medium · score 4 · share None
title: The slow query log is off
summary: Without it there is no record of individual slow statements with their real parameter values, only the aggregated digests. Logged to the mysql.slow_log table, Culprit also uses those statements to plan digests that have no sample text.
impact: Setting
fix: Turn it on with a threshold that matches your latency budget. Set slow_query_log to ON, long_query_time to 1 and log_output to FILE,TABLE; SET PERSIST keeps it across restarts. The new long_query_time applies to new connections: reopen the application's existing connections, or have the application set the session variable on them.
sql: SET PERSIST slow_query_log = 'ON';
SET PERSIST long_query_time = 1;
SET PERSIST log_output = 'FILE,TABLE';
verify: Re-scan once the change has taken effect: this finding is gone.

id: config:statements_digest · Medium · Medium · score 17.6 · share None
title: Statement digests aren't being collected
summary: The statements_digest consumer is disabled, so performance_schema doesn't aggregate statements: the workload view stays empty.
impact: Setting
fix: Enable the consumer (and persist it in my.cnf).
sql: UPDATE performance_schema.setup_consumers SET ENABLED = 'YES' WHERE NAME = 'statements_digest';
verify: Re-scan once the change has taken effect: this finding is gone.

id: config:performance_schema_digests_size · Low · High · score 6.4 · share None
title: performance_schema ran out of room for statement digests
summary: Its digest table holds 10000 statements and is full: 1.2K new statements were lumped into one anonymous row. Those statements are invisible to per-query analysis. It usually means the app sends many distinct statements (generated SQL, unparameterized literals).
impact: Setting
metric: Digests lost: 1200 Count
fix: Raise the table size and truncate the summary to start fresh. Set it in the server configuration (my.cnf) and restart.
sql: -- In the server configuration (my.cnf), then restart:
[mysqld]
performance_schema_digests_size = 50000
-- or start over now:
TRUNCATE TABLE performance_schema.events_statements_summary_by_digest;
verify: Re-scan after the restart: no digests lost, and no statement time left in the anonymous row. After a truncate alone, compare the lost count between two scans: it should stop growing.

id: config:max_digest_length · Low · Medium · score 6.4 · share None
title: 3 statements are cut off at 1,024 characters
summary: Long statements are truncated in performance_schema, so statements that differ only after the cut are merged into one digest, and Culprit can't explain them.
impact: Setting
fix: Keep longer text. Set it in the server configuration (my.cnf) and restart.
sql: -- In the server configuration (my.cnf), then restart:
[mysqld]
max_digest_length = 4096
performance_schema_max_digest_length = 4096
performance_schema_max_sql_text_length = 4096
cost: A little more memory per digest.
verify: Re-scan after the server has restarted with the change and the statements have run again: they are no longer cut off, and this finding is gone.

id: config:innodb_buffer_pool_wait_free · Low · Medium · score 6.4 · share None
title: Queries waited 500 times for a clean buffer-pool page
summary: Innodb_buffer_pool_wait_free is 500 in the 24 hours since the server last restarted: foreground threads needed a free page in the InnoDB buffer pool when none was clean, and had to wait for dirty pages to be flushed first.
impact: Setting
metric: Buffer pool free-page waits: 500 Count
fix: If the buffer pool is undersized for the working set, grow innodb_buffer_pool_size; if write bursts dirty pages faster than background flushing cleans them, raise innodb_io_capacity (and innodb_io_capacity_max) to match the storage's IOPS.
verify: Compare Innodb_buffer_pool_wait_free between two scans under normal write load: it should stop growing.

id: config:performance_schema_digests_size · Medium · High · score 17.6 · share None
title: performance_schema ran out of room for statement digests
summary: Its digest table holds -1 statements and is full: 1.2K new statements were lumped into one anonymous row, which holds 33% of statement time. Those statements are invisible to per-query analysis. It usually means the app sends many distinct statements (generated SQL, unparameterized literals).
impact: Setting
metric: Digests lost: 1200 Count
metric: Untracked time: 0.33333333333333337 Ratio
fix: Raise the table size and truncate the summary to start fresh. Set performance_schema_digests_size to 50000 in the instance's DB parameter group. It takes effect after a reboot.
sql: -- Start over now:
TRUNCATE TABLE performance_schema.events_statements_summary_by_digest;
verify: Re-scan after the restart: no digests lost, and no statement time left in the anonymous row. After a truncate alone, compare the lost count between two scans: it should stop growing.

id: config:max_digest_length · Low · Medium · score 6.4 · share None
title: 3 statements are cut off at 1,024 characters
summary: Long statements are truncated in performance_schema, so statements that differ only after the cut are merged into one digest, and Culprit can't explain them.
impact: Setting
fix: Keep longer text. Set max_digest_length to 4096, performance_schema_max_digest_length to 4096 and performance_schema_max_sql_text_length to 4096 in the instance's DB parameter group. It takes effect after a reboot.
cost: A little more memory per digest.
verify: Re-scan after the server has restarted with the change and the statements have run again: they are no longer cut off, and this finding is gone."#,
    );
}

// ── SQLite ───────────────────────────────────────────────────────────────────────────────────────────────────────

#[test]
fn sqlite_files_say_what_they_hold() {
    pin(
        "config_sqlite_demo",
        &config(&fixture("sqlite-demo.json")),
        r#"id: config:sqlite-statistics · Low · High · score 8 · share None
title: SQLite has no statistics to choose between this file's indexes
summary: The file has no current sqlite_stat1 statistics, so SQLite falls back on fixed guesses about how big each table is and how well each index narrows a search. With 3 secondary indexes to choose from, a guess can pick the wrong one, or a full scan.
impact: No statistics
metric: Secondary indexes: 3 Count
fix: Run ANALYZE once, then PRAGMA optimize from time to time (when a connection closes is the usual place): it re-analyzes only what changed.
sql: ANALYZE;
cost: ANALYZE reads every index once and holds the write lock while it stores the results: on a large file, run it when writes are quiet.
verify: Scan again: the file has sqlite_stat1, and this finding is gone.

id: config:sqlite-free-pages · Low · High · score 8 · share None
title: 47.0 MB of the file is free pages (71% of it)
summary: Deleted rows leave their pages in the file for SQLite to reuse; the file doesn't shrink on its own. That's fine when new rows will fill them again. If not, the file stays larger than its data, which every backup and copy pays for.
impact: 47.0 MB free
metric: Free pages: 49270784 Bytes
metric: Share of the file: 0.7079630392560767 Ratio
fix: VACUUM rewrites the file without its free pages. To give space back after large deletes from then on, switch auto_vacuum to INCREMENTAL first (it takes effect with that VACUUM) and run PRAGMA incremental_vacuum when convenient.
sql: PRAGMA auto_vacuum = INCREMENTAL;
VACUUM;
cost: VACUUM holds the write lock for its whole run and can need up to twice the original file's size in free disk space.
verify: Scan again: free pages are a small part of the file."#,
    );
    let mut all = Vec::new();
    // Foreign keys declared but not enforced; statistics for some indexes but not these.
    let mut s = fixture("sqlite-demo.json");
    set(&mut s, "foreign_keys", "0");
    all.extend(only(&s, "config:sqlite-foreign-keys"));
    s.foreign_keys.truncate(1);
    all.extend(only(&s, "config:sqlite-foreign-keys"));
    // One secondary index, the tables' sizes unknown.
    let mut s = fixture("sqlite-demo.json");
    let secondary = s.indexes.iter().position(|i| !i.is_primary_key).unwrap();
    let keep = s.indexes[secondary].clone();
    s.indexes.retain(|i| i.is_primary_key);
    s.indexes.push(keep);
    for t in &mut s.tables {
        t.rows = None;
    }
    all.extend(only(&s, "config:sqlite-statistics"));
    // Free pages, already vacuumed incrementally.
    let mut s = fixture("sqlite-demo.json");
    set(&mut s, "auto_vacuum", "2");
    all.extend(only(&s, "config:sqlite-free-pages"));
    pin(
        "config_sqlite",
        &all,
        r#"id: config:sqlite-foreign-keys · Low · High · score 8 · share None
title: Foreign key constraints are declared, but PRAGMA foreign_keys is OFF
summary: This database declares 3 foreign key constraints, but PRAGMA foreign_keys is off, so SQLite ignores them on every insert, update and delete. Enable PRAGMA foreign_keys = ON on each application connection if referential integrity should be enforced.
impact: 3 foreign keys unenforced
metric: Foreign keys: 3 Count
fix: Run PRAGMA foreign_keys = ON when opening each connection in the application (SQLite resets this setting per connection). Check existing rows with PRAGMA foreign_key_check before enabling it.
sql: PRAGMA foreign_key_check;
PRAGMA foreign_keys = ON;
verify: Check that every application connection sets PRAGMA foreign_keys = ON after opening the database.

id: config:sqlite-foreign-keys · Low · High · score 8 · share None
title: Foreign key constraints are declared, but PRAGMA foreign_keys is OFF
summary: This database declares 1 foreign key constraint, but PRAGMA foreign_keys is off, so SQLite ignores them on every insert, update and delete. Enable PRAGMA foreign_keys = ON on each application connection if referential integrity should be enforced.
impact: 1 foreign key unenforced
metric: Foreign keys: 1 Count
fix: Run PRAGMA foreign_keys = ON when opening each connection in the application (SQLite resets this setting per connection). Check existing rows with PRAGMA foreign_key_check before enabling it.
sql: PRAGMA foreign_key_check;
PRAGMA foreign_keys = ON;
verify: Check that every application connection sets PRAGMA foreign_keys = ON after opening the database.

id: config:sqlite-statistics · Low · High · score 8 · share None
title: SQLite has no statistics to choose between this file's indexes
summary: The file has no current sqlite_stat1 statistics, so SQLite falls back on fixed guesses about how big each table is and how well each index narrows a search. With 1 secondary index to choose from, a guess can pick the wrong one, or a full scan. Culprit can't tell each table's size either.
impact: No statistics
metric: Secondary indexes: 1 Count
fix: Run ANALYZE once, then PRAGMA optimize from time to time (when a connection closes is the usual place): it re-analyzes only what changed.
sql: ANALYZE;
cost: ANALYZE reads every index once and holds the write lock while it stores the results: on a large file, run it when writes are quiet.
verify: Scan again: the file has sqlite_stat1, and this finding is gone.

id: config:sqlite-free-pages · Low · High · score 8 · share None
title: 47.0 MB of the file is free pages (71% of it)
summary: Deleted rows leave their pages in the file for SQLite to reuse; the file doesn't shrink on its own. That's fine when new rows will fill them again. If not, the file stays larger than its data, which every backup and copy pays for.
impact: 47.0 MB free
metric: Free pages: 49270784 Bytes
metric: Share of the file: 0.7079630392560767 Ratio
fix: The file already vacuums incrementally: release the free pages now.
sql: PRAGMA incremental_vacuum;
verify: Scan again: free pages are a small part of the file."#,
    );
}

#[test]
fn sqlite_tables_say_which_indexes_lack_statistics() {
    let mut s = fixture("sqlite-demo.json");
    s.server.capabilities.push(culprit_core::Capability::Statistics);
    for i in &mut s.indexes {
        i.has_statistics = Some(false);
    }
    for t in &mut s.tables {
        t.rows = Some(50_000.0);
    }
    let found: Vec<_> = config(&s).into_iter().filter(|f| f.id.starts_with("config:sqlite-statistics:")).collect();
    pin(
        "config_sqlite_index_statistics",
        &found,
        r#"id: config:sqlite-statistics:main.customers · Low · High · score 8 · share None
title: customers has 1 indexes without planner statistics
summary: sqlite_stat1 exists, but these indexes have no entry. Other tables having statistics does not tell SQLite how selective these keys are. This can follow adding an index after maintenance; it does not by itself establish a slow query.
impact: 1 indexes without statistics
metric: Indexes: sqlite_autoindex_customers_1
fix: Analyze this table, then run PRAGMA optimize periodically from the application.
sql: ANALYZE "main"."customers";
cost: Reads the table's indexes and writes planner statistics; schedule it around important writes.
verify: Scan again and check the named indexes have sqlite_stat1 entries. Compare representative query plans and durations; an unchanged access path can be appropriate.

id: config:sqlite-statistics:main.orders · Low · High · score 8 · share None
title: orders has 2 indexes without planner statistics
summary: sqlite_stat1 exists, but these indexes have no entry. Other tables having statistics does not tell SQLite how selective these keys are. This can follow adding an index after maintenance; it does not by itself establish a slow query.
impact: 2 indexes without statistics
metric: Indexes: ix_orders_status_again, ix_orders_status
fix: Analyze this table, then run PRAGMA optimize periodically from the application.
sql: ANALYZE "main"."orders";
cost: Reads the table's indexes and writes planner statistics; schedule it around important writes.
verify: Scan again and check the named indexes have sqlite_stat1 entries. Compare representative query plans and durations; an unchanged access path can be appropriate."#,
    );
}
