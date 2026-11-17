//! The `connection-churn` check: New connections per second since startup (SQL Server logins, PostgreSQL 14+ sessions, MySQL and MariaDB Connections), with how long sessions last and how many came from a pool where the engine says.

use super::*;
use crate::evidence::Requirement as Evidence;

pub fn connection_churn_info() -> CheckInfo {
    CheckInfo {
        id: "connection-churn".into(),
        title: t!("check.connection_churn.info.title"),
        category: Category::Configuration,
        engines: vec![Engine::Mssql, Engine::Postgres, Engine::Mysql],
        requires: vec![],
        description: t!("check.connection_churn.info.description"),
    }
}

check!(
    ConnectionChurnCheck: connection_churn_info,
    decide = new_connections,
    render = connection_churn_finding,
    case<'s> = ConnectionChurn,
    evidence = &[Evidence::Pressure { signal: None, timed_files: false, complete: false }],
);

/// A thread cache this large is enough: the fix raises a smaller one to it, never lowers one.
const THREAD_CACHE: f64 = 64.0;

/// New connections a second since the server started, above the engine's bar, and what the engine says about them.
pub struct ConnectionChurn {
    rate: f64,
    uptime_hours: f64,
    native_period: Option<culprit_core::ConnectionOutcomeRate>,
    /// PostgreSQL: how long its sessions last, in seconds.
    session_length: Option<f64>,
    /// SQL Server: resets against resets plus new logins, as a percentage.
    pooled_reuse: Option<f64>,
    /// MySQL: connections that found the thread cache empty, as a percentage.
    thread_cache_misses: Option<f64>,
    /// MySQL: the thread cache it keeps now, and whether the fix raises it to [`THREAD_CACHE`].
    thread_cache: Option<f64>,
    raise_thread_cache: bool,
    /// A count of the whole server's connections: said, but not graded on this database.
    shared: bool,
    severity: Severity,
}

fn new_connections(snapshot: &Snapshot) -> Vec<ConnectionChurn> {
    let Some(p) = snapshot.pressure.as_ref() else { return Vec::new() };
    let (rate, uptime_hours, native_period) = if let Some(read) = &p.connection_outcomes {
        if !read.target.snapshot_namespace_comparison(snapshot).allows_comparison() {
            return Vec::new();
        }
        let Some(rate) = read.rate(culprit_core::ConnectionOutcomeMetricKind::SessionsEstablished) else {
            return Vec::new();
        };
        (rate.per_second, rate.seconds / 3600.0, Some(rate))
    } else {
        let Some(rate) = p.signal(culprit_core::signal_id::NEW_CONNECTIONS).map(|s| s.value) else { return Vec::new() };
        let Some(hours) = p.uptime_hours.filter(|hours| *hours >= 1.0) else { return Vec::new() };
        (rate, hours, None)
    };
    let engine = snapshot.server.engine;
    // On Azure SQL Database the login counters belong to the server process behind the database, shared by its
    // elastic pool: another database's logins aren't this one's to fix.
    if engine == Engine::Mssql && snapshot.server.on(culprit_core::Platform::AzureSqlDatabase) {
        return Vec::new();
    }
    let limit = if engine == Engine::Postgres { 2.0 } else { 10.0 };
    if rate < limit {
        return Vec::new();
    }
    let session_length =
        if native_period.is_none() { p.signal(culprit_core::signal_id::SESSION_LENGTH).map(|s| s.value) } else { None };
    let pooled_reuse = p.signal(culprit_core::signal_id::POOLED_REUSE).map(|s| s.value);
    // A large pool can legitimately open many long-lived sessions. Neither a rate nor a thread-cache miss ratio
    // establishes one connection per request. Known long-lived sessions / frequent pooled resets contradict it.
    if (engine == Engine::Postgres && session_length.is_some_and(|s| s >= 60.0))
        || (engine == Engine::Mssql && pooled_reuse.is_some_and(|s| s >= 90.0))
    {
        return Vec::new();
    }
    // A count of the whole server's connections is shared: said, with what it counts, but not graded on this
    // database (CLAUDE.md: mark shared counters and don't grade them). PostgreSQL's is this database's own.
    let shared = engine != Engine::Postgres;
    let severity = if !shared && rate >= limit * 5.0 && session_length.is_some_and(|s| s <= 1.0) {
        Severity::Medium
    } else {
        Severity::Low
    };
    // A larger thread cache, where the server is configured, and never smaller than it is.
    let thread_cache = snapshot.setting("thread_cache_size").and_then(|s| s.value.trim().parse::<f64>().ok());
    vec![ConnectionChurn {
        rate,
        uptime_hours,
        native_period,
        session_length,
        pooled_reuse,
        thread_cache_misses: p.signal(culprit_core::signal_id::THREAD_CACHE_MISSES).map(|s| s.value),
        thread_cache,
        raise_thread_cache: engine == Engine::Mysql && thread_cache.is_none_or(|n| n < THREAD_CACHE),
        shared,
        severity,
    }]
}

fn connection_churn_finding(snapshot: &Snapshot, churn: ConnectionChurn) -> Finding {
    let engine = snapshot.server.engine;
    // Keyed on `server`, never the connection's name: finding ids travel into exports and agent briefs.
    let mut f = finding(
        "connection-churn",
        "server",
        Category::Configuration,
        subject(SubjectKind::Database, &snapshot.server.database),
    );
    let rate = format::count(churn.rate);
    // PostgreSQL counts sessions to this database; SQL Server's logins and MySQL's connections count the whole server,
    // every database and application on it.
    f.title = if engine == Engine::Postgres {
        t!("check.connection_churn.title.database", n = churn.rate, rate = &rate)
    } else {
        t!("check.connection_churn.title.server", n = churn.rate, rate = &rate)
    };
    let mut summary = vec![if let Some(period) = &churn.native_period {
        t!("view.connection_outcomes.churn_period", rate = &rate, reset = &period.reset_at, at = &period.observed_at)
    } else {
        t!(
            "check.connection_churn.summary.accepted",
            engine = engine_name(snapshot),
            ago = format::duration(churn.uptime_hours * 3_600_000.0),
            rate = &rate,
            scope = culprit_core::wire_name(&engine)
        )
    }];
    match engine {
        Engine::Yugabyte => summary.push(t!("check.connection_churn.detail.yugabyte")),
        Engine::Sqlite => summary.push(t!("check.connection_churn.detail.sqlite")),
        Engine::Postgres => {
            summary.push(t!("check.connection_churn.detail.postgres"));
            if let Some(s) = churn.session_length {
                summary.push(t!("check.connection_churn.detail.session_length", length = format::duration(s * 1000.0)));
            }
        }
        Engine::Mssql => summary.push(match churn.pooled_reuse {
            Some(pct) => t!("check.connection_churn.detail.pooled_reuse", percent = format!("{pct:.0}")),
            None => t!("check.connection_churn.detail.mssql"),
        }),
        Engine::Mysql => summary.push(match churn.thread_cache_misses {
            Some(pct) => t!("check.connection_churn.detail.thread_cache_misses", percent = format!("{pct:.0}")),
            None => t!("check.connection_churn.detail.mysql"),
        }),
    }
    summary.push(t!("check.connection_churn.summary.not_proof"));
    f.summary = summary.join(" ");
    f.severity = churn.severity;
    if churn.shared {
        f.caveats.push(t!("check.connection_churn.caveat.shared", engine = snapshot.server.name()));
    }
    severity_score(&mut f);
    f.impact.set_label(t!("check.connection_churn.impact", rate = &rate));
    f.confidence = Confidence::Medium;
    f.metrics = vec![Metric::said(culprit_core::key!("check.connection_churn.metric.rate"), churn.rate, Unit::Count)];
    let cache_change = churn
        .raise_thread_cache
        .then(|| setting_change(&snapshot.server, "thread_cache_size", &format!("{THREAD_CACHE:.0}")));
    f.fix = Some(match engine {
        Engine::Yugabyte => Fix { summary: t!("check.connection_churn.fix.yugabyte"), sql: None, cost: None },
        Engine::Sqlite => Fix { summary: t!("check.connection_churn.fix.sqlite"), sql: None, cost: None },
        Engine::Postgres => Fix { summary: t!("check.connection_churn.fix.postgres"), sql: None, cost: None },
        Engine::Mssql => Fix { summary: t!("check.connection_churn.fix.mssql"), sql: None, cost: None },
        Engine::Mysql => Fix {
            summary: match (&cache_change, churn.thread_cache) {
                (Some(change), _) => t!("check.connection_churn.fix.mysql_thread_cache", how = &change.how),
                (None, Some(n)) => t!("check.connection_churn.fix.mysql_cache_kept", threads = format::count(n)),
                (None, None) => t!("check.connection_churn.fix.mysql"),
            },
            sql: cache_change.and_then(|c| c.sql),
            cost: None,
        },
    });
    f.verify = vec![t!("check.connection_churn.verify")];
    f.caveats.push(if churn.native_period.is_some() {
        t!("view.connection_outcomes.limit.current")
    } else {
        t!("check.connection_churn.caveat.since_startup")
    });
    f
}

#[cfg(test)]
mod native_period_tests {
    use super::*;
    use culprit_core::{Pressure, Signal, SignalScope, SignalUnit};

    fn snapshot() -> Snapshot {
        let read = crate::connection_outcomes::tests::snapshot(180000);
        let mut snapshot = crate::waits::snapshot_with(Engine::Postgres);
        snapshot.server.database = "fixture".into();
        snapshot.bind_identity(read.target.database().unwrap().clone());
        snapshot.pressure = Some(Pressure {
            uptime_hours: Some(10000.0),
            signals: vec![Signal::new(
                culprit_core::signal_id::NEW_CONNECTIONS,
                "controlled-legacy-rate",
                999.0,
                SignalUnit::PerSecond,
                SignalScope::SinceStartup,
            )],
            connection_outcomes: Some(Box::new(read)),
            ..Default::default()
        });
        snapshot
    }

    #[test]
    fn a_native_reset_period_controls_churn_without_a_server_age_or_completed_session_latency() {
        let mut snapshot = snapshot();
        snapshot.pressure.as_mut().unwrap().signals.push(Signal::new(
            culprit_core::signal_id::SESSION_LENGTH,
            "controlled-other-mean",
            60000.0,
            SignalUnit::Seconds,
            SignalScope::SinceStartup,
        ));
        let cases = new_connections(&snapshot);
        let [case] = cases.as_slice() else { panic!("one own-period session-rate observation") };
        assert_eq!(case.rate, 9007199254740993u64 as f64 / 86400.0);
        assert_eq!(case.uptime_hours, 24.0);
        assert!(case.session_length.is_none(), "recorded state time is not completed-session latency");
        assert_eq!(case.severity, Severity::Low);
        let finding = connection_churn_finding(&snapshot, new_connections(&snapshot).remove(0));
        assert!(finding.summary.contains("2020-01-01T00:00:00Z"));
        assert!(!finding.caveats.contains(&t!("check.connection_churn.caveat.since_startup")));
    }

    #[test]
    fn unknown_or_invalid_native_evidence_never_borrows_legacy_rate_age_or_another_database() {
        for change in ["reset", "clock", "header", "identity", "database", "engine", "incarnation"] {
            let mut snapshot = snapshot();
            let read = snapshot.pressure.as_mut().unwrap().connection_outcomes.as_mut().unwrap();
            match change {
                "reset" => {
                    for counter in &mut read.observations[0].counters.counters {
                        counter.reset = culprit_core::ResetMarker::Unknown;
                    }
                }
                "clock" => {
                    read.observations[0].counters.properties.remove("counter_observed_at");
                }
                "header" => read.sources[0].records = 0,
                "identity" => snapshot.identity = None,
                "database" => snapshot.server.database = "other".into(),
                "engine" => snapshot.server.engine = Engine::Mysql,
                "incarnation" => {
                    let culprit_core::telemetry::CollectionTarget::Database(database) = &mut read.target else {
                        unreachable!()
                    };
                    database.incarnation = Some("replacement-native-db".into());
                    snapshot.identity.as_mut().unwrap().incarnation = Some("original-native-db".into());
                }
                _ => unreachable!(),
            }
            assert!(new_connections(&snapshot).is_empty(), "{change}");
        }
    }
}
