use super::predicates::is_value;
use super::*;

#[test]
fn a_counter_rate_requires_its_own_valid_epoch_and_enough_observation_time() {
    use culprit_core::ResetMarker;
    let mut snapshot = crate::waits::snapshot_with(Engine::Mssql);
    snapshot.taken_at = "2026-10-11T12:00:00Z".into();
    snapshot.server.started_at = Some("2026-10-01T00:00:00Z".into());
    for reset in [
        ResetMarker::Unknown,
        ResetMarker::Generation("epoch-1".into()),
        ResetMarker::At("invalid".into()),
        ResetMarker::At("2026-10-11T13:00:00Z".into()),
        ResetMarker::At(snapshot.taken_at.clone()),
        ResetMarker::At("2026-10-11T11:30:00Z".into()),
    ] {
        assert_eq!(counter_rate_per_day_since(&snapshot, 12.0, &reset), None, "{reset:?}");
    }
    let reset = ResetMarker::At("2026-10-11T00:00:00Z".into());
    assert_eq!(counter_rate_per_day_since(&snapshot, 12.0, &reset), Some(24.0));
    for total in [-1.0, f64::NAN, f64::INFINITY] {
        assert_eq!(counter_rate_per_day_since(&snapshot, total, &reset), None);
    }
}

/// A name a database allows (a quoted identifier with a line break in it) can't end a comment of fix SQL and run the
/// rest of itself as a statement.
#[test]
fn a_name_in_a_comment_stays_in_the_comment() {
    for engine in [Engine::Mssql, Engine::Postgres, Engine::Mysql, Engine::Sqlite] {
        let hostile = "orders\nDROP TABLE users;\r\n--";
        let line = format!("-- read {}", comment_text(&qualified(engine, "app", hostile)));
        assert_eq!(line.lines().count(), 1, "{engine:?}: {line:?}");
        assert!(line.starts_with("-- read "), "{line}");
    }
    assert_eq!(comment_text("a\u{2028}b\u{85}c\u{2029}d\te"), "a b c d e");
    assert_eq!(comment_text("plain [app].[orders]"), "plain [app].[orders]");
}

/// Names and statements come from the database in any script: every text helper slices only at character
/// boundaries, so none of them can panic on a scan.
#[test]
fn text_helpers_never_panic_on_non_ascii() {
    let texts = [
        "SELECT * FROM [dbo].[Bestellungen_Übersicht] AS b JOIN İstanbul_şube s ON s.id = b.id WHERE b.ñ = N'ç' AND b.x::text LIKE 'é%'",
        "UPDATE 顧客 SET 名前 = @p1 WHERE ид = @p2",
        "INSERT INTO `tábla` (`ő`) VALUES (?)",
        "CALL `prøcédure`(?)",
        "ǅ İ ß ﬁ 𝔘 :: AS İ AND ǅ",
        "",
    ];
    for text in texts {
        let _ = mentions_word(text, "i̇stanbul_şube");
        let _ = mentions_word(text, "ß");
        let _ = predicate_columns(text);
        let _ = parse_epoch_seconds(text);
        let _ = parse_epoch_seconds("2026-10-14T08:54:29+İ");
        // Out-of-range parts from a hostile saved report: no overflow, just not a time.
        assert_eq!(parse_epoch_seconds("9223372036854775807-12-31T00:00:00Z"), None);
        assert_eq!(parse_epoch_seconds("2026-9223372036854775807-01T00:00:00Z"), None);
        assert_eq!(parse_epoch_seconds("2026-10-14Tinf:00:00Z"), None);
        assert!(parse_epoch_seconds("2026-10-14T08:54:29Z").is_some());
        let mut q = crate::waits::snapshot_with(Engine::Mssql).queries()[0].clone();
        q.text = text.into();
        let _ = query_handle(&q);
    }
    let server = crate::waits::snapshot_with(Engine::Postgres).server;
    let _ = setting_change(&server, "work_mem", "64MB ünï");
}

// The aptabase readings: random_page_cost can be set for one database, which RDS allows at once and without a
// custom parameter group; the parameter group re-plans every database on the instance. Settings only a superuser
// sets per database (track_io_timing), and those that are the server's alone (pg_stat_statements.max), stay the
// platform's.
#[test]
fn a_per_database_setting_is_set_for_this_database_first_on_a_managed_server() {
    let mut server = crate::waits::snapshot_with(Engine::Postgres).server;
    server.set_platform(Some(culprit_core::Platform::AmazonRds));
    let change = setting_change(&server, "random_page_cost", "1.1");
    assert_eq!(change.sql.as_deref(), Some("ALTER DATABASE demo SET random_page_cost = 1.1;"));
    assert!(change.how.starts_with("Set random_page_cost to 1.1 for this database only"), "{}", change.how);
    assert!(change.how.contains("sessions that connect after the change use it"), "{}", change.how);
    assert!(change.how.ends_with("set it in the instance's DB parameter group."), "{}", change.how);
    for name in ["track_io_timing", "pg_stat_statements.max", "autovacuum"] {
        let change = setting_change(&server, name, "on");
        assert!(change.sql.is_none() && change.how.contains("DB parameter group"), "{name}: {}", change.how);
    }
    // A name that needs quoting is quoted.
    server.database = "Shop-EU".into();
    let change = setting_change(&server, "work_mem", "64MB");
    assert_eq!(change.sql.as_deref(), Some("ALTER DATABASE \"Shop-EU\" SET work_mem = '64MB';"));
    // A server you run takes ALTER SYSTEM, as before; MySQL keeps no settings per database.
    server.set_platform(None);
    let change = setting_change(&server, "random_page_cost", "1.1");
    assert!(change.sql.unwrap().starts_with("ALTER SYSTEM SET random_page_cost = 1.1;"));
    let mut mysql = crate::waits::snapshot_with(Engine::Mysql).server;
    mysql.set_platform(Some(culprit_core::Platform::AmazonRds));
    let change = setting_change(&mysql, "sort_buffer_size", "4194304");
    assert!(change.sql.is_none() && change.how.contains("DB parameter group"), "{}", change.how);
}

// Theme A's note on missing-index: a request's grade took the statements' share over Culprit's scans with the time
// their totals hold. A share is graded on the basis it is on: over the scans, the amount a day it is of what the
// database ran in them.
#[test]
fn a_share_over_the_scans_is_graded_on_their_amount_not_the_totals() {
    let path = format!("{}/../../fixtures/postgres-demo.json", env!("CARGO_MANIFEST_DIR"));
    let mut s: Snapshot = serde_json::from_str(&std::fs::read_to_string(path).expect("fixture")).expect("snapshot");
    let w = s.workload.as_mut().expect("workload");
    // 40 days of totals; 36 hours of scans, in which the database ran 10 s a 12-hour step and the statement 3 s.
    (w.window_start, w.window_hours) = (Some("2026-09-01T11:54:21Z".into()), 960.0);
    w.timeline = Some(
        serde_json::from_value(serde_json::json!({
            "source": "scans",
            "starts": ["2026-10-10T00:00:00Z", "2026-10-10T12:00:00Z", "2026-10-11T00:00:00Z"],
            "end": "2026-10-11T12:00:00Z",
            "time_ms": [10000.0, 10000.0, 10000.0],
            "executions": [5000.0, 5000.0, 5000.0]
        }))
        .expect("timeline"),
    );
    let q = &mut w.queries[0];
    q.trend = Some(culprit_core::QueryTrend { time_ms: vec![Some(3_000.0); 3], executions: vec![Some(10.0); 3] });
    // Its totals: 1,000 runs of a minute since the reset, 25 minutes a day and 30% of the database's.
    (q.executions, q.mean_time_ms, q.total_time_ms) = (1_000.0, 60_000.0, 60_000_000.0);
    w.total_time_ms = 200_000_000.0;
    let q = s.workload.as_ref().unwrap().queries[0].clone();
    assert!(s.workload.as_ref().unwrap().scans_of(&q).is_some_and(|s| !s.is_recent(&q)));
    // 30% of the scans' time is 6 s a day of a database that ran 20 s a day in them: Low, however much time the
    // totals hold; the totals' time beside the scans' share made it High.
    assert_eq!(severity_for_statement_share(&s, &[&q], 0.3), Severity::Low);
    assert_eq!(amount_severity_of(&s, &[&q], 0.3), Severity::Low);
    assert_eq!(severity_for_time_of(&s, &[&q], q.total_time_ms, 0.3), Severity::High, "the old mix");
    // Without the scans, the totals are the basis of both.
    let mut totals = s.clone();
    totals.workload.as_mut().unwrap().timeline = None;
    assert_eq!(severity_for_statement_share(&totals, &[&q], 0.3), Severity::High);
}

// D's open gap (MY-5): performance_schema's table counters start over with its statements, so "no writes" waits a
// week of the digests' own counting, however long the server has been up; then it is said as counted.
#[test]
fn no_writes_on_counters_that_may_be_reset_waits_a_week_of_their_own() {
    let path = format!("{}/../../fixtures/mysql-demo.json", env!("CARGO_MANIFEST_DIR"));
    let mut s: Snapshot = serde_json::from_str(&std::fs::read_to_string(path).expect("fixture")).expect("snapshot");
    let taken = parse_epoch_seconds(&s.taken_at).expect("taken_at");
    s.server.started_at = Some(culprit_core::format::iso_from_epoch(taken - 10.0 * 86_400.0));
    s.pressure.get_or_insert_with(Default::default).uptime_hours = Some(240.0);
    let digests = |s: &mut Snapshot, days: f64| {
        let w = s.workload.as_mut().expect("workload");
        w.window_start = Some(culprit_core::format::iso_from_epoch(taken - days * 86_400.0));
        w.window_hours = days * 24.0;
    };
    digests(&mut s, 2.0);
    assert!(counters_may_be_reset(&s) && watched_days(&s) == Some(2.0));
    let cost = crate::indexes::write_cost(&s, "shop", "orders").expect("a cost");
    assert!(cost.contains("in what its counters cover, which may be short"), "{cost}");
    digests(&mut s, 8.0);
    let cost = crate::indexes::write_cost(&s, "shop", "orders").expect("a cost");
    assert_eq!(
        cost,
        "shop.orders received no writes since the server started or its performance_schema counters were last \
         reset, so this index would cost next to nothing to maintain."
    );
}

#[test]
fn usage_windows_say_how_long_the_server_has_been_up() {
    let mut s = crate::waits::snapshot_with(Engine::Mssql);
    s.taken_at = "2026-10-11T17:05:00Z".into();
    s.pressure = None;
    let mut at = |started: &str| {
        s.server.started_at = Some(started.into());
        since_phrase(&s)
    };
    assert_eq!(at("2026-10-11T16:49:00Z"), "in the 16 minutes since the server last restarted");
    assert_eq!(at("2026-10-11T16:05:00Z"), "in the hour since the server last restarted");
    assert_eq!(at("2026-10-11T12:05:00Z"), "in the 5 hours since the server last restarted");
    assert_eq!(at("2026-10-01T17:05:00Z"), "in the 10 days since the server last restarted");
    // A staging SQL Server reading: 9.51 days of counters were "the 10 days", more than they counted.
    assert_eq!(at("2026-10-02T04:50:36Z"), "in the 9.5 days since the server last restarted");
    assert_eq!(at("2026-10-02T05:05:00Z"), "in the 9.5 days since the server last restarted");
    assert_eq!(at("2026-10-09T17:05:00Z"), "in the 2 days since the server last restarted");
    assert_eq!(at("2026-09-28T23:53:00Z"), "in the 12 days since the server last restarted");
}

#[test]
fn postgres_counters_that_outlived_a_restart_say_so() {
    // The server restarted 5 hours ago, but its statistics were last reset 10.9 hours ago and survived.
    let mut s = crate::waits::snapshot_with(Engine::Postgres);
    s.taken_at = "2026-10-11T17:00:00Z".into();
    s.server.started_at = Some("2026-10-11T12:00:00Z".into());
    s.pressure = Some(culprit_core::Pressure { uptime_hours: Some(10.9), ..Default::default() });
    assert_eq!(since_phrase(&s), "in the 11 hours since its statistics were last reset");
    assert!((counter_days(&s).unwrap() * 24.0 - 10.9).abs() < 1e-9);
    // Counters as old as the server: a restart, as before.
    s.pressure = Some(culprit_core::Pressure { uptime_hours: Some(5.0), ..Default::default() });
    assert_eq!(since_phrase(&s), "in the 5 hours since the server last restarted");
}

#[test]
fn counts_are_per_day_only_over_a_day_of_counters() {
    let mut s = crate::waits::snapshot_with(Engine::Mssql);
    s.taken_at = "2026-10-11T17:05:00Z".into();
    s.pressure = None;
    s.server.started_at = Some("2026-10-11T12:05:00Z".into());
    let days = counter_days(&s).unwrap();
    // 5 hours: what was counted, not "~1.6K times a day".
    assert_eq!(
        how_many(&s, 340.0, days, culprit_core::key!("common.noun.time")),
        "340 times in the 5 hours since the server last restarted"
    );
    s.server.started_at = Some("2026-10-09T17:05:00Z".into());
    let days = counter_days(&s).unwrap();
    assert_eq!(how_many(&s, 340.0, days, culprit_core::key!("common.noun.time")), "about 170 times a day");
}

#[test]
fn system_schema_reads_are_monitoring_on_every_engine() {
    assert!(is_monitoring_text(
        "SELECT `EVENT_NAME` FROM `performance_schema` . `events_waits_summary_global_by_event_name`"
    ));
    assert!(is_monitoring_text("SELECT * FROM mysql.slow_log WHERE db = ?"));
    assert!(is_monitoring_text("SELECT * FROM sys.innodb_lock_waits"));
    assert!(is_monitoring_text("SELECT * FROM `information_schema` . `TABLES`"));
    assert!(!is_monitoring_text("SELECT `id` FROM `orders` WHERE `customer_id` = ?"));
    assert!(!is_monitoring_text("SELECT analysis.id FROM analysis WHERE mysqlish = 1"));
}

#[test]
fn native_internal_catalog_reads_are_not_application_queries() {
    let mut s: Snapshot = serde_json::from_str(include_str!("../../../../fixtures/yugabyte-demo.json")).unwrap();
    let workload = s.workload.as_mut().unwrap();
    let mut catalog = workload.queries[0].clone();
    catalog.id = "internal-catalog-read".into();
    catalog.text = "SELECT c.reltuples, o.option_name FROM pg_class c LEFT JOIN LATERAL pg_options_to_table(c.reloptions) o ON true".into();
    catalog.plan = None;
    workload.queries = vec![catalog];
    assert_eq!(app_queries(&s).count(), 0);
    s.workload.as_mut().unwrap().queries[0].text = "SELECT 'pg_class', pg_class_hint FROM public.orders".into();
    assert_eq!(app_queries(&s).count(), 1);
    s.workload.as_mut().unwrap().queries[0].text = "SELECT * FROM public.pg_class".into();
    assert_eq!(app_queries(&s).count(), 1);
    s.workload.as_mut().unwrap().queries[0].text = "SELECT 'pg_class' FROM \"PG_CLASS\"".into();
    assert_eq!(app_queries(&s).count(), 1);
    s.server.engine = Engine::Mssql;
    s.workload.as_mut().unwrap().queries[0].text = "SELECT * FROM pg_class".into();
    assert_eq!(app_queries(&s).count(), 1);
}

#[test]
fn epoch_parsing_handles_offsets() {
    let a = parse_epoch_seconds("2026-10-01T00:00:00Z").unwrap();
    let b = parse_epoch_seconds("2026-10-01T02:00:00+02:00").unwrap();
    assert_eq!(a, b);
    assert_eq!(days_between("2026-10-01T00:00:00Z", "2026-10-11 12:00:00").unwrap(), 10.5);
}

#[test]
fn word_matching() {
    assert!(mentions_word("SELECT * FROM [dbo].[Orders] m", "orders"));
    assert!(!mentions_word("SELECT * FROM OrdersArchive", "orders"));
}

#[test]
fn query_handles() {
    let mut q = sample();
    q.text = "SELECT TOP 10 * FROM [dbo].[Orders] WHERE x = @p1".into();
    assert_eq!(query_handle(&q), "a read from Orders");
    q.text = "UPDATE dbo.Users SET a = 1".into();
    assert_eq!(query_handle(&q), "an update of Users");
    q.text = "UPDATE STATISTICS dbo.[Orders] WITH SAMPLE 100 PERCENT".into();
    assert_eq!(query_handle(&q), "a statistics refresh on Orders");
    q.text = "UPDATE\nSTATISTICS dbo.[Orders]".into();
    assert_eq!(query_handle(&q), "a statistics refresh on Orders");
    q.text = "(@P1 int) UPDATE STATISTICS dbo.[Orders]".into();
    assert_eq!(query_handle(&q), "a statistics refresh on Orders");
    assert!(statistics_maintenance_table("UPDATE dbo.Statistics SET value = 1").is_none());
    assert!(statistics_maintenance_table("UPDATE statistics_log SET value = 1").is_none());
    assert!(statistics_maintenance_table("UPDATE statistics SET value = 1").is_none());
    assert!(statistics_maintenance_table("UPDATE statistics s SET value = 1").is_none());
    assert!(statistics_maintenance_table("UPDATE statistics AS s SET value = 1").is_none());
    q.text = "UPDATE TOP (1000) dbo.[Orders] SET state = 1".into();
    assert_eq!(query_handle(&q), "an update of Orders");
    q.text = "UPDATE TOP (@limit) dbo.[Orders] SET state = 1".into();
    assert_eq!(query_handle(&q), "an update of Orders");
    q.text = "UPDATE TOP 10 PERCENT dbo.[Orders] SET state = 1".into();
    assert_eq!(query_handle(&q), "an update of Orders");
    q.text = "UPDATE top100 SET state = 1".into();
    assert_eq!(query_handle(&q), "an update of top100");
}

pub(crate) fn sample() -> QueryStat {
    QueryStat {
        plan_history: None,
        resolved_write_target: None,
        requests: None,
        id: "1".into(),
        query_hash: None,
        text: String::new(),
        parameters: None,
        object_name: None,
        executions: 1.0,
        total_time_ms: 1.0,
        mean_time_ms: 1.0,
        max_time_ms: None,
        stddev_time_ms: None,
        cpu_time_ms: None,
        rows: None,
        logical_reads: None,
        physical_reads: None,
        temp_writes: None,
        first_seen: None,
        last_seen: None,
        plans: Vec::new(),
        plan: None,
        waits: Vec::new(),
        trend: None,
        spread: None,
        costs: None,
        rows_affected: None,
        plan_id: None,
    }
}

#[test]
fn statements_without_a_table_are_named() {
    assert_eq!(statement_handle("DO SLEEP(?)"), "a call to SLEEP");
    assert_eq!(statement_handle("DISCARD ALL"), "DISCARD ALL");
    // A write behind a WITH list is the write, wherever its CTEs read.
    assert_eq!(statement_handle("WITH b AS (SELECT id FROM jobs) DELETE FROM jobs USING b"), "a delete from jobs");
    assert_eq!(statement_handle("WITH b AS (SELECT 'delete' FROM x) UPDATE t SET a = 1"), "an update of t");
    assert_eq!(statement_handle("SELECT sleep(?)"), "a call to sleep");
    assert_eq!(statement_handle("SELECT pg_sleep($1)"), "a call to pg_sleep");
    assert_eq!(statement_handle("DO `SLEEP` (?)"), "a call to SLEEP");
    assert_eq!(statement_handle("CALL `refresh_totals` (?)"), "a call to refresh_totals");
    assert_eq!(statement_handle("EXEC [dbo].[usp_orders] @id = 1"), "a call to dbo.usp_orders");
    assert_eq!(statement_handle("CALL shop.refresh(?)"), "a call to shop.refresh");
    assert_eq!(statement_handle("CALL refresh_totals(?)"), "a call to refresh_totals");
    assert_eq!(statement_handle("SELECT id FROM orders WHERE id = ?"), "a read from orders");
    // A pool's ping is named as it is.
    assert_eq!(statement_handle("SELECT 1"), "SELECT 1");
    assert_eq!(
        statement_handle("SELECT count(*) FROM (VALUES ($1),($2)) v(x) JOIN orders o ON o.id = v.x"),
        "a read from orders"
    );
    assert_eq!(statement_handle("SELECT count(*) FROM (SELECT 1) s"), "a read");
}

#[test]
fn mysql_online_ddl_options_are_space_separated() {
    // `ALGORITHM=INPLACE, LOCK=NONE` is ERROR 1064 in CREATE/DROP INDEX on MySQL and MariaDB: index options don't
    // take commas (only ALTER TABLE's clauses do).
    // MariaDB's DROP INDEX has no such options at all, so the drop goes through ALTER TABLE (whose clauses do).
    let drop = Engine::Mysql.dialect().drop_index("shop", "orders", "ix_orders_status");
    assert_eq!(drop, "ALTER TABLE `shop`.`orders` DROP INDEX `ix_orders_status`, ALGORITHM=INPLACE, LOCK=NONE;");
}

#[test]
fn postgres_predicates() {
    let p = predicate_columns("(customer_id = $1)");
    assert_eq!(p.equality, vec!["customer_id"]);
    let p = predicate_columns("(((status)::text = 'open'::text) AND (created_at > (now() - '7 days'::interval)))");
    assert_eq!(p.equality, vec!["status"]);
    assert_eq!(p.range, vec!["created_at"]);
    let p = predicate_columns("(o.customer_id = c.id)");
    assert!(p.equality.is_empty());
}

#[test]
fn sql_server_predicates() {
    let p = predicate_columns(
        "[shop].[dbo].[Orders].[CustomerId] as [m].[CustomerId]=[@P1] AND [shop].[dbo].[Orders].[CreatedAt] as [m].[CreatedAt]>=[@P2]",
    );
    assert_eq!(p.equality, vec!["CustomerId"]);
    assert_eq!(p.range, vec!["CreatedAt"]);
    let p = predicate_columns("[shop].[dbo].[Users].[TenantId]=CONVERT_IMPLICIT(uniqueidentifier,[@P1],0)");
    assert_eq!(p.equality, vec!["TenantId"]);
}

// A function of parameters and constants is a value (the planted shipped_at range on SQL Server), and a function of
// a column isn't.
#[test]
fn a_function_of_values_is_a_value() {
    let p = predicate_columns(
        "[culprit_demo].[shop].[orders].[shipped_at]>=dateadd(day,[@P1],'2026-01-01 00:00:00.000') AND \
         [culprit_demo].[shop].[orders].[shipped_at]<dateadd(day,[@P2],'2026-01-01 00:00:00.000')",
    );
    assert_eq!(p.range, vec!["shipped_at"]);
    assert_eq!(predicate_columns("(total > COALESCE($1, 0))").range, vec!["total"]);
    assert_eq!(predicate_columns("(`o`.`created_at` > DATE_SUB(?, INTERVAL ? DAY))").range, vec!["created_at"]);
    assert_eq!(predicate_columns("([o].[due]>dateadd(day,(1),[o].[created_at]))"), PredicateColumns::default());
    assert_eq!(predicate_columns("(name = upper(other_name))"), PredicateColumns::default());
}

// An index on one branch of an OR can't find the other branch's rows, and a default b-tree can't seek not-equal,
// NOT, a pattern starting with a wildcard, or PostgreSQL's LIKE.
#[test]
fn only_conditions_an_index_can_seek_name_columns() {
    let p = predicate_columns("((status)::text = 'open'::text) OR (priority > 3)");
    assert_eq!(p, PredicateColumns::default());
    let p = predicate_columns("[db].[dbo].[t].[status]='open' OR [db].[dbo].[t].[priority]>(3)");
    assert_eq!(p, PredicateColumns::default());
    // A condition every row meets still counts next to an OR.
    let p = predicate_columns("((customer_id = $1) AND ((status = 'open') OR (priority > 3)))");
    assert_eq!(p.equality, vec!["customer_id"]);
    assert!(p.range.is_empty());
    // Parentheses decide: `a OR (b AND c)` requires nothing of every row.
    let p = predicate_columns("((a = 1) OR ((b = 2) AND (c = 3)))");
    assert_eq!(p, PredicateColumns::default());
    // The false flag: a cast of the column to date hides it from an index on it, like date(created_at) would.
    let p = predicate_columns("((created_at)::date = $1)");
    assert!(p.equality.is_empty() && p.range.is_empty(), "{p:?}");
    let p = predicate_columns("((amount)::integer > 10) AND (created_at > '2026-01-01'::date) AND (id = $1::bigint)");
    assert_eq!((p.equality, p.range), (vec!["id".to_string()], vec!["created_at".to_string()]));
    let p = predicate_columns("((status)::text <> 'closed'::text)");
    assert_eq!(p, PredicateColumns::default());
    let p = predicate_columns("((email)::text ~~ 'abc%'::text)");
    assert_eq!(p, PredicateColumns::default());
    let p = predicate_columns("(NOT (customer_id = $1))");
    assert_eq!(p, PredicateColumns::default());
    let p = predicate_columns("(shop.orders.status not in ('a','b'))");
    assert_eq!(p, PredicateColumns::default());
    // SQL Server and MySQL seek a LIKE with a fixed start, never one that starts with a wildcard.
    let p = predicate_columns("[db].[dbo].[t].[email] like N'abc%'");
    assert_eq!(p.range, vec!["email"]);
    let p = predicate_columns("[db].[dbo].[t].[email] like N'%abc'");
    assert_eq!(p, PredicateColumns::default());
    // MySQL: typed literals, BETWEEN and the null-safe equal.
    let p = predicate_columns(
        "((shop.orders.status = 'shipped') and (shop.orders.created_at >= TIMESTAMP'2026-09-05 00:00:00'))",
    );
    assert_eq!((p.equality, p.range), (vec!["status".to_string()], vec!["created_at".to_string()]));
    let p = predicate_columns("(shop.o.total between 10 and 20) and (shop.o.customer_id <=> 11272)");
    assert_eq!((p.equality, p.range), (vec!["customer_id".to_string()], vec!["total".to_string()]));
    // Quoted text is never taken apart, even when a literal on the left contains an operator.
    let p = predicate_columns("(note = 'a OR b') AND (kind = 'x <> y') AND ('a > b' = tag)");
    assert_eq!(p.equality, vec!["note", "kind", "tag"]);
}

// A staging SQL Server reading: a Key Lookup's residual on a column qualified by a database whose name has hyphens
// read as a call of values (`app` minus …), so the lookups that found nothing lost their reading and their fix.
#[test]
fn a_column_qualified_by_a_hyphenated_database_is_a_column() {
    let p = predicate_columns(
        "[app-db-01].[app].[call_sessions].[conversation_id]=CONVERT_IMPLICIT(uniqueidentifier,[@par2],0)",
    );
    assert_eq!(p.equality, vec!["conversation_id"]);
    // A call of values stays a value, with or without its parentheses.
    assert!(is_value("dateadd(day,@P1,'2026-01-01')") && is_value("dateadd day,@P1,'2026-01-01'"));
}

// What a statement does is a fact code can match, in any language; the words are said from it.
#[test]
fn a_handle_is_decided_before_it_is_worded() {
    assert_eq!(statement_handle_of("SELECT id FROM orders WHERE id = ?"), Handle::Read(Some("orders".into())));
    assert_eq!(statement_handle_of("SELECT count(*) FROM (SELECT 1) s"), Handle::Read(None));
    assert_eq!(statement_handle_of("VACUUM"), Handle::Query);
    assert_eq!(statement_handle_of("CALL shop.refresh(?)"), Handle::Call("shop.refresh".into()));
    let mut q = sample();
    q.text = "WITH b AS (SELECT id FROM jobs) UPDATE jobs SET a = 1 FROM b".into();
    assert_eq!(query_handle_of(&q), Handle::UpdateInWith);
    assert_eq!(query_handle(&q), Handle::UpdateInWith.words());
}

/// The postgres demo with 36 hours of Culprit's scans over 40 days of totals, in which its first statement ran 30
/// times and took 9 s of the database's 30 s.
fn over_scans() -> (Snapshot, QueryStat) {
    let path = format!("{}/../../fixtures/postgres-demo.json", env!("CARGO_MANIFEST_DIR"));
    let mut s: Snapshot = serde_json::from_str(&std::fs::read_to_string(path).expect("fixture")).expect("snapshot");
    let w = s.workload.as_mut().expect("workload");
    (w.window_start, w.window_hours) = (Some("2026-09-01T11:54:21Z".into()), 960.0);
    w.timeline = Some(
        serde_json::from_value(serde_json::json!({
            "source": "scans",
            "starts": ["2026-10-10T00:00:00Z", "2026-10-10T12:00:00Z", "2026-10-11T00:00:00Z"],
            "end": "2026-10-11T12:00:00Z",
            "time_ms": [10000.0, 10000.0, 10000.0],
            "executions": [5000.0, 5000.0, 5000.0]
        }))
        .expect("timeline"),
    );
    let q = &mut w.queries[0];
    q.trend = Some(culprit_core::QueryTrend { time_ms: vec![Some(3_000.0); 3], executions: vec![Some(10.0); 3] });
    (q.executions, q.mean_time_ms, q.total_time_ms) = (1_000.0, 60_000.0, 60_000_000.0);
    w.total_time_ms = 200_000_000.0;
    let q = s.workload.as_ref().unwrap().queries[0].clone();
    (s, q)
}

// Every phrase the shared helpers say, word for word, as English has always read it.
#[test]
fn support_phrases_read_as_english_writes_them() {
    let mut said: Vec<(String, String)> = Vec::new();
    let mut pin = |label: &str, text: String| said.push((label.to_string(), text));
    // How often, in each window.
    let s = crate::waits::snapshot_with(Engine::Mssql);
    let w = s.workload.as_ref().unwrap();
    pin("window", w.window_phrase());
    pin("runs 1", runs_phrase(&s, 1.0));
    pin("runs 0.4", runs_phrase(&s, 0.4));
    pin("runs 48000", runs_phrase(&s, 48_000.0));
    let q = &s.queries()[0];
    pin("statement_runs", format!("{:?}", statement_runs(&s, q)));
    pin("runs_of", runs_of(&s, q));
    pin("runs_of_all", runs_of_all(&s, &[q, &s.queries()[1]]));
    pin("cost", query_cost_context(&s, &[q]).unwrap_or_default());
    pin("share", share_label(0.31));
    let mut short = s.clone();
    short.workload.as_mut().unwrap().window_hours = 5.0;
    pin("runs short", runs_phrase(&short, 3.0));
    short.workload.as_mut().unwrap().window_hours = 0.0;
    pin("runs unknown", runs_phrase(&short, 3.0));
    let mut burst = q.clone();
    (burst.executions, burst.first_seen, burst.last_seen) =
        (750.0, Some("2026-10-11T15:51:56Z".into()), Some("2026-10-11T15:55:15Z".into()));
    pin("statement_runs burst", format!("{:?}", statement_runs(&s, &burst)));
    pin("runs_of burst", runs_of(&s, &burst));
    // Over Culprit's scans.
    let (s, q) = over_scans();
    pin("scans_sentence", scans_sentence(&s, &q, "of all database time").unwrap_or_default());
    pin("runs_of scans", runs_of(&s, &q));
    pin("cost scans", query_cost_context(&s, &[&q]).unwrap_or_default());
    // Finding who holds a lock, on each engine.
    for engine in [Engine::Mssql, Engine::Postgres, Engine::Mysql, Engine::Yugabyte] {
        let mut s = crate::waits::snapshot_with(engine);
        let mut q = s.queries()[0].clone();
        q.executions = 50.0;
        pin(&format!("{engine:?} live"), find_lock_holder(&s, &q, Some(1_000.0)).lead);
        pin(&format!("{engine:?} rare"), find_lock_holder(&s, &q, Some(1.0)).lead);
        pin(&format!("{engine:?} rare runs"), find_lock_holder(&s, &q, None).lead);
        let slow = log_next_waits(&s, Some(2_500.0));
        pin(&format!("{engine:?} slow"), format!("{} | {:?}", slow.lead, slow.sql));
        q.executions = 1.0;
        pin(&format!("{engine:?} once"), find_lock_holder(&s, &q, None).lead);
        s.server.set_platform(Some(culprit_core::Platform::AmazonRds));
        pin(&format!("{engine:?} rds"), setting_change(&s.server, "max_connections", "200").how);
    }
    let mut azure = crate::waits::snapshot_with(Engine::Mssql);
    azure.server.set_platform(Some(culprit_core::Platform::AzureSqlDatabase));
    pin("azure", log_next_waits(&azure, None).lead);
    let mut sqlite = crate::waits::snapshot_with(Engine::Mysql);
    sqlite.server.engine = Engine::Sqlite;
    pin("sqlite", log_next_waits(&sqlite, None).lead);
    let mut pg = crate::waits::snapshot_with(Engine::Postgres);
    pg.settings.retain(|s| s.name != "deadlock_timeout");
    pg.settings.retain(|s| s.name != "log_lock_waits");
    let setting = |name: &str, value: &str| culprit_core::Setting {
        name: name.into(),
        value: value.into(),
        default_value: None,
        scope: "server".into(),
    };
    pg.settings.push(setting("log_lock_waits", "on"));
    pin("pg on", log_next_waits(&pg, None).lead);
    pin("pg on slow", log_next_waits(&pg, Some(800.0)).lead);
    pg.settings.push(setting("deadlock_timeout", "500"));
    pin("pg deadlock 500", log_next_waits(&pg, None).lead);
    pg.settings.retain(|s| s.name != "deadlock_timeout");
    pg.settings.push(setting("deadlock_timeout", "2s"));
    pin("pg deadlock 2s", log_next_waits(&pg, None).lead);
    // A day or more of window: a rate a day, and rare waits on MySQL.
    let mut day = crate::waits::snapshot_with(Engine::Mysql);
    day.workload.as_mut().unwrap().window_hours = 48.0;
    pin("runs a day", runs_phrase(&day, 48_000.0));
    pin("runs few", runs_phrase(&day, 1.0));
    let mut q = day.queries()[0].clone();
    q.executions = 50.0;
    pin("mysql rare", find_lock_holder(&day, &q, Some(1.0)).lead);
    pin("mysql rare runs", find_lock_holder(&day, &q, None).lead);
    pin("mysql statement_runs", format!("{:?}", statement_runs(&day, &q)));
    pin("mysql runs_of", runs_of(&day, &q));
    let mut slow = day.clone();
    slow.settings.retain(|s| !matches!(s.name.as_str(), "slow_query_log" | "long_query_time"));
    slow.settings.push(setting("slow_query_log", "ON"));
    slow.settings.push(setting("long_query_time", "0.5"));
    pin("mysql logging", log_next_waits(&slow, Some(2_000.0)).lead);
    // A statement that stopped: nothing to catch now.
    let mut s = crate::waits::snapshot_with(Engine::Postgres);
    let w = s.workload.as_mut().unwrap();
    (w.windowed, w.window_hours, w.window_start, w.window_end) =
        (true, 144.0, Some("2026-10-05T12:00:00Z".into()), Some("2026-10-11T12:00:00Z".into()));
    let mut q = s.queries()[0].clone();
    (q.executions, q.first_seen, q.last_seen) =
        (50.0, Some("2026-10-05T12:00:00Z".into()), Some("2026-10-06T12:00:00Z".into()));
    pin("stopped rare", find_lock_holder(&s, &q, Some(1.0)).lead);
    pin("stopped", find_lock_holder(&s, &q, Some(10_000.0)).lead);
    pin("stopped runs_of", runs_of(&s, &q));
    pin("stopped statement_runs", format!("{:?}", statement_runs(&s, &q)));
    let mut maria = crate::waits::snapshot_with(Engine::Mysql);
    maria.server.product = "MariaDB 11.8.9".into();
    pin("mariadb", log_next_waits(&maria, Some(500.0)).lead);
    // Since when counters count.
    let mut yb = crate::waits::snapshot_with(Engine::Yugabyte);
    yb.pressure = None;
    pin("since yugabyte", since_phrase(&yb));
    let mut userstat = crate::waits::snapshot_with(Engine::Mysql);
    userstat.server.capabilities.retain(|c| *c != Capability::PerformanceSchema);
    userstat.server.capabilities.push(Capability::IndexUsage);
    pin("since userstat", since_phrase(&userstat));
    let mut reset = crate::waits::snapshot_with(Engine::Mysql);
    let w = reset.workload.as_mut().unwrap();
    w.kind = Some(culprit_core::WorkloadKind::PerformanceSchema);
    reset.server.started_at = Some("2026-01-01T00:00:00Z".into());
    pin("since reset", format!("{} | {}", counters_may_be_reset(&reset), since_phrase(&reset)));
    // Statements named by what they do.
    for text in [
        "INSERT INTO orders (id) VALUES (1)",
        "MERGE INTO stock USING x ON 1 = 1 WHEN MATCHED THEN DELETE",
        "DELETE FROM jobs WHERE id = 1",
        "VACUUM",
        "WITH x AS (SELECT 1) UPDATE t SET a = 1 FROM x",
    ] {
        pin(text, statement_handle(text));
    }
    let mut q = sample();
    q.object_name = Some("dbo.usp_refresh".into());
    pin("object", query_handle(&q));
    let mut q = sample();
    q.text = "WITH b AS (SELECT id FROM jobs) DELETE FROM jobs USING b".into();
    q.resolved_write_target = Some("public.jobs".into());
    pin("resolved delete", query_handle(&q));
    q.resolved_write_target = None;
    q.text = "WITH b AS (SELECT id FROM jobs) UPDATE jobs SET a = 1 FROM b".into();
    pin("with update", query_handle(&q));
    let live = "Find what holds the locks while it waits (Live shows the blocking chain).";
    let reported = "Have the next ones reported instead: SQL Server's blocked process report names the session holding \
                    the lock, with its statement, for each wait longer than the server's blocked process threshold (s) \
                    (0, off, by default; sp_configure sets it). Capture sqlserver.blocked_process_report in an event \
                    session to keep them.";
    let rds = "Set max_connections to 200 in the instance's DB parameter group.";
    let yugabyte = "Correlate YSQL statement logs and DocDB wait-queue evidence by transaction, node and time. Configure \
                    supported logging through YugabyteDB administration.";
    let expected: Vec<(&str, String)> = vec![
        ("window", "the last 5 hours".into()),
        ("runs 1", "once in 5 hours".into()),
        ("runs 0.4", "0 times in 5 hours".into()),
        ("runs 48000", "48K times in 5 hours".into()),
        ("statement_runs", r#"("1.2K times in 5 hours", "")"#.into()),
        ("runs_of", "1.2K times in 5 hours".into()),
        ("runs_of_all", "1.8K times in 5 hours".into()),
        ("cost", "22.2 s of database time over the last 5 hours (54%)".into()),
        ("share", "31% of DB time".into()),
        ("runs short", "3 times in 5 hours".into()),
        ("runs unknown", "3 times over the recorded workload window".into()),
        ("statement_runs burst", r#"("750 times in 5 hours", "")"#.into()),
        ("runs_of burst", "750 times in 5 hours".into()),
        (
            "scans_sentence",
            "It ran about 20 times a day at 300 ms on average over 36 hours of Culprit's own scans: 30% of all \
             database time in them (1 min on average over the 40 days since statistics were reset)."
                .into(),
        ),
        ("runs_of scans", "about 20 times a day over 36 hours of Culprit's scans".into()),
        ("cost scans", "9.0 s of database time over 36 hours of Culprit's scans (30%)".into()),
        ("Mssql live", live.into()),
        (
            "Mssql rare",
            format!(
                "Its waits can come from as few as 1 run in the last 5 hours, too few to count on catching one while \
                 it happens. {reported}"
            ),
        ),
        ("Mssql rare runs", live.into()),
        ("Mssql slow", format!("{reported} | None")),
        (
            "Mssql once",
            "It ran once in the last 5 hours, so there is no wait to catch now; if it runs again, have its lock waits \
             logged (SQL Server's blocked process report names the holder) to see what holds them."
                .into(),
        ),
        ("Mssql rds", rds.into()),
        ("Postgres live", live.into()),
        (
            "Postgres rare",
            "Its waits can come from as few as 1 run in the 5 hours since statistics were reset, too few to count on \
             catching one while it happens. Have the next ones logged instead. Set log_lock_waits to on and reload \
             the configuration (settings marked as needing a restart take effect after one). log_lock_waits then \
             logs each lock wait longer than deadlock_timeout (1 s by default) with the process holding the lock."
                .into(),
        ),
        ("Postgres rare runs", live.into()),
        (
            "Postgres slow",
            "Have the next ones logged instead. Set log_lock_waits to on and log_min_duration_statement to 2.5s and \
             reload the configuration (settings marked as needing a restart take effect after one). log_lock_waits \
             then logs each lock wait longer than deadlock_timeout (1 s by default) with the process holding the \
             lock, and log_min_duration_statement each statement slower than 2.5s (any statement, not only this one) \
             with its duration. | Some(\"ALTER SYSTEM SET log_lock_waits = 'on';\\nSELECT pg_reload_conf();\\nALTER \
             SYSTEM SET log_min_duration_statement = '2.5s';\\nSELECT pg_reload_conf();\")"
                .into(),
        ),
        (
            "Postgres once",
            "It ran once in the 5 hours since statistics were reset, so there is no wait to catch now; if it runs \
             again, have its lock waits logged (log_lock_waits) to see what holds them."
                .into(),
        ),
        ("Postgres rds", rds.into()),
        ("Mysql live", live.into()),
        ("Mysql rare", live.into()),
        ("Mysql rare runs", live.into()),
        (
            "Mysql slow",
            "Have the next ones logged instead. Set slow_query_log to ON and long_query_time to 2.5; SET PERSIST \
             keeps it across restarts. The slow query log then keeps each statement slower than 2.5 s with its lock \
             time and when it ran; MySQL keeps no record of who held the lock, so match those times with what the \
             application ran then. | Some(\"SET PERSIST slow_query_log = 'ON';\\nSET PERSIST long_query_time = \
             2.5;\")"
                .into(),
        ),
        (
            "Mysql once",
            "It ran once in the 2 minutes since statistics were reset, so there is no wait to catch now; if it runs \
             again, have its lock waits logged (the slow query log, with its lock time) to see what holds them."
                .into(),
        ),
        ("Mysql rds", rds.into()),
        ("Yugabyte live", live.into()),
        ("Yugabyte rare", live.into()),
        ("Yugabyte rare runs", live.into()),
        ("Yugabyte slow", format!("{yugabyte} | None")),
        (
            "Yugabyte once",
            "It ran once in the 1 minute since statistics were reset, so there is no wait to catch now; if it runs \
             again, have its lock waits logged (YSQL statement logs and DocDB wait-queue evidence) to see what holds \
             them."
                .into(),
        ),
        ("Yugabyte rds", rds.into()),
        (
            "azure",
            "Query Store keeps when it waited, interval by interval (sys.query_store_wait_stats), not who held the \
             lock: match the intervals of its waits with what the application ran then."
                .into(),
        ),
        (
            "sqlite",
            "SQLite keeps no record of what held the file's write lock: the application's own logs of its writes at \
             those times say which one it was."
                .into(),
        ),
        (
            "pg on",
            "Log_lock_waits is on already, and logs each lock wait longer than deadlock_timeout (1 s by default) with \
             the process holding the lock: the server log has the next ones, with the process holding the lock."
                .into(),
        ),
        (
            "pg on slow",
            "Have the next ones logged instead. Set log_min_duration_statement to 800ms and reload the configuration \
             (settings marked as needing a restart take effect after one). log_lock_waits is on already, and logs \
             each lock wait longer than deadlock_timeout (1 s by default) with the process holding the lock, and \
             log_min_duration_statement each statement slower than 800ms (any statement, not only this one) with its \
             duration."
                .into(),
        ),
        (
            "pg deadlock 500",
            "Log_lock_waits is on already, and logs each lock wait longer than deadlock_timeout (500 ms) with the \
             process holding the lock: the server log has the next ones, with the process holding the lock."
                .into(),
        ),
        (
            "pg deadlock 2s",
            "Log_lock_waits is on already, and logs each lock wait longer than deadlock_timeout (2s) with the process \
             holding the lock: the server log has the next ones, with the process holding the lock."
                .into(),
        ),
        ("runs a day", "about 24K times a day".into()),
        ("runs few", "once in the 2 days since statistics were reset".into()),
        (
            "mysql rare",
            "Its waits can come from as few as 1 run in the 2 days since statistics were reset, too few to count on \
             catching one while it happens. Have the next ones logged instead. Set slow_query_log to ON and \
             long_query_time to 1; SET PERSIST keeps it across restarts. The slow query log then keeps each statement \
             slower than 1 s with its lock time and when it ran; MySQL keeps no record of who held the lock, so match \
             those times with what the application ran then."
                .into(),
        ),
        ("mysql rare runs", live.into()),
        ("mysql statement_runs", r#"("50 times", ", on Oct 11 at 12:07 UTC")"#.into()),
        ("mysql runs_of", "50 times on Oct 11 at 12:07 UTC".into()),
        (
            "mysql logging",
            "Look for the next ones in the slow query log. The slow query log already keeps each statement slower \
             than 0.5 s with its lock time and when it ran; MySQL keeps no record of who held the lock, so match \
             those times with what the application ran then."
                .into(),
        ),
        (
            "stopped rare",
            "It no longer runs, so there is no wait to catch now; if it comes back, have its lock waits logged \
             (log_lock_waits): its runs are too few to catch one while it waits."
                .into(),
        ),
        (
            "stopped",
            "It no longer runs, so there is no wait to catch now; if it comes back, find what holds the locks while \
             it waits (Live shows the blocking chain)."
                .into(),
        ),
        ("stopped runs_of", "about 8 times a day".into()),
        ("stopped statement_runs", r#"("about 8 times a day", "")"#.into()),
        (
            "mariadb",
            "Have the next ones logged instead. Set slow_query_log to ON and long_query_time to 0.5 now, and in the \
             server's option file (my.cnf) so it survives a restart. The slow query log then keeps each statement \
             slower than 0.5 s with its lock time and when it ran; MariaDB keeps no record of who held the lock, so \
             match those times with what the application ran then. Written to a table (TABLE in log_output), MariaDB \
             counts the whole run of each statement it logs as that statement's lock time, so their lock time says \
             nothing (Culprit reads it as unknown); written to a file (FILE), it is the real wait."
                .into(),
        ),
        (
            "since yugabyte",
            "across cluster nodes since each node's database counters were reset; no common counting period is known"
                .into(),
        ),
        ("since userstat", "since userstat was turned on or its statistics last flushed".into()),
        ("since reset", "true | since the server started or its performance_schema counters were last reset".into()),
        ("INSERT INTO orders (id) VALUES (1)", "an insert into orders".into()),
        ("MERGE INTO stock USING x ON 1 = 1 WHEN MATCHED THEN DELETE", "a merge into stock".into()),
        ("DELETE FROM jobs WHERE id = 1", "a delete from jobs".into()),
        ("VACUUM", "a query".into()),
        ("WITH x AS (SELECT 1) UPDATE t SET a = 1 FROM x", "an update of t".into()),
        ("object", "a statement in dbo.usp_refresh".into()),
        ("resolved delete", "a delete from jobs".into()),
        ("with update", "an update in a WITH statement".into()),
    ];
    let differ: Vec<String> = said
        .iter()
        .zip(&expected)
        .filter(|((label, text), (want_label, want))| label != want_label || text != want)
        .map(|((label, text), (_, want))| format!("{label}:\n  said: {text}\n  want: {want}"))
        .collect();
    assert!(differ.is_empty() && said.len() == expected.len(), "{}", differ.join("\n"));
}
#[test]
fn sentence_initial_handles_preserve_native_session_text() {
    let native = "set application_name = 'MixedCaseApp'";
    for language in ["en", "de", "fr", "es"] {
        culprit_core::i18n::scope(language, || {
            let handle = super::Handle::Session(native.into());
            assert_eq!(handle.leading_words(), native);
        });
    }
}
