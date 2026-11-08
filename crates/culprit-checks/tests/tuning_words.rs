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
title: Plans used once fill 67% of the plan cache (2.0 GB)
summary: 40K plans in the cache were compiled for a statement that ran exactly once. They take memory that could hold data pages and reusable plans, and every one cost a compilation. 'Optimize for ad hoc workloads' is off, so each keeps a full plan instead of a small stub.
impact: 2.0 GB single-use
metric: Single-use plans: 40000 Count
metric: Their memory: 2147483648 Bytes
metric: Share of the cache: 0.6666666666666666 Ratio
fix: Keep only a stub for a plan's first use, and parameterize the statements behind it.
sql: EXEC sp_configure 'show advanced options', 1;
RECONFIGURE;
EXEC sp_configure 'optimize for ad hoc workloads', 1;
RECONFIGURE;
verify: Re-scan after a day: single-use plans take a much smaller share of the cache.

id: query-family:plan-cache · Low · High · score 11.333333333333332 · share None
title: Plans used once fill 33% of the plan cache (1.0 GB)
summary: 40K plans in the cache were compiled for a statement that ran exactly once. They take memory that could hold data pages and reusable plans, and every one cost a compilation. 'Optimize for ad hoc workloads' is off, so each keeps a full plan instead of a small stub.
impact: 1.0 GB single-use
metric: Single-use plans: 40000 Count
metric: Their memory: 1073741824 Bytes
metric: Share of the cache: 0.3333333333333333 Ratio
fix: Keep only a stub for a plan's first use, and parameterize the statements behind it.
sql: ALTER DATABASE SCOPED CONFIGURATION SET OPTIMIZE_FOR_AD_HOC_WORKLOADS = ON;
verify: Re-scan after a day: single-use plans take a much smaller share of the cache.

id: query-family:plan-cache · Medium · High · score 28.666666666666664 · share None
title: Plans used once fill 67% of the plan cache (2.0 GB)
summary: 40K plans in the cache were compiled for a statement that ran exactly once. They take memory that could hold data pages and reusable plans, and every one cost a compilation. 'Optimize for ad hoc workloads' is off, so each keeps a full plan instead of a small stub.
impact: 2.0 GB single-use
metric: Single-use plans: 40000 Count
metric: Their memory: 2147483648 Bytes
metric: Share of the cache: 0.6666666666666666 Ratio
fix: Keep only a stub for a plan's first use, and parameterize the statements behind it. Set optimize for ad hoc workloads to 1 in the instance's DB parameter group.
verify: Re-scan after a day: single-use plans take a much smaller share of the cache.

id: query-family:plan-cache · Low · High · score 11.333333333333332 · share None
title: Plans used once fill 33% of the plan cache (1.0 GB)
summary: 40K plans in the cache were compiled for a statement that ran exactly once. They take memory that could hold data pages and reusable plans, and every one cost a compilation.
impact: 1.0 GB single-use
metric: Single-use plans: 40000 Count
metric: Their memory: 1073741824 Bytes
metric: Share of the cache: 0.3333333333333333 Ratio
fix: Parameterize the statements behind it (see the families above).
verify: Re-scan after a day: single-use plans take a much smaller share of the cache."#,
    );
}
