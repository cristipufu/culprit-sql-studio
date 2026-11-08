//! Plan insights as findings: one per problem and table, however many statements' plans show it.

use culprit_core::{Finding, Snapshot};

fn report(name: &str) -> Snapshot {
    let path = format!("{}/../../fixtures/{name}", env!("CARGO_MANIFEST_DIR"));
    let report: culprit_core::Report = serde_json::from_slice(&std::fs::read(path).expect("fixture")).expect("report");
    report.snapshot
}

/// What the `plan-insights` check finds in `s`, before the report refines it.
fn plan_insights(s: &Snapshot) -> Vec<Finding> {
    culprit_checks::all().into_iter().find(|c| c.info().id == "plan-insights").expect("the check").run(s)
}

/// `s` with each statement that has a plan sent a second time under another id: the same plan problems, in twice the
/// statements.
fn twice(mut s: Snapshot) -> Snapshot {
    let w = s.workload.as_mut().expect("a workload");
    let copies: Vec<_> = w
        .queries
        .iter()
        .filter(|q| q.plan.is_some())
        .map(|q| {
            let mut copy = q.clone();
            copy.id = format!("{}0", q.id);
            copy
        })
        .collect();
    w.queries.extend(copies);
    s
}

// What plan-insights says, word for word, of a problem one statement's plan shows and of one several share (SQL
// Server's demo, and SQLite's index built on every run): its messages come from locales/en.json.
#[test]
fn plan_insights_say_what_the_plans_show_in_english() {
    assert_eq!(
        crate::info("plan-insights"),
        "Plan insights\n\
        Key lookups, functions on filtered columns, leading wildcards, NOT IN, scalar functions, eager spools, misestimates and repeated loops in the plans of queries that matter."
    );
    let said = |name: &str| plan_insights(&twice(report(name))).iter().map(crate::words).collect::<Vec<_>>();
    assert_eq!(
        said("mssql-demo.report.json"),
        [
            "SQL Server estimates an index on shop.order_items (order_id) would make this 99% cheaper · Low · Medium\n\
                The optimizer noticed it had to read shop.order_items without a fitting index (1.5M rows read by Clustered Index Scan). Its estimate is a guide, not a promise: check the columns against the other queries on order_items. Seen in a read from order_items. The same problem shows up in 4 queries that together take 12% of database time.\n\
                impact: 12% of DB time\n\
                metric: Share of DB time\n\
                metric: Queries affected\n\
                metric: Share of the plan's work\n\
                metric: Average duration\n\
                note: The optimizer's own index suggestion\n\
                note: The optimizer's own index suggestion\n\
                note: The optimizer's own index suggestion\n\
                note: The optimizer's own index suggestion\n\
                fix: Create an index on order_id.\n\
                sql: CREATE NONCLUSTERED INDEX [IX_order_items_order_id]\n    ON [shop].[order_items] ([order_id])\n    WITH (ONLINE = ON);\n\
                verify: Obtain a newly compiled plan and compare reads and execution time with equivalent parameters and load. Check index usage and write cost; a retained scan or old stored suggestion alone does not establish success or failure.\n\
                verify: Re-scan: this finding is gone.\n\
                caveat: Based on the estimated plan; actual row counts can tell a different story.",
            "SQL Server estimates an index on shop.orders (shipped_at) would make this 81% cheaper · Low · Medium\n\
                The optimizer noticed it had to read shop.orders without a fitting index (1.0M rows read by Clustered Index Scan). Its estimate is a guide, not a promise: check the columns against the other queries on orders. Seen in a read from orders. The same problem shows up in 2 queries that together take 3% of database time.\n\
                impact: 3% of DB time\n\
                metric: Share of DB time\n\
                metric: Queries affected\n\
                metric: Share of the plan's work\n\
                metric: Average duration\n\
                note: The optimizer's own index suggestion\n\
                note: The optimizer's own index suggestion\n\
                fix: Create an index on shipped_at, including total.\n\
                sql: CREATE NONCLUSTERED INDEX [IX_orders_shipped_at]\n    ON [shop].[orders] ([shipped_at])\n    INCLUDE ([total])\n    WITH (ONLINE = ON);\n\
                verify: Obtain a newly compiled plan and compare reads and execution time with equivalent parameters and load. Check index usage and write cost; a retained scan or old stored suggestion alone does not establish success or failure.\n\
                verify: Re-scan: this finding is gone.\n\
                caveat: Based on the estimated plan; actual row counts can tell a different story.",
            "Compares each of 1.5M rows of order_items with 999 rows the Table Spool replays (1.5B pairs) · Medium · Medium\n\
                The Table Spool reads order_items once and replays its 999 rows for each row of order_items; the join then checks `Expr1003 = Expr1004` on every pair. 100% of this plan's work. The query forces a loop join, so the optimizer didn't weigh a hash or merge join. Seen in a read from order_items.\n\
                impact: 27% of DB time\n\
                metric: Share of DB time\n\
                metric: Share of the plan's work\n\
                metric: Average duration\n\
                note: A nested loop rereading its inner side thousands of times\n\
                fix: The query forces a loop join (LOOP JOIN / OPTION (LOOP JOIN)); remove the hint and let the optimizer choose.\n\
                verify: Explain again without the hint: a hash or merge join instead of the loop.\n\
                verify: Re-scan: this finding is gone.\n\
                caveat: Based on the estimated plan; actual row counts can tell a different story.",
            "This plan runs on one CPU because its statement says OPTION (MAXDOP 1) · Medium · Medium\n\
                Its estimated cost is 7152, above this server's cost threshold for parallelism (5) and the 50 it's commonly raised to, so SQL Server would consider a parallel plan for it either way; the whole run happens on a single core instead. Seen in a read from order_items.\n\
                impact: 27% of DB time\n\
                metric: Share of DB time\n\
                metric: Average duration\n\
                note: An expensive plan that can't go parallel\n\
                fix: Remove OPTION (MAXDOP 1) from the statement, unless something needs it to run on one CPU.\n\
                verify: Explain again: no NonParallelPlanReason, and Parallelism operators appear.\n\
                verify: Re-scan: this finding is gone.\n\
                caveat: Based on the estimated plan; actual row counts can tell a different story.",
            "Compares each of 1.5M rows of order_items with 999 rows the Table Spool replays (1.5B pairs) · Medium · Medium\n\
                The Table Spool reads order_items once and replays its 999 rows for each row of order_items; the join then checks `Expr1003 = Expr1004` on every pair. 100% of this plan's work. The query forces a loop join, so the optimizer didn't weigh a hash or merge join. Seen in a read from order_items.\n\
                impact: 27% of DB time\n\
                metric: Share of DB time\n\
                metric: Share of the plan's work\n\
                metric: Average duration\n\
                note: A nested loop rereading its inner side thousands of times\n\
                fix: The query forces a loop join (LOOP JOIN / OPTION (LOOP JOIN)); remove the hint and let the optimizer choose.\n\
                verify: Explain again without the hint: a hash or merge join instead of the loop.\n\
                verify: Re-scan: this finding is gone.\n\
                caveat: Based on the estimated plan; actual row counts can tell a different story.",
            "This plan runs on one CPU because its statement says OPTION (MAXDOP 1) · Medium · Medium\n\
                Its estimated cost is 7152, above this server's cost threshold for parallelism (5) and the 50 it's commonly raised to, so SQL Server would consider a parallel plan for it either way; the whole run happens on a single core instead. Seen in a read from order_items.\n\
                impact: 27% of DB time\n\
                metric: Share of DB time\n\
                metric: Average duration\n\
                note: An expensive plan that can't go parallel\n\
                fix: Remove OPTION (MAXDOP 1) from the statement, unless something needs it to run on one CPU.\n\
                verify: Explain again: no NonParallelPlanReason, and Parallelism operators appear.\n\
                verify: Re-scan: this finding is gone.\n\
                caveat: Based on the estimated plan; actual row counts can tell a different story.",
        ]
    );
    assert_eq!(
        said("sqlite-demo.report.json"),
        ["SQLite builds a temporary index on orders every time this runs · Medium · Medium\n\
        No index on orders fits this join, so SQLite builds one on customer_id for each run and throws it away. Seen in a read from customers. The same problem shows up in 2 queries that together take 38% of database time.\n\
        impact: 38% of DB time\n\
        metric: Share of DB time\n\
        metric: Queries affected\n\
        metric: Average duration\n\
        note: SQLite builds a throwaway index on every run because none fits\n\
        note: SQLite builds a throwaway index on every run because none fits\n\
        fix: Create that index once, so the work happens one time instead of on every run.\n\
        sql: CREATE INDEX IF NOT EXISTS \"ix_orders_customer_id\"\n    ON \"orders\" (\"customer_id\");\n\
        -- SQLite builds an index under the database's write lock: writers wait until it finishes.\n\
        verify: Explain again: no AUTOMATIC INDEX, and a SEARCH using the new index.\n\
        verify: Re-scan: this finding is gone."]
    );
}
