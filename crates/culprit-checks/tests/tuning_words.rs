//! What the tuning checks say, word for word: query-family (statements sent with literals, single-use plans) and
//! tuning-advice (SQL Server's own recommendations, forced plans and hints). Their messages come from
//! locales/en.json.

use crate::pins::{fixture, info, pin, pin_text, run, set};
use culprit_core::{PlanCacheUse, Platform, QueryFamily, Snapshot};

fn family(sample: &str, variants: f64, ids: &[&str]) -> QueryFamily {
    QueryFamily {
        key: "0x8A1F".into(),
        sample: sample.into(),
        variants,
        executions: 9000.0,
        total_time_ms: 5000.0,
        cached_bytes: None,
        query_ids: ids.iter().map(|s| s.to_string()).collect(),
    }
}

/// A copy of statement `id` as `twin`, with its text or parameters changed by `change`.
fn twin(s: &mut Snapshot, id: &str, twin: &str, change: impl Fn(&mut culprit_core::QueryStat)) {
    let w = s.workload.as_mut().expect("a workload");
    let mut copy = w.queries.iter().find(|q| q.id == id).expect("the statement").clone();
    copy.id = twin.into();
    change(&mut copy);
    w.queries.push(copy);
}

#[test]
fn query_families_say_what_their_copies_differ_in() {
    pin_text(
        "query_family_info",
        &info("query-family"),
        r#"title: Statements with literals baked in
description: The same statement sent many times with different literal values (or value-list lengths), so the database compiles and tracks each copy separately; and plan caches filled with single-use plans."#,
    );

    // SQL Server: two members whose string parameter is sized from each value, holding plan cache.
    let mut s = fixture("mssql-demo.json");
    let q = s.workload.as_mut().unwrap().queries.iter_mut().find(|q| q.id == "7").unwrap();
    q.parameters = Some("@P1 nvarchar(7)".into());
    twin(&mut s, "7", "7001", |q| q.parameters = Some("@P1 nvarchar(12)".into()));
    let mut declared = family(
        "(@P1 nvarchar(7))SELECT [id], [name], [segment] FROM [shop].[customers] WHERE [email] = @P1",
        300.0,
        &["7", "7001"],
    );
    declared.cached_bytes = Some(3.0 * 1024.0 * 1024.0);
    // One text Culprit has, its parameters named by the application: the declarations are what differs.
    let one_declared =
        family("(@email nvarchar(9))SELECT [id] FROM [shop].[customers] WHERE [email] = @email", 1500.0, &[]);
    // One text with a literal: what the copies differ in is inferred.
    let inferred = family("SELECT [id] FROM [shop].[orders] WHERE [status] = 'paid'", 40.0, &[]);
    s.query_families = vec![declared, one_declared, inferred];
    pin(
        "query_family_mssql_declarations",
        &run("query-family", &s),
        r#"id: query-family:0x8A1F · Low · High · score 17.885557044199842 · share Some(0.1227096640020427)
title: The app sends 300 versions of a read from customers that differ only in how their parameters are declared
summary: 300 variants of the same statement ran 9.0K times for 12% of database time, because their parameters are declared differently from call to call (`@P1 nvarchar(7)` and `@P1 nvarchar(12)`): drivers do this when a string parameter's size comes from each value (ADO.NET's AddWithValue, for one). SQL Server compiles and keeps a plan for each declaration (3.0 MB of plan cache). Statistics split across the copies too, so none of them looks as heavy as the statement really is.
impact: 300 copies
metric: Variants: 300 Count
metric: Executions: 9000 Count
metric: Plan cache: 3145728 Bytes
query: 7 None -
query: 7001 None -
fix: In the code that sends this statement (one copy is quoted below, as recorded), declare each string parameter with a fixed size, the column's length, instead of letting the driver size it from the value (in ADO.NET, set SqlParameter.Size or use Add with a SqlDbType and size rather than AddWithValue; other drivers have a similar setting). Then one plan serves every value. Forced parameterization can't help: these statements are already parameterized.
verify: Re-scan after a deploy: this family shrinks to one or a few statements.
caveat: One of them, as recorded:
(@P1 nvarchar(7))SELECT [id], [name], [segment] FROM [shop].[customers] WHERE [email] = @P1

id: query-family:0x8A1F · Medium · Medium · score 33.9824670572079 · share Some(0.1227096640020427)
title: The app sends 1.5K versions of a read from customers that differ only in how their parameters are declared
summary: 1.5K variants of the same statement ran 9.0K times for 12% of database time, because their parameters are declared differently from call to call (this one declares `@email nvarchar(9)`): drivers do this when a string parameter's size comes from each value (ADO.NET's AddWithValue, for one). SQL Server compiles and keeps a plan for each declaration. Statistics split across the copies too, so none of them looks as heavy as the statement really is.
impact: 1.5K copies
metric: Variants: 1500 Count
metric: Executions: 9000 Count
fix: In the code that sends this statement (one copy is quoted below, as recorded), declare each string parameter with a fixed size, the column's length, instead of letting the driver size it from the value (in ADO.NET, set SqlParameter.Size or use Add with a SqlDbType and size rather than AddWithValue; other drivers have a similar setting). Then one plan serves every value. Forced parameterization can't help: these statements are already parameterized.
verify: Re-scan after a deploy: this family shrinks to one or a few statements.
caveat: Culprit sees one of these statements' text, so what the others differ in is inferred from it.
caveat: One of them, as recorded:
(@email nvarchar(9))SELECT [id] FROM [shop].[customers] WHERE [email] = @email

id: query-family:0x8A1F · Low · Medium · score 15.260373254024742 · share Some(0.1227096640020427)
title: The app sends 40 versions of a read from orders that differ only in literal values
summary: 40 variants of the same statement ran 9.0K times for 12% of database time, so SQL Server compiles each one separately and keeps a plan for each. Statistics split across the copies too, so none of them looks as heavy as the statement really is.
impact: 40 copies
metric: Variants: 40 Count
metric: Executions: 9000 Count
fix: In the code that sends this statement (one copy is quoted below, as recorded), send values as parameters (sp_executesql, or your driver's parameterized queries) so one plan serves every value. If you can't change the app, forced parameterization does it for the whole database.
sql: -- Prefer fixing the app. As a database-wide fallback:
ALTER DATABASE [culprit_demo] SET PARAMETERIZATION FORCED;
cost: Forced parameterization can make some queries pick a plan that suits one value badly (parameter sensitivity); watch the regression findings after switching.
verify: Re-scan after a deploy: this family shrinks to one or a few statements.
caveat: Culprit sees one of these statements' text, so what the others differ in is inferred from it.
caveat: One of them, as recorded:
SELECT [id] FROM [shop].[orders] WHERE [status] = 'paid'"#,
    );

    // SQL Server: two members whose texts differ in a literal.
    let mut s = fixture("mssql-demo.json");
    twin(&mut s, "7", "7002", |q| q.text = q.text.replace("@P1", "N'someone@example.com'"));
    s.query_families =
        vec![family("SELECT [id] FROM [shop].[customers] WHERE [email] = N'a@b.c'", 25.0, &["7", "7002"])];
    pin(
        "query_family_mssql_literals",
        &run("query-family", &s),
        r#"id: query-family:0x8A1F · Low · High · score 14.648013306056967 · share Some(0.1227096640020427)
title: The app sends 25 versions of a read from customers that differ only in literal values
summary: 25 variants of the same statement ran 9.0K times for 12% of database time, so SQL Server compiles each one separately and keeps a plan for each. Statistics split across the copies too, so none of them looks as heavy as the statement really is.
impact: 25 copies
metric: Variants: 25 Count
metric: Executions: 9000 Count
query: 7 None -
query: 7002 None -
fix: In the code that sends this statement (one copy is quoted below, as recorded), send values as parameters (sp_executesql, or your driver's parameterized queries) so one plan serves every value. If you can't change the app, forced parameterization does it for the whole database.
sql: -- Prefer fixing the app. As a database-wide fallback:
ALTER DATABASE [culprit_demo] SET PARAMETERIZATION FORCED;
cost: Forced parameterization can make some queries pick a plan that suits one value badly (parameter sensitivity); watch the regression findings after switching.
verify: Re-scan after a deploy: this family shrinks to one or a few statements.
caveat: One of them, as recorded:
SELECT [id] FROM [shop].[customers] WHERE [email] = N'a@b.c'"#,
    );

    // Neither family has a sample text: the fix says the change on its own.
    let q = s.workload.as_mut().unwrap().queries.iter_mut().find(|q| q.id == "7").unwrap();
    q.parameters = Some("@P1 nvarchar(7)".into());
    twin(&mut s, "7", "7001", |q| q.parameters = Some("@P1 nvarchar(12)".into()));
    s.query_families = vec![family("", 30.0, &["7", "7001"]), family("", 30.0, &["7", "7002"])];
    pin(
        "query_family_mssql_no_sample",
        &run("query-family", &s),
        r#"id: query-family:0x8A1F · Low · High · score 14.885557044199842 · share Some(0.1227096640020427)
title: The app sends 30 versions of a query that differ only in how their parameters are declared
summary: 30 variants of the same statement ran 9.0K times for 12% of database time, because their parameters are declared differently from call to call (`@P1 nvarchar(7)` and `@P1 nvarchar(12)`): drivers do this when a string parameter's size comes from each value (ADO.NET's AddWithValue, for one). SQL Server compiles and keeps a plan for each declaration. Statistics split across the copies too, so none of them looks as heavy as the statement really is.
impact: 30 copies
metric: Variants: 30 Count
metric: Executions: 9000 Count
query: 7 None -
query: 7001 None -
fix: Declare each string parameter with a fixed size, the column's length, instead of letting the driver size it from the value (in ADO.NET, set SqlParameter.Size or use Add with a SqlDbType and size rather than AddWithValue; other drivers have a similar setting). Then one plan serves every value. Forced parameterization can't help: these statements are already parameterized.
verify: Re-scan after a deploy: this family shrinks to one or a few statements.

id: query-family:0x8A1F · Low · High · score 14.885557044199842 · share Some(0.1227096640020427)
title: The app sends 30 versions of a query that differ only in literal values
summary: 30 variants of the same statement ran 9.0K times for 12% of database time, so SQL Server compiles each one separately and keeps a plan for each. Statistics split across the copies too, so none of them looks as heavy as the statement really is.
impact: 30 copies
metric: Variants: 30 Count
metric: Executions: 9000 Count
query: 7 None -
query: 7002 None -
fix: Send values as parameters (sp_executesql, or your driver's parameterized queries) so one plan serves every value. If you can't change the app, forced parameterization does it for the whole database.
sql: -- Prefer fixing the app. As a database-wide fallback:
ALTER DATABASE [culprit_demo] SET PARAMETERIZATION FORCED;
cost: Forced parameterization can make some queries pick a plan that suits one value badly (parameter sensitivity); watch the regression findings after switching.
verify: Re-scan after a deploy: this family shrinks to one or a few statements."#,
    );
}

#[test]
fn postgres_families_differ_in_their_value_lists() {
    let mut s = fixture("postgres-demo.json");
    s.query_families = vec![family("SELECT id FROM public.orders WHERE id IN ($1, $2)", 12.0, &[])];
    pin(
        "query_family_postgres_value_lists",
        &run("query-family", &s),
        r#"id: query-family:0x8A1F · Low · High · score 11.873872809325404 · share Some(0.03181645355912639)
title: A read from orders is tracked as 12 separate statements
summary: 12 variants of the same statement ran 9.0K times for 3% of database time, because its value lists (IN lists, multi-row VALUES) have different lengths; each length is planned and counted on its own. Statistics split across the copies too, so none of them looks as heavy as the statement really is.
impact: 12 copies
metric: Variants: 12 Count
metric: Executions: 9000 Count
fix: In the code that sends this statement (one copy is quoted below, as recorded), pass value lists as one array parameter (`= ANY($1)` in Postgres) instead of a growing IN list, and batch multi-row inserts with UNNEST or COPY.
verify: After a deploy, no new copies should appear: pg_stat_statements keeps an entry for each copy until it evicts it or its statistics are reset, so compare the family's copies in a scan taken when the change ships and one a day or more later (12 in this scan): the count should stay as it is, or fall.
caveat: One of them, as recorded:
SELECT id FROM public.orders WHERE id IN ($1, $2)"#,
    );

    // Members whose texts differ, no sample text, and no time in the window.
    let mut s = fixture("postgres-demo.json");
    let first = s.workload.as_ref().unwrap().queries[0].id.clone();
    twin(&mut s, &first, "90001", |q| q.text = format!("{} /* a */ AND 1 = 1", q.text));
    s.workload.as_mut().unwrap().total_time_ms = 0.0;
    s.query_families = vec![family("", 8.0, &[first.as_str(), "90001"])];
    pin(
        "query_family_postgres_no_sample",
        &run("query-family", &s),
        r#"id: query-family:0x8A1F · Low · High · score 10.709269960975831 · share None
title: A query is tracked as 8 separate statements
summary: 8 variants of the same statement ran 9.0K times, because its value lists (IN lists, multi-row VALUES) have different lengths; each length is planned and counted on its own. Statistics split across the copies too, so none of them looks as heavy as the statement really is.
impact: 8 copies
metric: Variants: 8 Count
metric: Executions: 9000 Count
query: 9065391607317686058 None -
query: 90001 None -
fix: Pass value lists as one array parameter (`= ANY($1)` in Postgres) instead of a growing IN list, and batch multi-row inserts with UNNEST or COPY.
verify: After a deploy, no new copies should appear: pg_stat_statements keeps an entry for each copy until it evicts it or its statistics are reset, so compare the family's copies in a scan taken when the change ships and one a day or more later (8 in this scan): the count should stay as it is, or fall."#,
    );
}

fn plan_cache(platform: Option<Platform>, ad_hoc: Option<&str>, single_use_gb: f64) -> Snapshot {
    let mut s = fixture("mssql-demo.json");
    s.server.set_platform(platform);
    s.settings.retain(|x| x.name != "optimize for ad hoc workloads");
    if let Some(v) = ad_hoc {
        set(&mut s, "optimize for ad hoc workloads", v);
    }
    let gb = 1024.0 * 1024.0 * 1024.0;
    s.plan_cache = Some(PlanCacheUse {
        single_use_plans: 40_000.0,
        single_use_bytes: single_use_gb * gb,
        total_plans: 50_000.0,
        total_bytes: 3.0 * gb,
    });
    s
}

#[test]
fn single_use_plans_say_where_the_setting_is_kept() {
    let mut all = Vec::new();
    for (platform, ad_hoc, gb) in [
        (None, None, 2.0),
        (Some(Platform::AzureSqlDatabase), Some("0"), 1.0),
        (Some(Platform::AmazonRds), None, 2.0),
        (None, Some("1"), 1.0),
    ] {
        all.extend(
            run("query-family", &plan_cache(platform, ad_hoc, gb))
                .into_iter()
                .filter(|f| f.id.ends_with(":plan-cache")),
        );
    }
    pin(
        "query_family_plan_cache",
        &all,
        r#"id: query-family:plan-cache · Medium · High · score 28.666666666666664 · share None
title: Ad hoc plans with one lookup fill 67% of the cache (2.0 GB)
summary: 40K ad hoc cache entries have one recorded lookup and occupy memory that could hold data pages or reusable plans. 'Optimize for ad hoc workloads' is off. Enabling it stores a small stub on a plan's first use.
impact: 2.0 GB in ad hoc entries
metric: Ad hoc entries with one lookup: 40000 Count
metric: Their memory: 2147483648 Bytes
metric: Share of the cache: 0.6666666666666666 Ratio
fix: Keep only a stub for a plan's first use, and parameterize the statements behind it.
sql: EXEC sp_configure 'show advanced options', 1;
RECONFIGURE;
EXEC sp_configure 'optimize for ad hoc workloads', 1;
RECONFIGURE;
verify: Re-scan after a day: ad hoc entries with one lookup take a smaller share of the cache.

id: query-family:plan-cache · Low · High · score 11.333333333333332 · share None
title: Ad hoc plans with one lookup fill 33% of the cache (1.0 GB)
summary: 40K ad hoc cache entries have one recorded lookup and occupy memory that could hold data pages or reusable plans. 'Optimize for ad hoc workloads' is off. Enabling it stores a small stub on a plan's first use.
impact: 1.0 GB in ad hoc entries
metric: Ad hoc entries with one lookup: 40000 Count
metric: Their memory: 1073741824 Bytes
metric: Share of the cache: 0.3333333333333333 Ratio
fix: Keep only a stub for a plan's first use, and parameterize the statements behind it.
sql: ALTER DATABASE SCOPED CONFIGURATION SET OPTIMIZE_FOR_AD_HOC_WORKLOADS = ON;
verify: Re-scan after a day: ad hoc entries with one lookup take a smaller share of the cache.

id: query-family:plan-cache · Medium · High · score 28.666666666666664 · share None
title: Ad hoc plans with one lookup fill 67% of the cache (2.0 GB)
summary: 40K ad hoc cache entries have one recorded lookup and occupy memory that could hold data pages or reusable plans. 'Optimize for ad hoc workloads' is off. Enabling it stores a small stub on a plan's first use.
impact: 2.0 GB in ad hoc entries
metric: Ad hoc entries with one lookup: 40000 Count
metric: Their memory: 2147483648 Bytes
metric: Share of the cache: 0.6666666666666666 Ratio
fix: Keep only a stub for a plan's first use, and parameterize the statements behind it. Set optimize for ad hoc workloads to 1 in the instance's DB parameter group.
verify: Re-scan after a day: ad hoc entries with one lookup take a smaller share of the cache.

id: query-family:plan-cache · Low · High · score 11.333333333333332 · share None
title: Ad hoc plans with one lookup fill 33% of the cache (1.0 GB)
summary: 40K ad hoc cache entries have one recorded lookup and occupy memory that could hold data pages or reusable plans.
impact: 1.0 GB in ad hoc entries
metric: Ad hoc entries with one lookup: 40000 Count
metric: Their memory: 1073741824 Bytes
metric: Share of the cache: 0.3333333333333333 Ratio
fix: Parameterize the statements behind it (see the families above).
verify: Re-scan after a day: ad hoc entries with one lookup take a smaller share of the cache."#,
    );
}

// ── tuning-advice ────────────────────────────────────────────────────────────────────────────────────────────────

use culprit_core::{
    AutomaticTuningOption, ForcedPlan, PlanNode, QueryHint, RecommendedIndex, TuningAdvice, TuningRecommendation,
};

fn correction(desired: &str, actual: &str, reason: Option<&str>) -> Vec<AutomaticTuningOption> {
    vec![AutomaticTuningOption {
        name: "FORCE_LAST_GOOD_PLAN".into(),
        desired: desired.into(),
        actual: actual.into(),
        reason: reason.map(str::to_string),
    }]
}

fn force(query: &str, good: &str, bad: Option<&str>) -> TuningRecommendation {
    TuningRecommendation {
        name: format!("PR_{query}"),
        kind: "FORCE_LAST_GOOD_PLAN".into(),
        state: Some("Active".into()),
        query_id: Some(query.into()),
        regressed_plan_id: bad.map(str::to_string),
        recommended_plan_id: Some(good.into()),
        ..Default::default()
    }
}

fn tuned(advice: TuningAdvice) -> Snapshot {
    let mut s = fixture("mssql-demo.json");
    s.tuning = Some(advice);
    s
}

fn advice(s: &Snapshot) -> Vec<culprit_core::Finding> {
    run("tuning-advice", s)
}

#[test]
fn query_hint_failures_keep_replica_scopes_and_clear_the_observed_replica() {
    let hints = ["1", "2"]
        .into_iter()
        .map(|group| QueryHint {
            query_id: "7".into(),
            hint_id: group.into(),
            scope: culprit_core::QueryStoreHintScope::ReplicaGroup { id: group.into() },
            hint: "OPTION(MAXDOP 1)".into(),
            failures: 3.0,
            last_failure_reason: None,
        })
        .collect();
    let findings = advice(&tuned(TuningAdvice { hints, ..Default::default() }));
    let hints: Vec<_> = findings.iter().filter(|finding| finding.id.contains("hint-failing:")).collect();
    assert_eq!(hints.len(), 2);
    assert_ne!(hints[0].id, hints[1].id);
    for group in ["1", "2"] {
        let finding = hints.iter().find(|finding| finding.id.ends_with(&format!("replica:{group}"))).unwrap();
        assert!(
            finding
                .fix
                .as_ref()
                .and_then(|fix| fix.sql.as_ref())
                .unwrap()
                .contains(&format!("@replica_group_id = {group};"))
        );
        assert_eq!(finding.caveats.len(), 1);
    }
}

#[test]
fn unknown_hint_scope_preserves_the_failure_without_issuing_a_fix_against_the_local_replica() {
    let hints = vec![QueryHint {
        hint_id: "3".into(),
        query_id: "7".into(),
        scope: culprit_core::QueryStoreHintScope::Unknown,
        hint: "OPTION(MAXDOP 1)".into(),
        failures: 3.0,
        last_failure_reason: None,
    }];
    let findings = advice(&tuned(TuningAdvice { hints, ..Default::default() }));
    let hint = findings.iter().find(|finding| finding.id.contains("hint-failing:")).unwrap();
    assert!(hint.fix.as_ref().unwrap().sql.is_none());
    assert_eq!(hint.caveats.len(), 1);
}

#[test]
fn sql_servers_recommendations_say_what_they_would_change() {
    pin_text(
        "tuning_advice_info",
        &info("tuning-advice"),
        r#"title: SQL Server's own tuning advice
description: Plan-forcing and index recommendations from automatic tuning, forced plans (those that stopped applying, and those that scan where an index added since could seek), Query Store hints that stopped applying, and automatic plan correction left off."#,
    );

    // A plan to force for a statement in the window, with automatic plan correction off: both are findings.
    let mut seven = force("7", "815", Some("685"));
    (seven.estimated_gain, seven.valid_since) = (Some(0.73), Some("2026-10-09T10:00:00Z".into()));
    seven.script = Some("exec sp_query_store_force_plan @query_id = 7, @plan_id = 815".into());
    let s = tuned(TuningAdvice {
        automatic_tuning: correction("OFF", "OFF", None),
        recommendations: vec![seven.clone()],
        ..Default::default()
    });
    pin(
        "tuning_advice_force_with_correction_off",
        &advice(&s),
        r#"id: tuning-advice:force:7 · Medium · High · score 73.75477290129452 · share Some(0.5439766525721529)
title: SQL Server suggests forcing plan 815 for a read from customers (estimated 73% less CPU per run)
summary: Automatic tuning noticed that query 7 got slower when it switched to plan 685 and recommends going back to plan 815 (since 2026-10-09 10:00:00). Forcing is reversible and Query Store keeps both plans.
impact: 54% of DB time
metric: Estimated CPU saved per run: 0.73 Ratio
metric: Recommended plan: 815
metric: Current plan: 685
query: 7 None -
fix: Force the recommended plan (copy and run it yourself; Culprit never runs it). To let SQL Server do this on its own, turn on automatic plan correction.
sql: exec sp_query_store_force_plan @query_id = 7, @plan_id = 815
verify: Re-scan: query 7 runs with plan 815 and its average duration is back to its earlier level.
verify: If it gets worse, undo with EXEC sp_query_store_unforce_plan @query_id = 7, @plan_id = 815;

id: tuning-advice:automatic-plan-correction · Low · Medium · score 12 · share None
title: Automatic plan correction is off while SQL Server has fixes waiting
summary: SQL Server found 1 plan regression it could fix by going back to the last good plan, but automatic plan correction is off, so they wait for someone to apply them.
impact: Setting
fix: Let SQL Server force the last good plan itself when a query regresses; it verifies the result and backs out if the forced plan is not better.
sql: ALTER DATABASE [culprit_demo] SET AUTOMATIC_TUNING (FORCE_LAST_GOOD_PLAN = ON);
verify: Re-scan: recommendations move from Active to Verifying and then Success."#,
    );

    // A statement that has not run for days (a deploy may have replaced it), one that fails more often, and
    // automatic plan correction on.
    let mut gone = force("123456", "12", None);
    (gone.error_prone, gone.last_execution) = (true, Some("2026-10-08T06:00:00Z".into()));
    let s = tuned(TuningAdvice {
        automatic_tuning: correction("ON", "ON", None),
        recommendations: vec![gone, force("9", "9", Some("3"))],
        ..Default::default()
    });
    pin(
        "tuning_advice_force_gone_with_correction_on",
        &advice(&s),
        r#"id: tuning-advice:force:123456 · Low · High · score 2 · share None · inactive
title: SQL Server suggests forcing plan 12 for query 123456
summary: Automatic tuning noticed that query 123456 got slower when it switched to plan ? and recommends going back to plan 12. The current plan also fails more often than the recommended one.
impact: Recommended by SQL Server
metric: Recommended plan: 12
fix: Force the recommended plan (copy and run it yourself; Culprit never runs it). Automatic plan correction (FORCE_LAST_GOOD_PLAN) is on for this database: SQL Server forces the last good plan itself once it is sure a regression costs enough (sys.dm_db_tuning_recommendations lists the ones it found), and unforces a plan it forced that turns out slower. A plan forced by hand stays forced until someone unforces it, whatever changes after.
sql: EXEC sp_query_store_force_plan @query_id = 123456, @plan_id = 12;
verify: Re-scan: query 123456 runs with plan 12 and its average duration is back to its earlier level.
verify: If it gets worse, undo with EXEC sp_query_store_unforce_plan @query_id = 123456, @plan_id = 12;
caveat: Query 123456 has not run since 2026-10-08 06:00 (3.3 days before this scan's data ends). A deploy may have changed it; check that the current version of this code doesn't have the same problem.

id: tuning-advice:force:9 · Low · High · score 40.678776880387865 · share Some(0.16547628884843782)
title: SQL Server suggests forcing plan 9 for a read from order_items
summary: Automatic tuning noticed that query 9 got slower when it switched to plan 3 and recommends going back to plan 9. Forcing is reversible and Query Store keeps both plans.
impact: 17% of DB time
metric: Recommended plan: 9
metric: Current plan: 3
query: 9 None -
fix: Force the recommended plan (copy and run it yourself; Culprit never runs it). Automatic plan correction (FORCE_LAST_GOOD_PLAN) is on for this database: SQL Server forces the last good plan itself once it is sure a regression costs enough (sys.dm_db_tuning_recommendations lists the ones it found), and unforces a plan it forced that turns out slower. A plan forced by hand stays forced until someone unforces it, whatever changes after.
sql: EXEC sp_query_store_force_plan @query_id = 9, @plan_id = 9;
verify: Re-scan: query 9 runs with plan 9 and its average duration is back to its earlier level.
verify: If it gets worse, undo with EXEC sp_query_store_unforce_plan @query_id = 9, @plan_id = 9;"#,
    );

    // Correction set on but not running, with and without a reason; off with two fixes waiting.
    let mut all = Vec::new();
    for (desired, reason) in [("ON", Some("QUERY_STORE_OFF")), ("ON", None), ("OFF", None)] {
        let s = tuned(TuningAdvice {
            automatic_tuning: correction(desired, "OFF", reason),
            recommendations: vec![force("123456", "12", None), force("123457", "13", None)],
            ..Default::default()
        });
        all.extend(advice(&s).into_iter().filter(|f| f.id == "tuning-advice:automatic-plan-correction"));
    }
    pin(
        "tuning_advice_correction",
        &all,
        r#"id: tuning-advice:automatic-plan-correction · Medium · High · score 22 · share None
title: Automatic plan correction is turned on but isn't running
summary: FORCE_LAST_GOOD_PLAN is set to ON, yet SQL Server reports it as OFF (QUERY_STORE_OFF). Plan regressions are not being corrected.
impact: Setting
fix: Usually Query Store is off or read-only; turn it back on (see the Query Store finding).
sql: ALTER DATABASE [culprit_demo] SET QUERY_STORE (OPERATION_MODE = READ_WRITE);
verify: Re-scan: FORCE_LAST_GOOD_PLAN reports ON.

id: tuning-advice:automatic-plan-correction · Medium · High · score 22 · share None
title: Automatic plan correction is turned on but isn't running
summary: FORCE_LAST_GOOD_PLAN is set to ON, yet SQL Server reports it as OFF. Plan regressions are not being corrected.
impact: Setting
fix: Usually Query Store is off or read-only; turn it back on (see the Query Store finding).
sql: ALTER DATABASE [culprit_demo] SET QUERY_STORE (OPERATION_MODE = READ_WRITE);
verify: Re-scan: FORCE_LAST_GOOD_PLAN reports ON.

id: tuning-advice:automatic-plan-correction · Low · Medium · score 12 · share None
title: Automatic plan correction is off while SQL Server has fixes waiting
summary: SQL Server found 2 plan regressions it could fix by going back to the last good plan, but automatic plan correction is off, so they wait for someone to apply them.
impact: Setting
fix: Let SQL Server force the last good plan itself when a query regresses; it verifies the result and backs out if the forced plan is not better.
sql: ALTER DATABASE [culprit_demo] SET AUTOMATIC_TUNING (FORCE_LAST_GOOD_PLAN = ON);
verify: Re-scan: recommendations move from Active to Verifying and then Success."#,
    );
}

#[test]
fn forced_plans_and_hints_that_stopped_applying_say_why() {
    let failing = |query: &str, failures: f64, reason: Option<&str>, last: Option<&str>| ForcedPlan {
        query_id: query.into(),
        plan_id: format!("4{query}"),
        failures,
        last_failure_reason: reason.map(str::to_string),
        last_execution: last.map(str::to_string),
        forcing: None,
    };
    let s = tuned(TuningAdvice {
        forced_plans: vec![
            failing("7", 34.0, Some("NO_INDEX"), None),
            failing("9", 150.0, None, None),
            failing("55555", 3.0, Some("NO_PLAN"), Some("2026-10-07T23:00:00Z")),
            failing("8", 2.0, Some("TIME_OUT"), None),
            failing("6", 2.0, Some("GENERAL_FAILURE"), None),
        ],
        hints: vec![
            QueryHint {
                query_id: "7".into(),
                hint_id: "7".into(),
                scope: culprit_core::QueryStoreHintScope::Local,
                hint: " OPTION (MAXDOP 1) ".into(),
                failures: 3.0,
                last_failure_reason: Some("QUERY_HINT_ERROR".into()),
            },
            QueryHint {
                query_id: "9".into(),
                hint_id: "9".into(),
                scope: culprit_core::QueryStoreHintScope::Local,
                hint: "OPTION (RECOMPILE)".into(),
                failures: 1.0,
                last_failure_reason: None,
            },
        ],
        ..Default::default()
    });
    pin(
        "tuning_advice_failing",
        &advice(&s),
        r#"id: tuning-advice:forced-failing:7 · Low · High · score 9.531478917042255 · share None
title: A forced plan for a read from customers stopped applying 34 times (NO_INDEX)
summary: Plan 47 is forced for query 7, but SQL Server could not use it 34 times, most recently because of NO_INDEX. Each time, it compiled a new plan instead, so the forcing gives none of the stability it was meant to. An index the plan uses no longer exists.
impact: 34 failed forcings
metric: Forcing failures: 34 Count
metric: Last reason: NO_INDEX
query: 7 None -
fix: Restore what the plan needs (often an index that was dropped or renamed), or unforce it and let SQL Server choose again.
sql: EXEC sp_query_store_unforce_plan @query_id = 7, @plan_id = 47;
verify: Re-scan: plan 47's failure count stops growing.

id: tuning-advice:forced-failing:9 · Medium · High · score 24.17609125905568 · share None
title: A forced plan for a read from order_items stopped applying 150 times (unknown reason)
summary: Plan 49 is forced for query 9, but SQL Server could not use it 150 times, most recently because of unknown reason. Each time, it compiled a new plan instead, so the forcing gives none of the stability it was meant to. 
impact: 150 failed forcings
metric: Forcing failures: 150 Count
metric: Last reason: unknown reason
query: 9 None -
fix: Restore what the plan needs (often an index that was dropped or renamed), or unforce it and let SQL Server choose again.
sql: EXEC sp_query_store_unforce_plan @query_id = 9, @plan_id = 49;
verify: Re-scan: plan 49's failure count stops growing.

id: tuning-advice:forced-failing:55555 · Low · High · score 2.1192803136799157 · share None · inactive
title: A forced plan for query 55555 stopped applying 3 times (NO_PLAN)
summary: Plan 455555 is forced for query 55555, but SQL Server could not use it 3 times, most recently because of NO_PLAN. Each time, it compiled a new plan instead, so the forcing gives none of the stability it was meant to. The query or schema changed so the plan can no longer be produced.
impact: 3 failed forcings
metric: Forcing failures: 3 Count
metric: Last reason: NO_PLAN
fix: Restore what the plan needs (often an index that was dropped or renamed), or unforce it and let SQL Server choose again.
sql: EXEC sp_query_store_unforce_plan @query_id = 55555, @plan_id = 455555;
verify: Re-scan: plan 455555's failure count stops growing.
caveat: Query 55555 has not run since 2026-10-07 23:00 (3.6 days before this scan's data ends). A deploy may have changed it; check that the current version of this code doesn't have the same problem.

id: tuning-advice:forced-failing:8 · Low · High · score 8.301029995663981 · share None
title: A forced plan for a read from orders stopped applying 2 times (TIME_OUT)
summary: Plan 48 is forced for query 8, but SQL Server could not use it 2 times, most recently because of TIME_OUT. Each time, it compiled a new plan instead, so the forcing gives none of the stability it was meant to. Compiling with the forced plan took too long.
impact: 2 failed forcings
metric: Forcing failures: 2 Count
metric: Last reason: TIME_OUT
query: 8 None -
fix: Restore what the plan needs (often an index that was dropped or renamed), or unforce it and let SQL Server choose again.
sql: EXEC sp_query_store_unforce_plan @query_id = 8, @plan_id = 48;
verify: Re-scan: plan 48's failure count stops growing.

id: tuning-advice:forced-failing:6 · Low · High · score 8.301029995663981 · share None
title: A forced plan for a read from products stopped applying 2 times (GENERAL_FAILURE)
summary: Plan 46 is forced for query 6, but SQL Server could not use it 2 times, most recently because of GENERAL_FAILURE. Each time, it compiled a new plan instead, so the forcing gives none of the stability it was meant to. The query or schema changed so the plan can no longer be produced.
impact: 2 failed forcings
metric: Forcing failures: 2 Count
metric: Last reason: GENERAL_FAILURE
query: 6 None -
fix: Restore what the plan needs (often an index that was dropped or renamed), or unforce it and let SQL Server choose again.
sql: EXEC sp_query_store_unforce_plan @query_id = 6, @plan_id = 46;
verify: Re-scan: plan 46's failure count stops growing.

id: tuning-advice:hint-failing:7 · Low · High · score 8 · share None
title: A Query Store hint on query 7 failed 3 times (QUERY_HINT_ERROR)
summary: The hint OPTION (MAXDOP 1) is set for query 7, but SQL Server could not apply it 3 times, so the query ran without it.
impact: 3 failed hints
fix: Fix or clear the hint.
sql: EXEC sys.sp_query_store_clear_hints @query_id = 7;
verify: Re-scan: the hint's failure count stops growing.

id: tuning-advice:hint-failing:9 · Low · High · score 8 · share None
title: A Query Store hint on query 9 failed once (unknown reason)
summary: The hint OPTION (RECOMPILE) is set for query 9, but SQL Server could not apply it once, so the query ran without it.
impact: 1 failed hint
fix: Fix or clear the hint.
sql: EXEC sys.sp_query_store_clear_hints @query_id = 9;
verify: Re-scan: the hint's failure count stops growing."#,
    );
}

#[test]
fn forced_plans_are_listed_with_who_forced_them() {
    let plan = |query: &str, plan: &str, forcing: Option<&str>| ForcedPlan {
        query_id: query.into(),
        plan_id: plan.into(),
        forcing: forcing.map(str::to_string),
        ..Default::default()
    };
    let mut applied = force("7", "999", Some("3"));
    (applied.state, applied.applied_at) = (Some("Success".into()), Some("2026-10-03T08:00:00Z".into()));
    let mut gone = plan("9001", "4", None);
    gone.last_execution = Some("2026-10-02T06:00:00Z".into());
    let many: Vec<ForcedPlan> = (0..14)
        .map(|i| plan(&format!("80{i:02}"), &format!("{i}"), Some(if i < 3 { "AUTO" } else { "MANUAL" })))
        .collect();
    let cases = [
        (vec![plan("7", "999", Some("AUTO"))], vec![applied], None),
        (vec![gone], vec![], None),
        (
            vec![plan("7", "999", Some("AUTO")), plan("9001", "5", Some("AUTO")), plan("9002", "6", Some("AUTO"))],
            vec![],
            Some("ON"),
        ),
        (vec![plan("7", "999", Some("AUTO")), plan("9003", "4", Some("MANUAL"))], vec![], Some("ON")),
        (many, vec![], Some("ON")),
        (vec![plan("9004", "4", Some("MANUAL")), plan("9005", "5", Some("MANUAL"))], vec![], Some("OFF")),
    ];
    let mut all = Vec::new();
    for (forced_plans, recommendations, on) in cases {
        let s = tuned(TuningAdvice {
            automatic_tuning: on.map(|on| correction(on, on, None)).unwrap_or_default(),
            recommendations,
            forced_plans,
            ..Default::default()
        });
        all.extend(advice(&s).into_iter().filter(|f| f.id == "tuning-advice:forced-plans"));
    }
    pin(
        "tuning_advice_forced_plans",
        &all,
        r#"id: tuning-advice:forced-plans · Low · High · score 8.2 · share None
title: Automatic tuning forced a plan in Query Store
summary: SQL Server runs this query with the plan forced for it instead of choosing one, even after an index is added or the data grows. A forced plan keeps a query on the plan it ran fast with, so a new plan can't slow it down. Automatic plan correction forced it after the query got slower with a new plan. Once the query or its indexes change, a forced plan can become the slower one without anything saying so.
impact: 1 forced plan
metric: Forced plans: 1 Count
metric: Forced by automatic tuning: 1 Count
query: 7 None Plan 999 is forced.
fix: Nothing to do while they help. Unforce one whose query or indexes changed since it was forced and compare its duration before and after: SQL Server chooses a plan again.
sql: -- One at a time, comparing the query's duration before and after
-- EXEC sp_query_store_unforce_plan @query_id = 7, @plan_id = 999;
verify: Re-scan after unforcing one: its query's average duration stays the same or drops.
caveat: Forced plans:
Query 7 (a read from customers): plan 999, forced by automatic tuning on Oct 3.

id: tuning-advice:forced-plans · Low · High · score 8.2 · share None
title: A plan is forced in Query Store
summary: SQL Server runs this query with the plan forced for it instead of choosing one, even after an index is added or the data grows. A forced plan keeps a query on the plan it ran fast with, so a new plan can't slow it down. Once the query or its indexes change, a forced plan can become the slower one without anything saying so.
impact: 1 forced plan
metric: Forced plans: 1 Count
fix: Nothing to do while they help. Unforce one whose query or indexes changed since it was forced and compare its duration before and after: SQL Server chooses a plan again.
sql: -- One at a time, comparing the query's duration before and after
-- EXEC sp_query_store_unforce_plan @query_id = 9001, @plan_id = 4;
verify: Re-scan after unforcing one: its query's average duration stays the same or drops.
caveat: Forced plans:
Query 9001: plan 4, forced; not run since Oct 2.

id: tuning-advice:forced-plans · Low · High · score 8.6 · share None
title: 3 plans are forced in Query Store, all by automatic tuning
summary: SQL Server runs these queries with the plan forced for each instead of choosing one, even after an index is added or the data grows. A forced plan keeps a query on the plan it ran fast with, so a new plan can't slow it down. Automatic plan correction forced all of them after their queries got slower with a new plan. It is still on, so it keeps checking the plans it forced and unforces one that turns out slower.
impact: 3 forced plans
metric: Forced plans: 3 Count
metric: Forced by automatic tuning: 3 Count
query: 7 None Plan 999 is forced.
fix: Nothing to do while they help. Unforce one whose query or indexes changed since it was forced and compare its duration before and after: SQL Server chooses a plan again.
sql: -- One at a time, comparing the query's duration before and after
-- EXEC sp_query_store_unforce_plan @query_id = 7, @plan_id = 999;
-- EXEC sp_query_store_unforce_plan @query_id = 9001, @plan_id = 5;
-- EXEC sp_query_store_unforce_plan @query_id = 9002, @plan_id = 6;
verify: Re-scan after unforcing one: its query's average duration stays the same or drops.
caveat: Forced plans:
Query 7 (a read from customers): plan 999, forced by automatic tuning.
Query 9001: plan 5, forced by automatic tuning.
Query 9002: plan 6, forced by automatic tuning.

id: tuning-advice:forced-plans · Low · High · score 8.4 · share None
title: 2 plans are forced in Query Store, 1 by automatic tuning
summary: SQL Server runs these queries with the plan forced for each instead of choosing one, even after an index is added or the data grows. A forced plan keeps a query on the plan it ran fast with, so a new plan can't slow it down. Automatic plan correction forced one of them after its query got slower with a new plan. It is still on, so it keeps checking the plans it forced and unforces one that turns out slower. The other plan stays forced until someone unforces it: once its query or indexes change, it can become the slower one without anything saying so.
impact: 2 forced plans
metric: Forced plans: 2 Count
metric: Forced by automatic tuning: 1 Count
query: 7 None Plan 999 is forced.
fix: Nothing to do while they help. Unforce one whose query or indexes changed since it was forced and compare its duration before and after: SQL Server chooses a plan again.
sql: -- One at a time, comparing the query's duration before and after
-- EXEC sp_query_store_unforce_plan @query_id = 7, @plan_id = 999;
-- EXEC sp_query_store_unforce_plan @query_id = 9003, @plan_id = 4;
verify: Re-scan after unforcing one: its query's average duration stays the same or drops.
caveat: Forced plans:
Query 7 (a read from customers): plan 999, forced by automatic tuning.
Query 9003: plan 4, forced by hand.

id: tuning-advice:forced-plans · Low · High · score 10 · share None
title: 14 plans are forced in Query Store, 3 by automatic tuning
summary: SQL Server runs these queries with the plan forced for each instead of choosing one, even after an index is added or the data grows. A forced plan keeps a query on the plan it ran fast with, so a new plan can't slow it down. Automatic plan correction forced 3 of them after their queries got slower with a new plan. It is still on, so it keeps checking the plans it forced and unforces one that turns out slower. The other 11 stay forced until someone unforces them: once their queries or indexes change, one can become the slower one without anything saying so.
impact: 14 forced plans
metric: Forced plans: 14 Count
metric: Forced by automatic tuning: 3 Count
fix: Nothing to do while they help. Unforce one whose query or indexes changed since it was forced and compare its duration before and after: SQL Server chooses a plan again.
sql: -- One at a time, comparing the query's duration before and after
-- EXEC sp_query_store_unforce_plan @query_id = 8000, @plan_id = 0;
-- EXEC sp_query_store_unforce_plan @query_id = 8001, @plan_id = 1;
-- EXEC sp_query_store_unforce_plan @query_id = 8002, @plan_id = 2;
-- EXEC sp_query_store_unforce_plan @query_id = 8003, @plan_id = 3;
-- EXEC sp_query_store_unforce_plan @query_id = 8004, @plan_id = 4;
-- EXEC sp_query_store_unforce_plan @query_id = 8005, @plan_id = 5;
-- EXEC sp_query_store_unforce_plan @query_id = 8006, @plan_id = 6;
-- EXEC sp_query_store_unforce_plan @query_id = 8007, @plan_id = 7;
-- EXEC sp_query_store_unforce_plan @query_id = 8008, @plan_id = 8;
-- EXEC sp_query_store_unforce_plan @query_id = 8009, @plan_id = 9;
-- EXEC sp_query_store_unforce_plan @query_id = 8010, @plan_id = 10;
-- EXEC sp_query_store_unforce_plan @query_id = 8011, @plan_id = 11;
verify: Re-scan after unforcing one: its query's average duration stays the same or drops.
caveat: Forced plans:
Query 8000: plan 0, forced by automatic tuning.
Query 8001: plan 1, forced by automatic tuning.
Query 8002: plan 2, forced by automatic tuning.
Query 8003: plan 3, forced by hand.
Query 8004: plan 4, forced by hand.
Query 8005: plan 5, forced by hand.
Query 8006: plan 6, forced by hand.
Query 8007: plan 7, forced by hand.
Query 8008: plan 8, forced by hand.
Query 8009: plan 9, forced by hand.
Query 8010: plan 10, forced by hand.
Query 8011: plan 11, forced by hand.
And 2 more.

id: tuning-advice:forced-plans · Low · High · score 8.4 · share None
title: 2 plans are forced in Query Store
summary: SQL Server runs these queries with the plan forced for each instead of choosing one, even after an index is added or the data grows. A forced plan keeps a query on the plan it ran fast with, so a new plan can't slow it down. Once the query or its indexes change, a forced plan can become the slower one without anything saying so.
impact: 2 forced plans
metric: Forced plans: 2 Count
fix: Nothing to do while they help. Unforce one whose query or indexes changed since it was forced and compare its duration before and after: SQL Server chooses a plan again.
sql: -- One at a time, comparing the query's duration before and after
-- EXEC sp_query_store_unforce_plan @query_id = 9004, @plan_id = 4;
-- EXEC sp_query_store_unforce_plan @query_id = 9005, @plan_id = 5;
verify: Re-scan after unforcing one: its query's average duration stays the same or drops.
caveat: Forced plans:
Query 9004: plan 4, forced by hand.
Query 9005: plan 5, forced by hand."#,
    );
}

fn node_mut(node: &mut PlanNode, id: u32) -> Option<&mut PlanNode> {
    if node.id == id {
        return Some(node);
    }
    node.children.iter_mut().find_map(|child| node_mut(child, id))
}

#[test]
fn a_forced_plan_that_scans_past_an_index_says_so() {
    let mut applied = force("9", "9", Some("3"));
    (applied.state, applied.applied_at) = (Some("Verifying".into()), Some("2026-10-03T08:00:00Z".into()));
    let mut s = tuned(TuningAdvice {
        recommendations: vec![applied],
        forced_plans: vec![ForcedPlan {
            query_id: "9".into(),
            plan_id: "9".into(),
            forcing: Some("AUTO".into()),
            ..Default::default()
        }],
        ..Default::default()
    });
    let mut index = s.indexes.iter().find(|i| i.name == "ix_order_items_product_id").unwrap().clone();
    index.name = "ix_order_items_order_id".into();
    index.columns = vec!["order_id".into()];
    s.indexes.push(index);
    let mut all = advice(&s).into_iter().filter(|f| f.id == "tuning-advice:forced-scan:9").collect::<Vec<_>>();
    // Dated after the forcing: most likely forced before it existed.
    s.indexes.last_mut().unwrap().created_at_latest = Some("2026-10-08T00:00:00Z".into());
    all.extend(advice(&s).into_iter().filter(|f| f.id == "tuning-advice:forced-scan:9"));
    // Forced by hand, or by no one the view names, past an index that holds every column the query returns.
    s.indexes.last_mut().unwrap().include = vec!["product_id".into(), "quantity".into(), "unit_price".into()];
    for forcing in [Some("MANUAL"), None] {
        let tuning = s.tuning.as_mut().unwrap();
        tuning.recommendations.clear();
        tuning.forced_plans[0].forcing = forcing.map(str::to_string);
        all.extend(advice(&s).into_iter().filter(|f| f.id == "tuning-advice:forced-scan:9"));
    }
    pin(
        "tuning_advice_forced_scan",
        &all,
        r#"id: tuning-advice:forced-scan:9 · Low · Medium · score 40.678776880387865 · share Some(0.16547628884843782)
title: A forced plan makes a read from order_items scan order_items though ix_order_items_order_id could seek it
summary: Plan 9 is forced by automatic tuning on Oct 3 for query 9. SQL Server runs a forced plan as it is, even after an index is added, so this one reads all of shop.order_items (1.5M rows) and keeps about <1% of them to find the rows by order_id, while ix_order_items_order_id leads with it and could seek them. ix_order_items_order_id doesn't hold product_id, quantity and unit_price, so a seek would look each row it finds up in the table: still far less than reading every row for the <1% it keeps.
impact: 17% of DB time
metric: Forced plan: 9
metric: Index that could seek: ix_order_items_order_id
metric: Rows in table: 1500000 Rows
query: 9 Some(1) Scans order_items under the forced plan.
fix: Unforce it, so SQL Server compiles a plan that can seek ix_order_items_order_id. If automatic plan correction is on and the new plan turns out slower, it forces a plan again, chosen among the plans it has seen since.
sql: EXEC sp_query_store_unforce_plan @query_id = 9, @plan_id = 9;
verify: Re-scan: query 9 seeks ix_order_items_order_id, and its reads and average duration drop.
verify: If it gets slower, force the plan again: EXEC sp_query_store_force_plan @query_id = 9, @plan_id = 9;

id: tuning-advice:forced-scan:9 · Low · Medium · score 40.678776880387865 · share Some(0.16547628884843782)
title: A forced plan makes a read from order_items scan order_items though ix_order_items_order_id could seek it
summary: Plan 9 is forced by automatic tuning on Oct 3 for query 9. SQL Server runs a forced plan as it is, even after an index is added, so this one reads all of shop.order_items (1.5M rows) and keeps about <1% of them to find the rows by order_id, while ix_order_items_order_id leads with it and could seek them. ix_order_items_order_id doesn't hold product_id, quantity and unit_price, so a seek would look each row it finds up in the table: still far less than reading every row for the <1% it keeps. ix_order_items_order_id may be new (Culprit dates it to Oct 8 at the latest), so this plan was most likely forced before it existed.
impact: 17% of DB time
metric: Forced plan: 9
metric: Index that could seek: ix_order_items_order_id
metric: Rows in table: 1500000 Rows
query: 9 Some(1) Scans order_items under the forced plan.
fix: Unforce it, so SQL Server compiles a plan that can seek ix_order_items_order_id. If automatic plan correction is on and the new plan turns out slower, it forces a plan again, chosen among the plans it has seen since.
sql: EXEC sp_query_store_unforce_plan @query_id = 9, @plan_id = 9;
verify: Re-scan: query 9 seeks ix_order_items_order_id, and its reads and average duration drop.
verify: If it gets slower, force the plan again: EXEC sp_query_store_force_plan @query_id = 9, @plan_id = 9;

id: tuning-advice:forced-scan:9 · Low · Medium · score 40.678776880387865 · share Some(0.16547628884843782)
title: A forced plan makes a read from order_items scan order_items though ix_order_items_order_id could seek it
summary: Plan 9 is forced by hand for query 9. SQL Server runs a forced plan as it is, even after an index is added, so this one reads all of shop.order_items (1.5M rows) and keeps about <1% of them to find the rows by order_id, while ix_order_items_order_id leads with it and could seek them. ix_order_items_order_id may be new (Culprit dates it to Oct 8 at the latest), so this plan was most likely forced before it existed.
impact: 17% of DB time
metric: Forced plan: 9
metric: Index that could seek: ix_order_items_order_id
metric: Rows in table: 1500000 Rows
query: 9 Some(1) Scans order_items under the forced plan.
fix: Unforce it, so SQL Server compiles a plan that can seek ix_order_items_order_id. If automatic plan correction is on and the new plan turns out slower, it forces a plan again, chosen among the plans it has seen since.
sql: EXEC sp_query_store_unforce_plan @query_id = 9, @plan_id = 9;
verify: Re-scan: query 9 seeks ix_order_items_order_id, and its reads and average duration drop.
verify: If it gets slower, force the plan again: EXEC sp_query_store_force_plan @query_id = 9, @plan_id = 9;

id: tuning-advice:forced-scan:9 · Low · Medium · score 40.678776880387865 · share Some(0.16547628884843782)
title: A forced plan makes a read from order_items scan order_items though ix_order_items_order_id could seek it
summary: Plan 9 is forced for query 9. SQL Server runs a forced plan as it is, even after an index is added, so this one reads all of shop.order_items (1.5M rows) and keeps about <1% of them to find the rows by order_id, while ix_order_items_order_id leads with it and could seek them. ix_order_items_order_id may be new (Culprit dates it to Oct 8 at the latest), so this plan was most likely forced before it existed.
impact: 17% of DB time
metric: Forced plan: 9
metric: Index that could seek: ix_order_items_order_id
metric: Rows in table: 1500000 Rows
query: 9 Some(1) Scans order_items under the forced plan.
fix: Unforce it, so SQL Server compiles a plan that can seek ix_order_items_order_id. If automatic plan correction is on and the new plan turns out slower, it forces a plan again, chosen among the plans it has seen since.
sql: EXEC sp_query_store_unforce_plan @query_id = 9, @plan_id = 9;
verify: Re-scan: query 9 seeks ix_order_items_order_id, and its reads and average duration drop.
verify: If it gets slower, force the plan again: EXEC sp_query_store_force_plan @query_id = 9, @plan_id = 9;"#,
    );
}

/// Azure's recommendation of an index on `table` (`columns`, including `include`), scored `score`.
fn recommend(table: &str, columns: &[&str], include: &[&str], score: f64) -> TuningRecommendation {
    TuningRecommendation {
        name: format!("IR_{table}"),
        kind: "CreateIndex".into(),
        state: Some("Active".into()),
        score: Some(score),
        valid_since: Some("2026-10-06T00:00:00Z".into()),
        index: Some(RecommendedIndex {
            schema: "shop".into(),
            table: table.into(),
            name: Some(format!("nci_msft_{table}")),
            columns: columns.iter().map(|c| c.to_string()).collect(),
            include: include.iter().map(|c| c.to_string()).collect(),
        }),
        ..Default::default()
    }
}

fn recommended(recommendations: Vec<TuningRecommendation>) -> Snapshot {
    tuned(TuningAdvice { recommendations, ..Default::default() })
}

/// A statement on support_tickets like query 10, as `id`, with `text`, its runs and time.
fn ticket_statement(s: &mut Snapshot, id: &str, text: &str, time_ms: f64, plan: bool) {
    let w = s.workload.as_mut().expect("workload");
    let mut q = w.queries.iter().find(|q| q.id == "10").expect("query 10").clone();
    (q.id, q.query_hash, q.plans) = (id.into(), None, Vec::new());
    if !plan {
        q.plan = None;
    }
    q.text = text.into();
    (q.executions, q.total_time_ms) = (2000.0, time_ms);
    q.mean_time_ms = q.total_time_ms / q.executions;
    w.queries.push(q);
}

#[test]
fn azure_index_recommendations_say_what_they_would_add() {
    // Recommendations nothing else has: one on columns no index leads with, one an index there could include, one with
    // no date or score, and two to drop an index read now and then (with and without Azure's reason).
    let mut drop = recommend("orders", &[], &[], 20.0);
    drop.kind = "DropIndex".into();
    drop.index.as_mut().unwrap().name = Some("ix_orders_channel".into());
    let mut drop_why = drop.clone();
    (drop_why.reason, drop_why.valid_since) = (Some("Duplicate index".into()), None);
    drop_why.index.as_mut().unwrap().name = Some("ix_support_tickets_priority".into());
    drop_why.index.as_mut().unwrap().table = "support_tickets".into();
    let mut undated = recommend("products", &["price"], &["name"], 64.0);
    (undated.valid_since, undated.score) = (None, None);
    let mut s = recommended(vec![
        recommend("orders", &["channel", "created_at"], &[], 64.0),
        recommend("customers", &["email"], &["segment"], 64.0),
        undated,
        drop,
        drop_why,
    ]);
    let priority = s.indexes.iter_mut().find(|i| i.name == "ix_support_tickets_priority").unwrap();
    priority.definition =
        Some("CREATE INDEX [ix_support_tickets_priority] ON [shop].[support_tickets] ([priority])".into());
    priority.reads = None;
    pin(
        "tuning_advice_azure_indexes",
        &advice(&s),
        r#"id: tuning-advice:create-index:shop.orders(channel,created_at) · Medium · Medium · score 28.4 · share None
title: Azure SQL recommends an index on orders (channel and created_at)
summary: Azure SQL Database's automatic tuning recommends an index on shop.orders (channel and created_at), since Oct 6, and scores it 64 out of 100. It weighs what the queries that read orders would save against what the index costs every write. The optimizer's own missing-index requests since the last restart don't include it; none of the statements this scan captured compares all of channel and created_at with a value: the ones it was recommended for may have changed, stopped running, or not be among them.
impact: Recommended by Azure SQL
metric: Azure's score (0–100): 64 Count
metric: Rows in table: 1000000 Rows
fix: Create it online, outside the busiest hours. Azure SQL can also create recommended indexes itself (automatic tuning's CREATE_INDEX) and drops one that doesn't help.
sql: CREATE NONCLUSTERED INDEX [nci_msft_orders] ON [shop].[orders] ([channel], [created_at]) WITH (ONLINE = ON);
verify: Re-scan a day later: the recommendation is gone, and the queries that read orders do fewer reads.
caveat: ix_orders_channel already leads with channel, 1 of the 2 recommended keys: check whether the queries that read orders use it before adding an index beside it.

id: tuning-advice:create-index:shop.customers(email) · Medium · Medium · score 28.4 · share None
title: Azure SQL recommends an index on customers (email)
summary: Azure SQL Database's automatic tuning recommends an index on shop.customers (email, including segment), since Oct 6, and scores it 64 out of 100. It weighs what the queries that read customers would save against what the index costs every write. The optimizer's own missing-index requests since the last restart don't include it; a read from customers (below) compares email with a value.
impact: Recommended by Azure SQL
metric: Azure's score (0–100): 64 Count
metric: Rows in table: 200000 Rows
query: 7 None -
fix: ix_customers_email already has the right keys but makes every matching row go back to the table for segment. Rebuild it with those columns included so it covers the query. The statement keeps its keys, their order, its filter and the columns it already includes; restate DATA_COMPRESSION, FILLFACTOR or its partition scheme in it if the index uses them.
sql: CREATE NONCLUSTERED INDEX [ix_customers_email]
    ON [shop].[customers] ([email])
    INCLUDE ([segment])
    WITH (DROP_EXISTING = ON, ONLINE = ON);
cost: The index grows from 7.1 MB; writes that touch the included columns maintain it.
verify: Re-scan a day later: the recommendation is gone, and the queries that read customers do fewer reads.
caveat: ix_customers_email already leads with email; the recommended index adds segment, which spares the queries that read it the lookups back to the table. Including it in ix_customers_email does the same with one index fewer to maintain.

id: tuning-advice:create-index:shop.products(price) · Low · Medium · score 8 · share None
title: Azure SQL recommends an index on products (price)
summary: Azure SQL Database's automatic tuning recommends an index on shop.products (price, including name). It weighs what the queries that read products would save against what the index costs every write. The optimizer's own missing-index requests since the last restart don't include it; none of the statements this scan captured compares price with a value: the ones it was recommended for may have changed, stopped running, or not be among them.
impact: Recommended by Azure SQL
metric: Rows in table: 20000 Rows
fix: Create it online, outside the busiest hours. Azure SQL can also create recommended indexes itself (automatic tuning's CREATE_INDEX) and drops one that doesn't help.
sql: CREATE NONCLUSTERED INDEX [nci_msft_products] ON [shop].[products] ([price]) INCLUDE ([name]) WITH (ONLINE = ON);
verify: Re-scan a day later: the recommendation is gone, and the queries that read products do fewer reads.

id: tuning-advice:drop-index:shop.orders.ix_orders_channel · Low · Medium · score 9 · share None
title: Azure SQL recommends dropping ix_orders_channel on orders
summary: Azure SQL Database's automatic tuning recommends dropping ix_orders_channel since Oct 6, judging from a longer history than the usage counters here, which count 1 reads of it in the minute since the server last restarted.
impact: Recommended by Azure SQL
metric: Size: 22290432 Bytes
metric: Reads: 1 Count
fix: Disable it first: the definition stays, nothing reads or maintains it, and a rebuild brings it back. Drop it after a full business cycle without complaints.
sql: ALTER INDEX [ix_orders_channel] ON [shop].[orders] DISABLE;
verify: Watch the queries on orders for a week after disabling; none should get slower.

id: tuning-advice:drop-index:shop.support_tickets.ix_support_tickets_priority · Low · Medium · score 9 · share None
title: Azure SQL recommends dropping ix_support_tickets_priority on support_tickets
summary: Azure SQL Database's automatic tuning recommends dropping ix_support_tickets_priority (Duplicate index), judging from a longer history than the usage counters here.
impact: Recommended by Azure SQL
metric: Size: 3481600 Bytes
fix: Disable it first: the definition stays, nothing reads or maintains it, and a rebuild brings it back. Drop it after a full business cycle without complaints.
sql: ALTER INDEX [ix_support_tickets_priority] ON [shop].[support_tickets] DISABLE;
verify: Watch the queries on support_tickets for a week after disabling; none should get slower.
caveat: Its definition, to recreate it:
CREATE INDEX [ix_support_tickets_priority] ON [shop].[support_tickets] ([priority])"#,
    );
}

#[test]
fn azure_index_recommendations_are_weighed_against_the_indexes_there() {
    let mut s = fixture("mssql-demo.json");
    s.columns.iter_mut().find(|c| c.table == "support_tickets" && c.name == "subject").unwrap().data_type =
        "nvarchar(max)".into();
    let mut index = s.indexes.iter().find(|i| i.name == "ix_support_tickets_customer_id").unwrap().clone();
    index.name = "ix_support_tickets_customer_priority".into();
    index.columns = vec!["customer_id".into(), "priority".into()];
    s.indexes.push(index);
    let mut filtered = s.indexes.iter().find(|i| i.name == "ix_support_tickets_customer_id").unwrap().clone();
    filtered.name = "ix_support_tickets_opened_open".into();
    filtered.columns = vec!["opened_at".into()];
    filtered.filter = Some("([status]='open')".into());
    s.indexes.push(filtered);
    s.tuning = Some(TuningAdvice {
        recommendations: vec![
            recommend("support_tickets", &["priority", "customer_id"], &["subject"], 2.0),
            recommend("support_tickets", &["customer_id", "priority"], &["subject", "status"], 40.0),
            recommend("support_tickets", &["opened_at", "status"], &[], 3.0),
            recommend("support_tickets", &["customer_id", "status"], &[], 60.0),
        ],
        ..Default::default()
    });
    pin(
        "tuning_advice_weighed",
        &advice(&s),
        r#"id: tuning-advice:create-index:shop.support_tickets(priority,customer_id) · Low · Medium · score 8.2 · share None
title: Azure SQL recommends an index on support_tickets (priority and customer_id)
summary: Azure SQL Database's automatic tuning recommends an index on shop.support_tickets (priority and customer_id, including subject), since Oct 6, and scores it 2 out of 100. It weighs what the queries that read support_tickets would save against what the index costs every write. The optimizer's own missing-index requests since the last restart don't include it; none of the statements this scan captured compares all of priority and customer_id with a value: the ones it was recommended for may have changed, stopped running, or not be among them.
impact: Recommended by Azure SQL
metric: Azure's score (0–100): 2 Count
metric: Rows in table: 300000 Rows
fix: Azure scores it 2 out of 100: it expects the index to save little next to what it costs every write, so it's usually not worth having. Nothing to create: ix_support_tickets_customer_priority already finds these rows by customer_id and priority, and the recommended index would only copy subject into a second index. Let the recommendation expire.
verify: Nothing to change: once Azure lets the recommendation expire, this finding goes with it.

id: tuning-advice:create-index:shop.support_tickets(customer_id,priority) · Low · Medium · score 12 · share None
title: Azure SQL recommends an index on support_tickets (customer_id and priority)
summary: Azure SQL Database's automatic tuning recommends an index on shop.support_tickets (customer_id and priority, including subject and status), since Oct 6, and scores it 40 out of 100. It weighs what the queries that read support_tickets would save against what the index costs every write. The optimizer's own missing-index requests since the last restart don't include it; none of the statements this scan captured compares all of customer_id and priority with a value: the ones it was recommended for may have changed, stopped running, or not be among them.
impact: Recommended by Azure SQL
metric: Azure's score (0–100): 40 Count
metric: Rows in table: 300000 Rows
fix: ix_support_tickets_customer_priority already has the right keys but makes every matching row go back to the table for status. Rebuild it with those columns included so it covers the query. The statement keeps its keys, their order, its filter and the columns it already includes; restate DATA_COMPRESSION, FILLFACTOR or its partition scheme in it if the index uses them.
sql: CREATE NONCLUSTERED INDEX [ix_support_tickets_customer_priority]
    ON [shop].[support_tickets] ([customer_id], [priority])
    INCLUDE ([status])
    WITH (DROP_EXISTING = ON, ONLINE = ON);
cost: The index grows from 4.3 MB; writes that touch the included columns maintain it.
verify: Re-scan a day later: the recommendation is gone, and the queries that read support_tickets do fewer reads.
caveat: ix_support_tickets_customer_priority already leads with customer_id and priority; the recommended index adds status, which spares the queries that read it the lookups back to the table. Including it in ix_support_tickets_customer_priority does the same with one index fewer to maintain.
caveat: Left subject out of INCLUDE: its type can hold large values, and including it would copy whatever it holds into the index. The rows the index finds go back to the table for it.

id: tuning-advice:create-index:shop.support_tickets(opened_at,status) · Low · Medium · score 8.3 · share None
title: Azure SQL recommends an index on support_tickets (opened_at and status)
summary: Azure SQL Database's automatic tuning recommends an index on shop.support_tickets (opened_at and status), since Oct 6, and scores it 3 out of 100. It weighs what the queries that read support_tickets would save against what the index costs every write. The optimizer's own missing-index requests since the last restart don't include it; none of the statements this scan captured compares all of opened_at and status with a value: the ones it was recommended for may have changed, stopped running, or not be among them.
impact: Recommended by Azure SQL
metric: Azure's score (0–100): 3 Count
metric: Rows in table: 300000 Rows
fix: Azure scores it 3 out of 100: it expects the index to save little next to what it costs every write, so it's usually not worth having. If the queries that read support_tickets are slow all the same, create it online, outside the busiest hours.
sql: CREATE NONCLUSTERED INDEX [nci_msft_support_tickets] ON [shop].[support_tickets] ([opened_at], [status]) WITH (ONLINE = ON);
verify: Only if you apply it: re-scan a day later; the recommendation is gone, and the queries that read support_tickets do fewer reads.
caveat: ix_support_tickets_opened_open already leads with opened_at for the rows where status = 'open', 1 of the 2 recommended keys: check whether the queries that read support_tickets use it before adding an index beside it.

id: tuning-advice:create-index:shop.support_tickets(customer_id,status) · Medium · Medium · score 28 · share None
title: Azure SQL recommends an index on support_tickets (customer_id and status)
summary: Azure SQL Database's automatic tuning recommends an index on shop.support_tickets (customer_id and status), since Oct 6, and scores it 60 out of 100. It weighs what the queries that read support_tickets would save against what the index costs every write. The optimizer's own missing-index requests since the last restart don't include it; none of the statements this scan captured compares all of customer_id and status with a value: the ones it was recommended for may have changed, stopped running, or not be among them.
impact: Recommended by Azure SQL
metric: Azure's score (0–100): 60 Count
metric: Rows in table: 300000 Rows
fix: Create it online, outside the busiest hours. Azure SQL can also create recommended indexes itself (automatic tuning's CREATE_INDEX) and drops one that doesn't help.
sql: CREATE NONCLUSTERED INDEX [nci_msft_support_tickets] ON [shop].[support_tickets] ([customer_id], [status]) WITH (ONLINE = ON);
verify: Re-scan a day later: the recommendation is gone, and the queries that read support_tickets do fewer reads.
caveat: ix_support_tickets_customer_id already leads with customer_id, 1 of the 2 recommended keys: check whether the queries that read support_tickets use it before adding an index beside it."#,
    );
}

#[test]
fn a_low_score_beside_fast_statements_is_nothing_to_create() {
    let bit = |s: &mut Snapshot, name: &str| {
        s.columns.push(
            serde_json::from_value(serde_json::json!({
                "schema": "shop", "table": "support_tickets", "name": name, "data_type": "bit",
                "nullable": false, "default": null, "generated": false, "ordinal": 40
            }))
            .unwrap(),
        )
    };
    // One bit key, two included columns, one statement.
    let mut s = recommended(vec![recommend("support_tickets", &["is_deleted"], &["status", "claimed_by"], 3.0)]);
    bit(&mut s, "is_deleted");
    ticket_statement(
        &mut s,
        "777",
        "SELECT [id], [status] FROM [shop].[support_tickets] WHERE [is_deleted] = 0 AND [id] = @P1",
        600.0,
        false,
    );
    let mut all = advice(&s);
    // Two bit keys, three statements of different speeds.
    let mut s = recommended(vec![recommend("support_tickets", &["is_deleted", "is_spam"], &[], 3.0)]);
    bit(&mut s, "is_deleted");
    bit(&mut s, "is_spam");
    for (id, ms) in [("781", 600.0), ("782", 2000.0), ("783", 9000.0)] {
        ticket_statement(
            &mut s,
            id,
            &format!(
                "SELECT [id] FROM [shop].[support_tickets] WHERE [is_deleted] = 0 AND [is_spam] = 0 AND [id] > {id}"
            ),
            ms,
            false,
        );
    }
    all.extend(advice(&s));
    pin(
        "tuning_advice_fast_statements",
        &all,
        r#"id: tuning-advice:create-index:shop.support_tickets(is_deleted) · Low · Medium · score 8.3 · share None
title: Azure SQL recommends an index on support_tickets (is_deleted)
summary: Azure SQL Database's automatic tuning recommends an index on shop.support_tickets (is_deleted, including status and claimed_by), since Oct 6, and scores it 3 out of 100. It weighs what the queries that read support_tickets would save against what the index costs every write. The optimizer's own missing-index requests since the last restart don't include it; a read from support_tickets (below) compares is_deleted with a value.
impact: Recommended by Azure SQL
metric: Azure's score (0–100): 3 Count
metric: Rows in table: 300000 Rows
query: 777 None -
fix: Azure scores it 3 out of 100: it expects the index to save little next to what it costs every write, so it's usually not worth having. Nothing to create: the statement this scan captured that compares is_deleted with a value averages 0.30 ms a run, so there is no slow read for it to speed up, and its key, is_deleted, is a bit column: it holds at most two values, so an index on it narrows the rows little; every write that changes one of the 2 columns it includes would update it too. Let the recommendation expire.
verify: Nothing to change: once Azure lets the recommendation expire, this finding goes with it.

id: tuning-advice:create-index:shop.support_tickets(is_deleted,is_spam) · Low · Medium · score 8.3 · share None
title: Azure SQL recommends an index on support_tickets (is_deleted and is_spam)
summary: Azure SQL Database's automatic tuning recommends an index on shop.support_tickets (is_deleted and is_spam), since Oct 6, and scores it 3 out of 100. It weighs what the queries that read support_tickets would save against what the index costs every write. The optimizer's own missing-index requests since the last restart don't include it; 3 statements this scan captured (below) compare all of is_deleted and is_spam with a value.
impact: Recommended by Azure SQL
metric: Azure's score (0–100): 3 Count
metric: Rows in table: 300000 Rows
query: 783 None -
query: 782 None -
query: 781 None -
fix: Azure scores it 3 out of 100: it expects the index to save little next to what it costs every write, so it's usually not worth having. Nothing to create: the 3 statements this scan captured that compare all of is_deleted and is_spam with a value average 0.30 ms–4.5 ms a run, so there is no slow read for it to speed up, and its keys, is_deleted and is_spam, are bit columns: each holds at most two values, so an index on them narrows the rows little. Let the recommendation expire.
verify: Nothing to change: once Azure lets the recommendation expire, this finding goes with it."#,
    );
}

#[test]
fn a_low_azure_score_gives_way_to_a_heavy_statement() {
    let text = "SELECT [id], [subject] FROM [shop].[support_tickets] WHERE [customer_id] = @P1 AND [status] = @P2";
    let with_statement = |heavy: bool, plan: bool, score: f64, estimate: f64, rows: f64| {
        let mut s = recommended(vec![recommend("support_tickets", &["customer_id", "status"], &[], score)]);
        ticket_statement(&mut s, "777", text, if heavy { 40_000.0 } else { 40.0 }, plan);
        let w = s.workload.as_mut().unwrap();
        if heavy {
            w.total_time_ms = 400_000.0;
        }
        let q = w.queries.iter_mut().find(|q| q.id == "777").unwrap();
        (q.logical_reads, q.rows) = (Some(1_500_000.0), Some(rows));
        if let Some(seek) = q.plan.as_mut().and_then(|p| node_mut(&mut p.root, 2)) {
            seek.est_rows = estimate;
        }
        advice(&s)
    };
    let mut all = Vec::new();
    for (heavy, plan, score, estimate, rows) in [
        (true, false, 3.0, 1.0, 0.0),
        (false, false, 3.0, 1.0, 0.0),
        (true, true, 3.0, 1.0, 0.0),
        (true, true, 60.0, 1.0, 0.0),
        (true, true, 3.0, 288.0, 0.0),
        (true, false, 60.0, 1.0, 0.0),
        // Less than a row a run, and a seek expected to find two.
        (true, false, 3.0, 1.0, 1000.0),
        (true, true, 3.0, 2.0, 1000.0),
        (true, true, 60.0, 2.0, 0.0),
    ] {
        all.extend(with_statement(heavy, plan, score, estimate, rows));
    }
    // No statement on the keys.
    all.extend(advice(&recommended(vec![recommend("support_tickets", &["customer_id", "status"], &[], 3.0)])));
    pin(
        "tuning_advice_heavy_statement",
        &all,
        r#"id: tuning-advice:create-index:shop.support_tickets(customer_id,status) · Low · Medium · score 8.3 · share None
title: Azure SQL recommends an index on support_tickets (customer_id and status)
summary: Azure SQL Database's automatic tuning recommends an index on shop.support_tickets (customer_id and status), since Oct 6, and scores it 3 out of 100. It weighs what the queries that read support_tickets would save against what the index costs every write. The optimizer's own missing-index requests since the last restart don't include it; a read from support_tickets (below) compares all of customer_id and status with a value.
impact: Recommended by Azure SQL
metric: Azure's score (0–100): 3 Count
metric: Rows in table: 300000 Rows
query: 777 None -
fix: Azure scores it 3 out of 100, for what it expects the index to save next to what it costs every write, but a read from support_tickets, query 777 below, filters on exactly these columns, takes 10% of database time and reads 750 pages a run to return nothing: an index on them would find its rows directly. Create it online, outside the busiest hours.
sql: CREATE NONCLUSTERED INDEX [nci_msft_support_tickets] ON [shop].[support_tickets] ([customer_id], [status]) WITH (ONLINE = ON);
verify: Re-scan a day later: the recommendation is gone, and query 777 reads far fewer than 750 pages a run.
caveat: ix_support_tickets_customer_id already leads with customer_id, 1 of the 2 recommended keys: check whether the queries that read support_tickets use it before adding an index beside it.

id: tuning-advice:create-index:shop.support_tickets(customer_id,status) · Low · Medium · score 8.3 · share None
title: Azure SQL recommends an index on support_tickets (customer_id and status)
summary: Azure SQL Database's automatic tuning recommends an index on shop.support_tickets (customer_id and status), since Oct 6, and scores it 3 out of 100. It weighs what the queries that read support_tickets would save against what the index costs every write. The optimizer's own missing-index requests since the last restart don't include it; a read from support_tickets (below) compares all of customer_id and status with a value.
impact: Recommended by Azure SQL
metric: Azure's score (0–100): 3 Count
metric: Rows in table: 300000 Rows
query: 777 None -
fix: Azure scores it 3 out of 100: it expects the index to save little next to what it costs every write, so it's usually not worth having. Nothing to create: the statement this scan captured that compares all of customer_id and status with a value averages 0.02 ms a run, so there is no slow read for it to speed up. Let the recommendation expire.
verify: Nothing to change: once Azure lets the recommendation expire, this finding goes with it.

id: tuning-advice:create-index:shop.support_tickets(customer_id,status) · Low · Medium · score 8.3 · share None
title: Azure SQL recommends an index on support_tickets (customer_id and status)
summary: Azure SQL Database's automatic tuning recommends an index on shop.support_tickets (customer_id and status), since Oct 6, and scores it 3 out of 100. It weighs what the queries that read support_tickets would save against what the index costs every write. The optimizer's own missing-index requests since the last restart don't include it; a read from support_tickets (below) compares all of customer_id and status with a value.
impact: Recommended by Azure SQL
metric: Azure's score (0–100): 3 Count
metric: Rows in table: 300000 Rows
query: 777 None -
fix: Azure scores it 3 out of 100: it expects the index to save little next to what it costs every write, so it's usually not worth having. Nothing to create: ix_support_tickets_customer_id already leads with customer_id, which query 777 compares with a value, but its plan picks ix_support_tickets_priority expecting 1 row, and its runs read 750 pages each to return nothing. The recorded pages do not establish why that plan was chosen: inspect a representative actual plan before choosing a change (see heavy-query:777). Let the recommendation expire.
verify: Nothing to create here: gather the actual-plan and IO evidence described in heavy-query:777 first. After a change supported by that evidence, compare equivalent runs of query 777 for lower reads and time per run. When Azure lets the recommendation expire, this finding goes with it.

id: tuning-advice:create-index:shop.support_tickets(customer_id,status) · Medium · Medium · score 28 · share None
title: Azure SQL recommends an index on support_tickets (customer_id and status)
summary: Azure SQL Database's automatic tuning recommends an index on shop.support_tickets (customer_id and status), since Oct 6, and scores it 60 out of 100. It weighs what the queries that read support_tickets would save against what the index costs every write. The optimizer's own missing-index requests since the last restart don't include it; a read from support_tickets (below) compares all of customer_id and status with a value.
impact: Recommended by Azure SQL
metric: Azure's score (0–100): 60 Count
metric: Rows in table: 300000 Rows
query: 777 None -
fix: Create it online, outside the busiest hours. Azure SQL can also create recommended indexes itself (automatic tuning's CREATE_INDEX) and drops one that doesn't help.
sql: CREATE NONCLUSTERED INDEX [nci_msft_support_tickets] ON [shop].[support_tickets] ([customer_id], [status]) WITH (ONLINE = ON);
verify: Re-scan a day later: the recommendation is gone, and the queries that read support_tickets do fewer reads.
caveat: ix_support_tickets_customer_id already leads with customer_id, 1 of the 2 recommended keys, but query 777's plan reads support_tickets through ix_support_tickets_priority instead, expecting 1 row: heavy-query:777 is about that choice.

id: tuning-advice:create-index:shop.support_tickets(customer_id,status) · Low · Medium · score 8.3 · share None
title: Azure SQL recommends an index on support_tickets (customer_id and status)
summary: Azure SQL Database's automatic tuning recommends an index on shop.support_tickets (customer_id and status), since Oct 6, and scores it 3 out of 100. It weighs what the queries that read support_tickets would save against what the index costs every write. The optimizer's own missing-index requests since the last restart don't include it; a read from support_tickets (below) compares all of customer_id and status with a value.
impact: Recommended by Azure SQL
metric: Azure's score (0–100): 3 Count
metric: Rows in table: 300000 Rows
query: 777 None -
fix: Azure scores it 3 out of 100, for what it expects the index to save next to what it costs every write, but a read from support_tickets, query 777 below, filters on exactly these columns, takes 10% of database time and reads 750 pages a run to return nothing: an index on them would find its rows directly. Create it online, outside the busiest hours.
sql: CREATE NONCLUSTERED INDEX [nci_msft_support_tickets] ON [shop].[support_tickets] ([customer_id], [status]) WITH (ONLINE = ON);
verify: Re-scan a day later: the recommendation is gone, and query 777 reads far fewer than 750 pages a run.
caveat: ix_support_tickets_customer_id already leads with customer_id, 1 of the 2 recommended keys, but query 777's plan reads support_tickets through ix_support_tickets_priority instead, so that index isn't serving it.

id: tuning-advice:create-index:shop.support_tickets(customer_id,status) · Medium · Medium · score 28 · share None
title: Azure SQL recommends an index on support_tickets (customer_id and status)
summary: Azure SQL Database's automatic tuning recommends an index on shop.support_tickets (customer_id and status), since Oct 6, and scores it 60 out of 100. It weighs what the queries that read support_tickets would save against what the index costs every write. The optimizer's own missing-index requests since the last restart don't include it; a read from support_tickets (below) compares all of customer_id and status with a value.
impact: Recommended by Azure SQL
metric: Azure's score (0–100): 60 Count
metric: Rows in table: 300000 Rows
query: 777 None -
fix: Create it online, outside the busiest hours. Azure SQL can also create recommended indexes itself (automatic tuning's CREATE_INDEX) and drops one that doesn't help.
sql: CREATE NONCLUSTERED INDEX [nci_msft_support_tickets] ON [shop].[support_tickets] ([customer_id], [status]) WITH (ONLINE = ON);
verify: Re-scan a day later: the recommendation is gone, and query 777 reads far fewer than 750 pages a run.
caveat: ix_support_tickets_customer_id already leads with customer_id, 1 of the 2 recommended keys: check whether the queries that read support_tickets use it before adding an index beside it.

id: tuning-advice:create-index:shop.support_tickets(customer_id,status) · Low · Medium · score 8.3 · share None
title: Azure SQL recommends an index on support_tickets (customer_id and status)
summary: Azure SQL Database's automatic tuning recommends an index on shop.support_tickets (customer_id and status), since Oct 6, and scores it 3 out of 100. It weighs what the queries that read support_tickets would save against what the index costs every write. The optimizer's own missing-index requests since the last restart don't include it; a read from support_tickets (below) compares all of customer_id and status with a value.
impact: Recommended by Azure SQL
metric: Azure's score (0–100): 3 Count
metric: Rows in table: 300000 Rows
query: 777 None -
fix: Azure scores it 3 out of 100, for what it expects the index to save next to what it costs every write, but a read from support_tickets, query 777 below, filters on exactly these columns, takes 10% of database time and reads 750 pages a run to return less than a row: an index on them would find its rows directly. Create it online, outside the busiest hours.
sql: CREATE NONCLUSTERED INDEX [nci_msft_support_tickets] ON [shop].[support_tickets] ([customer_id], [status]) WITH (ONLINE = ON);
verify: Re-scan a day later: the recommendation is gone, and query 777 reads far fewer than 750 pages a run.
caveat: ix_support_tickets_customer_id already leads with customer_id, 1 of the 2 recommended keys: check whether the queries that read support_tickets use it before adding an index beside it.

id: tuning-advice:create-index:shop.support_tickets(customer_id,status) · Low · Medium · score 8.3 · share None
title: Azure SQL recommends an index on support_tickets (customer_id and status)
summary: Azure SQL Database's automatic tuning recommends an index on shop.support_tickets (customer_id and status), since Oct 6, and scores it 3 out of 100. It weighs what the queries that read support_tickets would save against what the index costs every write. The optimizer's own missing-index requests since the last restart don't include it; a read from support_tickets (below) compares all of customer_id and status with a value.
impact: Recommended by Azure SQL
metric: Azure's score (0–100): 3 Count
metric: Rows in table: 300000 Rows
query: 777 None -
fix: Azure scores it 3 out of 100: it expects the index to save little next to what it costs every write, so it's usually not worth having. Nothing to create: ix_support_tickets_customer_id already leads with customer_id, which query 777 compares with a value, but its plan picks ix_support_tickets_priority expecting 2 rows, and its runs read 750 pages each to return less than a row. The recorded pages do not establish why that plan was chosen: inspect a representative actual plan before choosing a change (see heavy-query:777). Let the recommendation expire.
verify: Nothing to create here: gather the actual-plan and IO evidence described in heavy-query:777 first. After a change supported by that evidence, compare equivalent runs of query 777 for lower reads and time per run. When Azure lets the recommendation expire, this finding goes with it.

id: tuning-advice:create-index:shop.support_tickets(customer_id,status) · Medium · Medium · score 28 · share None
title: Azure SQL recommends an index on support_tickets (customer_id and status)
summary: Azure SQL Database's automatic tuning recommends an index on shop.support_tickets (customer_id and status), since Oct 6, and scores it 60 out of 100. It weighs what the queries that read support_tickets would save against what the index costs every write. The optimizer's own missing-index requests since the last restart don't include it; a read from support_tickets (below) compares all of customer_id and status with a value.
impact: Recommended by Azure SQL
metric: Azure's score (0–100): 60 Count
metric: Rows in table: 300000 Rows
query: 777 None -
fix: Create it online, outside the busiest hours. Azure SQL can also create recommended indexes itself (automatic tuning's CREATE_INDEX) and drops one that doesn't help.
sql: CREATE NONCLUSTERED INDEX [nci_msft_support_tickets] ON [shop].[support_tickets] ([customer_id], [status]) WITH (ONLINE = ON);
verify: Re-scan a day later: the recommendation is gone, and the queries that read support_tickets do fewer reads.
caveat: ix_support_tickets_customer_id already leads with customer_id, 1 of the 2 recommended keys, but query 777's plan reads support_tickets through ix_support_tickets_priority instead, expecting 2 rows: heavy-query:777 is about that choice.

id: tuning-advice:create-index:shop.support_tickets(customer_id,status) · Low · Medium · score 8.3 · share None
title: Azure SQL recommends an index on support_tickets (customer_id and status)
summary: Azure SQL Database's automatic tuning recommends an index on shop.support_tickets (customer_id and status), since Oct 6, and scores it 3 out of 100. It weighs what the queries that read support_tickets would save against what the index costs every write. The optimizer's own missing-index requests since the last restart don't include it; none of the statements this scan captured compares all of customer_id and status with a value: the ones it was recommended for may have changed, stopped running, or not be among them.
impact: Recommended by Azure SQL
metric: Azure's score (0–100): 3 Count
metric: Rows in table: 300000 Rows
fix: Azure scores it 3 out of 100: it expects the index to save little next to what it costs every write, so it's usually not worth having. If the queries that read support_tickets are slow all the same, create it online, outside the busiest hours.
sql: CREATE NONCLUSTERED INDEX [nci_msft_support_tickets] ON [shop].[support_tickets] ([customer_id], [status]) WITH (ONLINE = ON);
verify: Only if you apply it: re-scan a day later; the recommendation is gone, and the queries that read support_tickets do fewer reads.
caveat: ix_support_tickets_customer_id already leads with customer_id, 1 of the 2 recommended keys: check whether the queries that read support_tickets use it before adding an index beside it."#,
    );
}

#[test]
fn a_recommendation_counts_the_statements_on_its_columns() {
    let mut all = Vec::new();
    for n in [3, 7] {
        let mut s = recommended(vec![recommend("support_tickets", &["status"], &[], 30.0)]);
        for i in 0..n {
            ticket_statement(
                &mut s,
                &format!("90{i}"),
                &format!("SELECT [id] FROM [shop].[support_tickets] WHERE [status] = @P1 AND [priority] = {i}"),
                10.0 + i as f64,
                false,
            );
        }
        all.extend(advice(&s));
    }
    // An index already finds the rows by the keys; the recommendation only adds a large column.
    let mut s = recommended(vec![recommend("customers", &["email"], &["notes"], 2.0)]);
    let mut notes = s.columns.iter().find(|c| c.table == "customers" && c.name == "name").unwrap().clone();
    (notes.name, notes.data_type) = ("notes".into(), "nvarchar(max)".into());
    s.columns.push(notes);
    all.extend(advice(&s));
    pin(
        "tuning_advice_statement_counts",
        &all,
        r#"id: tuning-advice:create-index:shop.support_tickets(status) · Low · Medium · score 11 · share None
title: Azure SQL recommends an index on support_tickets (status)
summary: Azure SQL Database's automatic tuning recommends an index on shop.support_tickets (status), since Oct 6, and scores it 30 out of 100. It weighs what the queries that read support_tickets would save against what the index costs every write. The optimizer's own missing-index requests since the last restart don't include it; 3 statements this scan captured (below) compare status with a value.
impact: Recommended by Azure SQL
metric: Azure's score (0–100): 30 Count
metric: Rows in table: 300000 Rows
query: 902 None -
query: 901 None -
query: 900 None -
fix: Create it online, outside the busiest hours. Azure SQL can also create recommended indexes itself (automatic tuning's CREATE_INDEX) and drops one that doesn't help.
sql: CREATE NONCLUSTERED INDEX [nci_msft_support_tickets] ON [shop].[support_tickets] ([status]) WITH (ONLINE = ON);
verify: Re-scan a day later: the recommendation is gone, and the queries that read support_tickets do fewer reads.

id: tuning-advice:create-index:shop.support_tickets(status) · Low · Medium · score 11 · share None
title: Azure SQL recommends an index on support_tickets (status)
summary: Azure SQL Database's automatic tuning recommends an index on shop.support_tickets (status), since Oct 6, and scores it 30 out of 100. It weighs what the queries that read support_tickets would save against what the index costs every write. The optimizer's own missing-index requests since the last restart don't include it; 7 statements this scan captured compare status with a value; the 5 that take the most time are below.
impact: Recommended by Azure SQL
metric: Azure's score (0–100): 30 Count
metric: Rows in table: 300000 Rows
query: 906 None -
query: 905 None -
query: 904 None -
query: 903 None -
query: 902 None -
fix: Create it online, outside the busiest hours. Azure SQL can also create recommended indexes itself (automatic tuning's CREATE_INDEX) and drops one that doesn't help.
sql: CREATE NONCLUSTERED INDEX [nci_msft_support_tickets] ON [shop].[support_tickets] ([status]) WITH (ONLINE = ON);
verify: Re-scan a day later: the recommendation is gone, and the queries that read support_tickets do fewer reads.

id: tuning-advice:create-index:shop.customers(email) · Low · Medium · score 8.2 · share None
title: Azure SQL recommends an index on customers (email)
summary: Azure SQL Database's automatic tuning recommends an index on shop.customers (email, including notes), since Oct 6, and scores it 2 out of 100. It weighs what the queries that read customers would save against what the index costs every write. The optimizer's own missing-index requests since the last restart don't include it; a read from customers (below) compares email with a value.
impact: Recommended by Azure SQL
metric: Azure's score (0–100): 2 Count
metric: Rows in table: 200000 Rows
query: 7 None -
fix: Azure scores it 2 out of 100: it expects the index to save little next to what it costs every write, so it's usually not worth having. Nothing to create: ix_customers_email already finds these rows by email, and the recommended index would only copy notes into a second index. Let the recommendation expire.
verify: Nothing to change: once Azure lets the recommendation expire, this finding goes with it."#,
    );
}
