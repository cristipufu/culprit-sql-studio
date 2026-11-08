//! What last-actual-plans says, word for word: its messages come from locales/en.json.

use crate::pins::{fixture, info, pin, pin_text, run, set};

#[test]
fn last_actual_plans_says_what_each_setting_keeps() {
    pin_text(
        "last_actual_plans_info",
        &info("last-actual-plans"),
        r#"title: Actual plans without re-running queries
description: SQL Server 2019+ can keep each query's last actual plan (LAST_QUERY_PLAN_STATS) and report live per-operator progress (LIGHTWEIGHT_QUERY_PROFILING)."#,
    );
    // A server you manage: the trace flag is one more way to keep actual plans.
    let s = fixture("mssql-demo.json");
    pin(
        "last_actual_plans_managed_here",
        &run("last-actual-plans", &s),
        r#"id: last-actual-plans:last-query-plan-stats · Low · High · score 4.8 · share None
title: LAST_QUERY_PLAN_STATS is off for this database
summary: With LAST_QUERY_PLAN_STATS on, SQL Server can keep the real row counts and timings of an eligible cached query's most recent run. Complex or resource-intensive queries can then show where they actually spent their time, operator by operator, without running them again. Simple transactional queries may retain only the root operator; plans that aren't cached or have been evicted retain no last-run data. Without retained runtime data, the saved estimated plan cannot confirm a row misestimate. (Trace flag 2451 turns the same on for the whole server; if it is set, actual plans are kept already.)
impact: Diagnostics
fix: Turn it on for this database.
sql: ALTER DATABASE SCOPED CONFIGURATION SET LAST_QUERY_PLAN_STATS = ON;
cost: Uses SQL Server's lightweight profiling to keep a little runtime data per cached plan: a small CPU overhead on very busy servers.
verify: Let the workload run, re-scan, then open a complex or resource-intensive query in Queries and pick "Last run" when available: the diagram shows the rows and timings SQL Server retained. A simple query may have only a root operator, and an uncached or evicted plan has no last-run data."#,
    );
    // Azure SQL Database has no trace flags; live progress turned off too.
    let mut s = fixture("mssql-demo.json");
    s.server.set_platform(Some(culprit_core::Platform::AzureSqlDatabase));
    set(&mut s, "LIGHTWEIGHT_QUERY_PROFILING", "OFF");
    pin(
        "last_actual_plans_azure",
        &run("last-actual-plans", &s),
        r#"id: last-actual-plans:last-query-plan-stats · Low · High · score 4.8 · share None
title: LAST_QUERY_PLAN_STATS is off for this database
summary: With LAST_QUERY_PLAN_STATS on, SQL Server can keep the real row counts and timings of an eligible cached query's most recent run. Complex or resource-intensive queries can then show where they actually spent their time, operator by operator, without running them again. Simple transactional queries may retain only the root operator; plans that aren't cached or have been evicted retain no last-run data. Without retained runtime data, the saved estimated plan cannot confirm a row misestimate.
impact: Diagnostics
fix: Turn it on for this database.
sql: ALTER DATABASE SCOPED CONFIGURATION SET LAST_QUERY_PLAN_STATS = ON;
cost: Uses SQL Server's lightweight profiling to keep a little runtime data per cached plan: a small CPU overhead on very busy servers.
verify: Let the workload run, re-scan, then open a complex or resource-intensive query in Queries and pick "Last run" when available: the diagram shows the rows and timings SQL Server retained. A simple query may have only a root operator, and an uncached or evicted plan has no last-run data.

id: last-actual-plans:lightweight-query-profiling · Low · High · score 4.8 · share None
title: Live progress of running queries is turned off
summary: LIGHTWEIGHT_QUERY_PROFILING is off for this database, so SQL Server doesn't count rows per operator while a query runs. Live can show how long a statement has been running, but not how far along each step is or which step is slow right now.
impact: Diagnostics
fix: Turn it back on (it is on by default since SQL Server 2019).
sql: ALTER DATABASE SCOPED CONFIGURATION SET LIGHTWEIGHT_QUERY_PROFILING = ON;
cost: SQL Server's default; designed to run all the time.
verify: Open Live while a long query runs: its row expands into per-operator progress."#,
    );
}
