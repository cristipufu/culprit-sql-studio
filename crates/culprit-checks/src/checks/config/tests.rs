use super::*;
use crate::waits::snapshot_with;
use culprit_core::{Pressure, Setting, Signal, SignalScope, SignalUnit, TableStorage, WaitStat};

fn set(s: &mut Snapshot, name: &str, value: &str) {
    s.settings.retain(|x| !x.name.eq_ignore_ascii_case(name));
    s.settings.push(Setting { name: name.into(), value: value.into(), default_value: None, scope: "server".into() });
}

fn found<'a>(findings: &'a [Finding], id: &str) -> Option<&'a Finding> {
    findings.iter().find(|f| f.id == id)
}

fn fixture(name: &str) -> Snapshot {
    let text =
        std::fs::read_to_string(format!("{}/../../fixtures/{name}", env!("CARGO_MANIFEST_DIR"))).expect("fixture");
    serde_json::from_str(&text).expect("snapshot")
}

#[test]
fn rcsi_preserves_lock_symptoms_without_claiming_a_proven_isolation_cure() {
    let mut s = fixture("mssql-demo.json");
    let advice = found(&mssql::check(&s), "config:read-committed-snapshot").cloned().unwrap();
    assert_eq!(advice.severity, Severity::Low);
    assert!(advice.title.contains("review row versioning"), "{}", advice.title);
    let sql = advice.fix.as_ref().unwrap().sql.as_deref().unwrap();
    assert!(sql.contains("WITH NO_WAIT"));
    assert!(!sql.contains("ROLLBACK"));

    let w = s.workload.as_mut().unwrap();
    let read = w.queries.iter_mut().find(|q| q.text.starts_with("SELECT")).unwrap();
    read.waits.push(WaitStat { category: "Lock".into(), wait_type: None, wait_ms: 300_000.0, tasks: None });
    w.total_time_ms += 300_000.0;
    let candidate = found(&mssql::check(&s), "config:read-committed-snapshot").cloned().unwrap();
    assert_eq!(candidate.severity, Severity::Medium);
    assert!(candidate.summary.contains("at least 5 min of lock waits"), "{}", candidate.summary);
    assert!(candidate.summary.contains("do not identify the lock mode or blocker"));
    assert!(candidate.fix.as_ref().unwrap().sql.is_none());
    assert!(candidate.fix.as_ref().unwrap().summary.contains("locking hints"));
    assert!(candidate.verify[0].contains("schema lock"));
    assert_eq!(candidate.metrics[0].value, 300_000.0);

    set(&mut s, "snapshot_isolation_state_desc", "ON");
    assert!(found(&mssql::check(&s), "config:read-committed-snapshot").unwrap().fix.as_ref().unwrap().sql.is_none());
    let mut general = fixture("mssql-demo.json");
    set(&mut general, "snapshot_isolation_state_desc", "ON");
    let advice = found(&mssql::check(&general), "config:read-committed-snapshot").cloned().unwrap();
    assert!(advice.fix.unwrap().cost.unwrap().contains("ALLOW_SNAPSHOT_ISOLATION is already on"));
}

#[test]
fn rcsi_candidate_keeps_share_cadence_and_largest_reader_as_observations() {
    let mut s = fixture("mssql-demo.json");
    let w = s.workload.as_mut().unwrap();
    w.window_hours = 146.66;
    let read = w.queries.iter_mut().find(|q| q.text.starts_with("SELECT")).unwrap();
    read.waits.push(WaitStat { category: "Lock".into(), wait_type: None, wait_ms: 71_692.0, tasks: None });
    (read.first_seen, read.last_seen, read.trend) = (None, None, None);
    w.total_time_ms = 335_668.0;
    let f = found(&mssql::check(&s), "config:read-committed-snapshot").cloned().unwrap();
    assert_eq!(f.severity, Severity::Low, "about 12 s a day");
    assert!(f.summary.contains("at least 1 min of lock waits"), "{}", f.summary);
    assert!(f.summary.contains("nearly all for a read from"), "{}", f.summary);
    assert!(f.summary.contains("21% of captured workload time"), "{}", f.summary);
    assert!(f.summary.contains("at least 11.7 s a day"), "{}", f.summary);
    assert!(!f.summary.contains("locks held by writes"), "{}", f.summary);
    assert!(!f.summary.contains("costs little"), "{}", f.summary);
    assert!(f.fix.as_ref().unwrap().sql.is_none());

    let mut s = fixture("mssql-demo.json");
    for q in &mut s.workload.as_mut().unwrap().queries {
        q.waits.clear();
        q.waits_coverage = None;
    }
    let f = found(&mssql::check(&s), "config:read-committed-snapshot").cloned().unwrap();
    assert!(f.summary.contains("does not establish how long reads waited for locks"), "{}", f.summary);
    assert_eq!(f.verify, vec!["Re-scan once the change has taken effect: this finding is gone.".to_string()]);
}

#[test]
fn schema_and_unknown_aggregate_lock_waits_require_resource_investigation_before_setting_sql() {
    for native_type in [Some("LCK_M_SCH_S"), Some("LCK_M_S"), None] {
        let mut s = fixture("mssql-demo.json");
        let w = s.workload.as_mut().unwrap();
        let read = w.queries.iter_mut().find(|query| mssql::row_versioning_candidate(query)).unwrap();
        read.waits = vec![WaitStat {
            category: "Lock".into(),
            wait_type: native_type.map(str::to_owned),
            wait_ms: 300_000.0,
            tasks: None,
        }];
        read.waits_coverage = Some(culprit_core::QueryWaitCoverage {
            classes: vec![culprit_core::telemetry::WaitClass::Lock],
            complete: true,
        });
        w.total_time_ms += 300_000.0;
        let candidate = found(&mssql::check(&s), "config:read-committed-snapshot").cloned().unwrap();
        assert_eq!(candidate.severity, Severity::Medium);
        assert!(candidate.title.contains("investigate the blocking resource"));
        assert!(candidate.summary.contains("do not establish that row versioning would remove"));
        assert!(!candidate.summary.contains("held by writes"));
        assert!(candidate.fix.unwrap().sql.is_none());
        assert!(candidate.verify[0].contains("ordinary READ COMMITTED"));
        assert_eq!(candidate.metrics[0].value, 300_000.0);
    }
}

#[test]
fn a_heuristic_deadlock_advice_link_does_not_establish_a_read_write_blocking_cause() {
    let mut s = fixture("mssql-demo.json");
    let before = found(&mssql::check(&s), "config:read-committed-snapshot").cloned().unwrap();
    let mut deadlock = culprit_core::Deadlock::new("fixture", None, vec![], vec![]);
    deadlock.pattern = culprit_core::DeadlockPattern::KeyLookup;
    deadlock.fixes.push(culprit_core::DeadlockFix {
        finding: Some("config:read-committed-snapshot".into()),
        kind: Some(culprit_core::DeadlockFixKind::ReadCommittedSnapshot),
        ..Default::default()
    });
    s.deadlocks = Some(culprit_core::DeadlockReport { deadlocks: vec![deadlock], ..Default::default() });
    let after = found(&mssql::check(&s), "config:read-committed-snapshot").cloned().unwrap();
    assert_eq!(after.severity, before.severity);
    assert_eq!(after.title, before.title);
    assert_eq!(after.summary, before.summary);
}

#[test]
fn rcsi_absence_requires_qualified_lock_measurements_for_every_captured_read() {
    use culprit_core::{QueryWaitCoverage, telemetry::WaitClass};
    let mut s = fixture("mssql-demo.json");
    let workload = s.workload.as_mut().unwrap();
    let mut read = workload.queries.iter().find(|q| mssql::row_versioning_candidate(q)).unwrap().clone();
    read.waits = vec![WaitStat { category: "IO".into(), wait_type: None, wait_ms: 100.0, tasks: None }];
    read.waits_coverage = Some(QueryWaitCoverage { classes: vec![WaitClass::Lock], complete: true });
    let mut unqualified = read.clone();
    unqualified.id.push_str("-unqualified");
    unqualified.waits_coverage = None;
    workload.queries = vec![read, unqualified];

    let summary = |s: &Snapshot| found(&mssql::check(s), "config:read-committed-snapshot").unwrap().summary.clone();
    assert!(summary(&s).contains("does not establish how long reads waited for locks"));

    let queries = &mut s.workload.as_mut().unwrap().queries;
    queries[1].waits_coverage = queries[0].waits_coverage.clone();
    assert!(summary(&s).contains("No lock waits were recorded for the captured reads considered here"));

    s.workload.as_mut().unwrap().queries.clear();
    assert!(summary(&s).contains("does not establish how long reads waited for locks"));
}

// C6: the threshold is moot where nothing goes parallel; RDS words it as its parameter group.
#[test]
fn cost_threshold_only_where_plans_can_go_parallel() {
    let mut s = fixture("mssql-demo.json");
    let f = found(&mssql::check(&s), "config:cost-threshold-for-parallelism").cloned().unwrap();
    assert!(f.fix.as_ref().unwrap().sql.as_deref().unwrap().contains("show advanced options"));
    set(&mut s, "MAXDOP", "1");
    assert!(found(&mssql::check(&s), "config:cost-threshold-for-parallelism").is_none());
    set(&mut s, "MAXDOP", "0");
    set(&mut s, "max degree of parallelism", "1");
    assert!(found(&mssql::check(&s), "config:cost-threshold-for-parallelism").is_none());
    set(&mut s, "max degree of parallelism", "0");
    s.server.set_platform(Some(culprit_core::Platform::AmazonRds));
    let fix = found(&mssql::check(&s), "config:cost-threshold-for-parallelism").cloned().unwrap().fix.unwrap();
    assert!(fix.sql.is_none() && fix.summary.contains("DB parameter group"), "{}", fix.summary);
}

// C20: Azure SQL reports version 12.0 whatever it runs; the level it offers comes from the service.
#[test]
fn azure_compatibility_level_is_not_read_from_the_version() {
    let mut s = fixture("mssql-demo.json");
    s.server.set_platform(Some(culprit_core::Platform::AzureSqlDatabase));
    s.server.version = "12.0.2000.8".into();
    set(&mut s, "compatibility_level", "150");
    assert!(found(&mssql::check(&s), "config:compatibility-level").is_none(), "150 is one step behind 160");
    set(&mut s, "compatibility_level", "130");
    let f = found(&mssql::check(&s), "config:compatibility-level").cloned().unwrap();
    assert!(f.fix.unwrap().sql.unwrap().contains("COMPATIBILITY_LEVEL = 160"));
    assert!(f.summary.starts_with("Azure SQL Database supports level 160"), "{}", f.summary);
}

// C20: some products need automatic statistics creation off.
#[test]
fn auto_create_statistics_off_is_medium_with_the_vendor_caveat() {
    let mut s = fixture("mssql-demo.json");
    set(&mut s, "is_auto_create_stats_on", "0");
    let f = found(&mssql::check(&s), "config:auto_create_statistics").cloned().unwrap();
    assert_eq!(f.severity, Severity::Medium);
    assert!(f.caveats[0].contains("SharePoint"));
}

// C9: read-only by design (a secondary replica) isn't a size problem; the size limit gets more room, never less.
#[test]
fn query_store_read_only_says_why() {
    let mut s = fixture("mssql-demo.json");
    set(&mut s, "query_store_options.actual_state_desc", "READ_ONLY");
    set(&mut s, "query_store_options.readonly_reason", "8");
    assert!(found(&mssql::check(&s), "config:query-store").is_none());
    set(&mut s, "query_store_options.readonly_reason", "65536");
    set(&mut s, "query_store_options.max_storage_size_mb", "4096");
    let f = found(&mssql::check(&s), "config:query-store").cloned().unwrap();
    assert!(f.summary.contains("reached its size limit"), "{}", f.summary);
    assert!(f.fix.unwrap().sql.unwrap().contains("MAX_STORAGE_SIZE_MB = 8192"));
    set(&mut s, "query_store_options.readonly_reason", "131072");
    let f = found(&mssql::check(&s), "config:query-store").cloned().unwrap();
    assert!(f.fix.unwrap().sql.unwrap().contains("QUERY_CAPTURE_MODE = AUTO"));
}

// C7: managed services refuse ALTER SYSTEM and SET GLOBAL; the advice names their control instead.
#[test]
fn settings_on_managed_services_name_the_platform_control() {
    let mut pg = snapshot_with(Engine::Postgres);
    set(&mut pg, "track_io_timing", "off");
    let fix = found(&postgres::check(&pg), "config:track_io_timing").cloned().unwrap().fix.unwrap();
    assert!(fix.sql.unwrap().starts_with("ALTER SYSTEM SET track_io_timing"));
    pg.server.set_platform(Some(culprit_core::Platform::AmazonRds));
    let fix = found(&postgres::check(&pg), "config:track_io_timing").cloned().unwrap().fix.unwrap();
    assert!(fix.sql.is_none() && fix.summary.contains("DB parameter group"), "{}", fix.summary);

    let mut my = snapshot_with(Engine::Mysql);
    set(&mut my, "slow_query_log", "OFF");
    set(&mut my, "log_output", "FILE");
    let fix = found(&mysql::check(&my), "config:slow_query_log").cloned().unwrap().fix.unwrap();
    assert!(fix.sql.as_deref().unwrap().contains("slow_query_log = 'ON'"), "{:?}", fix.sql);
    my.server.set_platform(Some(culprit_core::Platform::GoogleCloudSql));
    let fix = found(&mysql::check(&my), "config:slow_query_log").cloned().unwrap().fix.unwrap();
    assert!(fix.sql.is_none());
    assert!(
        fix.summary.contains("Set slow_query_log to ON, long_query_time to 1 and log_output to FILE,TABLE in the instance's database flags"),
        "{}",
        fix.summary
    );
    // MY-10: MySQL's lock time is the same either way; MariaDB's, written to a table, is the whole logged run.
    assert_eq!(fix.cost, None);
    my.server.set_platform(None);
    my.server.product = "MariaDB".into();
    let fix = found(&mysql::check(&my), "config:slow_query_log").cloned().unwrap().fix.unwrap();
    let cost = fix.cost.unwrap_or_default();
    assert!(
        cost.starts_with("Written to a table (TABLE in log_output), MariaDB counts the whole run of each statement it logs as that statement's lock time"),
        "{cost}"
    );
    assert!(cost.contains("Leaving TABLE out keeps lock time real"), "{cost}");
}

fn buffer_pool_snapshot(hit: f64, reads: f64) -> Snapshot {
    let mut s = snapshot_with(Engine::Mysql);
    for t in &mut s.tables {
        t.total_bytes = Some(40.0 * 1024.0 * 1024.0 * 1024.0);
    }
    set(&mut s, "innodb_buffer_pool_size", &(8u64 * 1024 * 1024 * 1024).to_string());
    s.pressure = Some(Pressure {
        uptime_hours: Some(72.0),
        signals: vec![
            Signal::new("buffer-pool-hit", "hit", hit, SignalUnit::Percent, SignalScope::SinceStartup),
            Signal::new("buffer-pool-reads", "reads", reads, SignalUnit::PerSecond, SignalScope::SinceStartup),
        ],
        ..Pressure::default()
    });
    s
}

// C8: a large, mostly cold schema served from memory 99.9% of the time needs no bigger pool; when one is needed,
// no size is proposed without knowing the server's memory.
#[test]
fn buffer_pool_is_graded_on_misses_not_data_size() {
    assert!(mysql::buffer_pool_check(&buffer_pool_snapshot(99.9, 3.0)).is_none());
    let f = mysql::buffer_pool_check(&buffer_pool_snapshot(90.0, 400.0)).unwrap();
    assert_eq!(f.severity, Severity::Low);
    assert!(f.fix.as_ref().unwrap().sql.is_none());
    assert_eq!(mysql::buffer_pool_check(&buffer_pool_snapshot(97.0, 400.0)).unwrap().severity, Severity::Low);
    // MyISAM tables (a primary key that isn't clustered) have no pages in the buffer pool.
    let mut s = buffer_pool_snapshot(90.0, 400.0);
    for i in &mut s.indexes {
        i.is_clustered = false;
    }
    for t in &mut s.tables {
        t.has_primary_key = true;
    }
    assert!(mysql::buffer_pool_check(&s).is_none());
}

// C9: with size-based cleanup on AUTO, Query Store trims itself at 90% and never fills.
#[test]
fn a_query_store_that_cleans_itself_is_never_too_full() {
    let mut s = snapshot_with(Engine::Mssql);
    set(&mut s, "query_store_options.current_storage_size_mb", "950");
    set(&mut s, "query_store_options.max_storage_size_mb", "1000");
    set(&mut s, "query_store_options.size_based_cleanup_mode_desc", "AUTO");
    assert!(found(&mssql::check(&s), "config:query-store-size").is_none());
    set(&mut s, "query_store_options.size_based_cleanup_mode_desc", "OFF");
    assert!(found(&mssql::check(&s), "config:query-store-size").is_some());
}

// C6: MAXDOP 0 is Microsoft's own advice up to 8 processors; above that the cap fits one NUMA node.
#[test]
fn maxdop_zero_matters_only_on_a_bigger_server() {
    let mut s = snapshot_with(Engine::Mssql);
    set(&mut s, "max degree of parallelism", "0");
    set(&mut s, "MAXDOP", "0");
    s.pressure = None;
    assert!(found(&mssql::check(&s), "config:maxdop").is_none(), "unknown processor count: nothing to judge");
    s.pressure = Some(Pressure { cpus: Some(4.0), numa_nodes: Some(1.0), ..Pressure::default() });
    assert!(found(&mssql::check(&s), "config:maxdop").is_none(), "4 processors: MAXDOP 0 is fine");
    s.pressure = Some(Pressure { cpus: Some(24.0), numa_nodes: Some(2.0), ..Pressure::default() });
    let f = found(&mssql::check(&s), "config:maxdop").cloned().unwrap();
    assert!(f.title.contains("all 24 processors"), "{}", f.title);
    assert!(f.fix.unwrap().sql.unwrap().contains("MAXDOP = 8"));
    s.pressure = Some(Pressure { cpus: Some(12.0), numa_nodes: Some(2.0), ..Pressure::default() });
    assert!(
        found(&mssql::check(&s), "config:maxdop").cloned().unwrap().fix.unwrap().sql.unwrap().contains("MAXDOP = 6")
    );
}

// C16: only big tables that churn and follow the server's factor; the title says the factor in use.
#[test]
fn autovacuum_scale_skips_append_only_tables_and_their_own_settings() {
    let mut s = snapshot_with(Engine::Postgres);
    set(&mut s, "autovacuum_vacuum_scale_factor", "0.3");
    for t in &mut s.tables {
        t.rows = Some(1_000.0);
    }
    let t = &mut s.tables[0];
    t.rows = Some(20_000_000.0);
    t.updates = Some(0.0);
    t.deletes = Some(0.0);
    assert!(found(&postgres::check(&s), "config:autovacuum-scale").is_none(), "append-only: no dead rows to wait for");
    s.tables[0].updates = Some(2_000_000.0);
    let f = found(&postgres::check(&s), "config:autovacuum-scale").cloned().unwrap();
    assert!(f.title.starts_with("Autovacuum waits for 30% of"), "{}", f.title);
    s.tables[0].storage =
        Some(TableStorage { options: vec!["autovacuum_vacuum_scale_factor=0.01".into()], ..Default::default() });
    assert!(found(&postgres::check(&s), "config:autovacuum-scale").is_none(), "it has its own factor");
}

// C7: ALTER SYSTEM is refused on managed PostgreSQL; never propose a smaller pg_stat_statements.max.
#[test]
fn postgres_settings_are_worded_where_they_run() {
    let mut s = snapshot_with(Engine::Postgres);
    set(&mut s, "autovacuum", "off");
    set(&mut s, "pg_stat_statements.dealloc", "40");
    set(&mut s, "pg_stat_statements.max", "50000");
    let out = postgres::check(&s);
    assert!(
        found(&out, "config:autovacuum")
            .unwrap()
            .fix
            .as_ref()
            .unwrap()
            .sql
            .as_deref()
            .unwrap()
            .starts_with("ALTER SYSTEM SET autovacuum")
    );
    let max = found(&out, "config:pg_stat_statements.max").unwrap().fix.clone().unwrap();
    assert!(max.sql.unwrap().contains("pg_stat_statements.max = 100000"));
    s.server.set_platform(Some(culprit_core::Platform::AzureDatabase));
    let fix = found(&postgres::check(&s), "config:autovacuum").unwrap().fix.clone().unwrap();
    assert!(fix.sql.is_none() && fix.summary.contains("server parameters"), "{}", fix.summary);
}

// C7: with performance_schema off on RDS, the fix is the parameter group, not SET GLOBAL and my.cnf.
#[test]
fn mysql_restart_settings_on_a_managed_server() {
    let mut s = snapshot_with(Engine::Mysql);
    set(&mut s, "performance_schema", "OFF");
    set(&mut s, "slow_query_log", "OFF");
    let fix = found(&mysql::check(&s), "config:performance_schema").unwrap().fix.clone().unwrap();
    assert!(fix.sql.as_deref().unwrap().contains("[mysqld]\nperformance_schema = ON"));
    s.server.set_platform(Some(culprit_core::Platform::AmazonRds));
    let fix = found(&mysql::check(&s), "config:performance_schema").unwrap().fix.clone().unwrap();
    assert!(fix.sql.is_none(), "{:?}", fix.sql);
    assert!(fix.summary.contains("in the instance's DB parameter group"), "{}", fix.summary);
    assert!(fix.summary.contains("after a reboot"));
    // Where the setting is set is a sentence of its own in each language: German's keeps its capitals.
    culprit_core::i18n::scope("de", || {
        let fix = found(&mysql::check(&s), "config:performance_schema").unwrap().fix.clone().unwrap();
        assert!(fix.summary.contains("zu erhalten: Setzen Sie performance_schema in "), "{}", fix.summary);
    });
}

#[test]
fn sqlite_foreign_keys_off_when_declared_and_recorded() {
    let mut s: Snapshot = serde_json::from_str(include_str!("../../../../../fixtures/sqlite-demo.json")).unwrap();
    assert!(!s.foreign_keys.is_empty());
    assert!(found(&sqlite::check(&s), "config:sqlite-foreign-keys").is_none());
    set(&mut s, "foreign_keys", "0");
    let out = sqlite::check(&s);
    let f = found(&out, "config:sqlite-foreign-keys").expect("foreign_keys off");
    assert!(f.title.contains("PRAGMA foreign_keys is OFF"), "{}", f.title);
    assert!(f.fix.as_ref().unwrap().sql.as_deref().unwrap().contains("PRAGMA foreign_keys = ON;"));
    set(&mut s, "foreign_keys", "1");
    assert!(found(&sqlite::check(&s), "config:sqlite-foreign-keys").is_none());
}

#[test]
fn postgres_wal_buffers_full_and_mysql_buffer_pool_wait_free() {
    let mut pg = snapshot_with(Engine::Postgres);
    pg.pressure.get_or_insert_with(Default::default).uptime_hours = Some(24.0);
    pg.pressure.as_mut().unwrap().signals.push(culprit_core::Signal::new(
        "wal-buffers-full",
        "WAL buffer full writes",
        30_000.0,
        culprit_core::SignalUnit::Count,
        culprit_core::SignalScope::SinceStartup,
    ));
    set(&mut pg, "wal_buffers", "16MB");
    let out = postgres::check(&pg);
    let f = found(&out, "config:wal_buffers").expect("wal_buffers finding");
    assert!(f.title.contains("WAL buffers filled 30K times"), "{}", f.title);
    assert!(f.fix.as_ref().unwrap().sql.as_deref().unwrap().contains("wal_buffers = '64MB'"));
    set(&mut pg, "wal_buffers", "64MB");
    assert!(found(&postgres::check(&pg), "config:wal_buffers").is_none());

    let mut my = snapshot_with(Engine::Mysql);
    my.pressure.get_or_insert_with(Default::default).uptime_hours = Some(24.0);
    my.pressure.as_mut().unwrap().signals.push(culprit_core::Signal::new(
        "buffer-pool-wait-free",
        "Buffer pool free-page waits",
        500.0,
        culprit_core::SignalUnit::Count,
        culprit_core::SignalScope::SinceStartup,
    ));
    let out = mysql::check(&my);
    let f = found(&out, "config:innodb_buffer_pool_wait_free").expect("innodb_buffer_pool_wait_free finding");
    assert!(f.title.contains("Queries waited 500 times for a clean buffer-pool page"), "{}", f.title);
}
