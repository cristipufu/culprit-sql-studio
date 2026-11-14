//! What plan insights say, word for word, on every captured plan in `tests/insights` and `tests/plans`: each rule's
//! title, detail, fix, SQL and verify step, and the note on what the plan can't show. The words come from
//! `locales/en.json`; this pins the English, so moving them into the catalog changes none of it.

use std::fmt::Write;
use std::path::Path;

use culprit_core::insights::{coverage_note, insights};
use culprit_engine::plans::parse_plan;

/// Every plan in `dir` (a directory of this crate), in name order, as its insights read.
fn words(dir: &str) -> String {
    let base = Path::new(env!("CARGO_MANIFEST_DIR")).join(dir);
    let mut names: Vec<String> = std::fs::read_dir(&base)
        .unwrap()
        .filter_map(|e| e.ok()?.file_name().into_string().ok())
        .filter(|n| n != "samples.json" && [".sqlplan", ".json", ".txt"].iter().any(|ext| n.ends_with(ext)))
        .collect();
    names.sort();
    let mut out = String::new();
    for name in names {
        let text = std::fs::read_to_string(base.join(&name)).unwrap();
        let plan = parse_plan(&text).unwrap_or_else(|e| panic!("{name}: {}", e.message));
        let _ = writeln!(out, "== {dir}/{name}");
        if let Some(note) = coverage_note(&plan) {
            let _ = writeln!(out, "note: {note}");
        }
        for i in insights(&plan) {
            let _ = writeln!(out, "[{}] {}", i.rule, i.title);
            let _ = writeln!(out, "  detail: {}", i.detail);
            let _ = writeln!(out, "  fix: {}", i.fix);
            if let Some(sql) = &i.fix_sql {
                let _ = writeln!(out, "  sql: {}", sql.replace('\n', "\n       "));
            }
            let _ = writeln!(out, "  verify: {}", i.verify);
        }
    }
    out
}

#[test]
fn plan_insights_say_the_same_english_word_for_word() {
    let actual = format!("{}{}", words("tests/insights"), words("tests/plans"));
    let (mut a, mut e) = (actual.lines(), EXPECTED.lines());
    let mut line = 0;
    loop {
        line += 1;
        match (a.next(), e.next()) {
            (None, None) => break,
            (got, wanted) => assert_eq!(got, wanted, "line {line} of what plan insights say"),
        }
    }
}

const EXPECTED: &str = r##"== tests/insights/mssql-cross.sqlplan
note: Estimated plan: misestimates, spills, unused memory grants and uneven threads need an actual plan.
[no-join-predicate] products and customers are joined without a condition: every row with every row
  detail: Nested Loops has no join condition, so its output is the product of both inputs (10 rows planned). That's almost always a missing ON clause.
  fix: Add the missing join condition; if you really want every combination, write CROSS JOIN so the intent is clear.
  verify: Explain again: the join has a condition and its row count is reasonable.
== tests/insights/mssql-grant.sqlplan
[missing-index-hint] SQL Server estimates an index on shop.orders (customer_id) would make this 77% cheaper
  detail: The optimizer noticed it had to read shop.orders without a fitting index (1.0M rows read by Clustered Index Scan). Its estimate is a guide, not a promise: check the columns against the other queries on orders.
  fix: Create the index on customer_id and include total and created_at.
  sql: CREATE NONCLUSTERED INDEX [IX_orders_customer_id]
           ON [shop].[orders] ([customer_id])
           INCLUDE ([total], [created_at]);
       -- Add WITH (ONLINE = ON) on Enterprise or Azure SQL to keep the table available while it builds.
  verify: Explain again: the suggestion is gone and the read of orders becomes a seek.
[memory-grant] Reserves 41.5 MB of memory per run and uses 1.5 MB
  detail: The grant is sized from row estimates before the query runs; when they're too high, memory sits reserved and other queries wait for it (RESOURCE_SEMAPHORE). SQL Server flags it too: Excessive Grant.
  fix: Fix the estimates (update statistics, avoid functions on filtered columns). SQL Server 2019+ shrinks grants on later runs (memory grant feedback); as a last resort cap it with OPTION (MAX_GRANT_PERCENT = 5).
  verify: Run with an actual plan: GrantedMemory is close to MaxUsedMemory.
== tests/insights/mssql-implicit.sqlplan
note: Estimated plan: misestimates, spills, unused memory grants and uneven threads need an actual plan.
[implicit-conversion] A type mismatch on customers.email stops SQL Server from seeking an index
  detail: The value it's compared with has a different type, so SQL Server converts customers.email to nvarchar(200) on every row (`CONVERT_IMPLICIT(nvarchar(200),email,0) = @1`) and, by its own warning, can't seek the index.
  fix: Make the types match: send the value as non-Unicode text (SqlDbType.VarChar in .NET, sql.VarChar in Node's mssql, a non-Unicode mapping in your ORM), or, if your schema keeps text as Unicode, change customers.email to nvarchar(200). Changing a column rewrites it and the indexes on it.
  verify: Explain again: no CONVERT_IMPLICIT in the plan, and the scan became a seek.
== tests/insights/mssql-like.sqlplan
note: Estimated plan: misestimates, spills, unused memory grants and uneven threads need an actual plan.
== tests/insights/mssql-lookups.sqlplan
[key-lookups] Looks up 20K rows in shop.orders one at a time
  detail: ix_orders_status finds the rows but doesn't hold customer_id, channel, total, created_at and shipped_at, so SQL Server goes back to shop.orders for each one (Key Lookup). Together that's 100% of this plan's work.
  fix: Add customer_id, channel, total, created_at and shipped_at to ix_orders_status as included columns (keep its keys and its existing included columns) so the seek answers the query by itself, or select fewer columns.
  verify: Explain again: the Key Lookup is gone and ix_orders_status returns the rows by itself.
[memory-grant] Waited 3.0 s for memory before it could start
  detail: Other queries held the memory it needed. The run was slow before it did any work.
  fix: Find the queries with oversized grants (they show up with this same insight) and shrink them; adding memory only postpones it.
  verify: Run with an actual plan: GrantWaitTime is 0.
[parallel-skew] One thread of 14 handled 94% of the rows in Key Lookup
  detail: Parallel work is only as fast as its busiest thread; the others finished early and waited.
  fix: Uneven work usually means one key value holds most of the rows (check the distribution of the join or filter column) or a parallel scan over a small range; update statistics and consider filtering that value separately.
  verify: Run with actual rows: each thread handles a similar share.
== tests/insights/mssql-loops.sqlplan
[nested-loops] Repeats its read of customers 30K times, once per row of orders
  detail: The query forces a loop join, so the optimizer didn't weigh a hash or merge join.
  fix: The query forces a loop join (LOOP JOIN / OPTION (LOOP JOIN)); remove the hint and let the optimizer choose.
  verify: Explain again without the hint: a hash or merge join instead of the loop.
[missing-index-hint] SQL Server estimates an index on shop.orders (status) would make this 21% cheaper
  detail: The optimizer noticed it had to read shop.orders without a fitting index (33K rows read by Index Scan). Its estimate is a guide, not a promise: check the columns against the other queries on orders.
  fix: Create the index on status and include customer_id.
  sql: CREATE NONCLUSTERED INDEX [IX_orders_status]
           ON [shop].[orders] ([status])
           INCLUDE ([customer_id]);
       -- Add WITH (ONLINE = ON) on Enterprise or Azure SQL to keep the table available while it builds.
  verify: Explain again: the suggestion is gone and the read of orders becomes a seek.
[parallel-skew] One thread of 14 handled 64% of the rows in Index Seek
  detail: Parallel work is only as fast as its busiest thread; the others finished early and waited.
  fix: Uneven work usually means one key value holds most of the rows (check the distribution of the join or filter column) or a parallel scan over a small range; update statistics and consider filtering that value separately.
  verify: Run with actual rows: each thread handles a similar share.
== tests/insights/mssql-maxdop1.sqlplan
note: Estimated plan: misestimates, spills, unused memory grants and uneven threads need an actual plan.
[parallelism-blocked] This plan runs on one CPU because its statement says OPTION (MAXDOP 1)
  detail: Its estimated cost is 31. SQL Server considers a parallel plan above the server's cost threshold for parallelism (5 by default, often raised to 50), so whether it would split depends on that setting; the whole run happens on a single core instead.
  fix: Remove OPTION (MAXDOP 1) from the statement, unless something needs it to run on one CPU.
  verify: Explain again: no NonParallelPlanReason, and Parallelism operators appear.
== tests/insights/mssql-missing.sqlplan
note: Estimated plan: misestimates, spills, unused memory grants and uneven threads need an actual plan.
[missing-index-hint] SQL Server estimates an index on shop.orders (channel and shipped_at) would make this 75% cheaper
  detail: The optimizer noticed it had to read shop.orders without a fitting index (1.0M rows read by Clustered Index Scan). Its estimate is a guide, not a promise: check the columns against the other queries on orders.
  fix: Create the index on channel and shipped_at and include total.
  sql: CREATE NONCLUSTERED INDEX [IX_orders_channel_shipped_at]
           ON [shop].[orders] ([channel], [shipped_at])
           INCLUDE ([total]);
       -- Add WITH (ONLINE = ON) on Enterprise or Azure SQL to keep the table available while it builds.
  verify: Explain again: the suggestion is gone and the read of orders becomes a seek.
== tests/insights/mssql-nonsargable.sqlplan
note: Estimated plan: misestimates, spills, unused memory grants and uneven threads need an actual plan.
[non-sargable] DATEPART() around created_at keeps the database from using an index on orders
  detail: Each row's created_at is computed before it can be compared (`datepart(year,created_at) = (2026) AND datepart(month,created_at) = (3)`), so an index on created_at can't narrow the search: Clustered Index Scan reads 1.0M rows to keep 6%. 97% of this plan's work.
  fix: Where it preserves the original date, time-zone and NULL rules, compare created_at itself with a range, e.g. `created_at >= '2026-03-01' AND created_at < '2026-04-01'`. Test representative values: an index can then narrow that range when the optimizer finds it cheaper.
  verify: Compare identical results, including NULLs and boundary values, then obtain a fresh plan for orders with representative parameters. Compare rows read and execution time; an index condition on created_at can help, but the optimizer may correctly retain a scan.
== tests/insights/mssql-notin.sqlplan
note: Estimated plan: misestimates, spills, unused memory grants and uneven threads need an actual plan.
[not-in] NOT IN adds repeated subquery work; preserve NULL behavior when testing a rewrite
  detail: The plan includes repeated work and a Row Count Spool under Nested Loops. NULL handling can require extra checks for NOT IN, but this shape alone does not establish which constraints or values apply. A rewrite must preserve the query's NULL semantics.
  fix: First establish the intended NULL behavior: NOT IN and NOT EXISTS are not interchangeable when either comparison value can be NULL (an empty subquery is another boundary case). If both compared expressions are guaranteed NOT NULL and the subquery's conditions and correlations stay the same, test an equivalent NOT EXISTS rewrite. Otherwise preserve the original three-valued logic explicitly; do not remove NULL handling or add a NOT NULL constraint merely to change the plan.
  verify: Compare original and rewritten results with NULLs on either side, an empty subquery, duplicates and representative values. Then compare repeated subquery executions, reads and execution time in a fresh measured plan. An anti-join is one possible improvement, not a required operator.
[missing-index-hint] SQL Server estimates an index on shop.orders (shipped_at) would make this 26% cheaper
  detail: The optimizer noticed it had to read shop.orders without a fitting index (1.0M rows read by Clustered Index Scan). Its estimate is a guide, not a promise: check the columns against the other queries on orders.
  fix: Create the index on shipped_at.
  sql: CREATE NONCLUSTERED INDEX [IX_orders_shipped_at]
           ON [shop].[orders] ([shipped_at]);
       -- Add WITH (ONLINE = ON) on Enterprise or Azure SQL to keep the table available while it builds.
  verify: Explain again: the suggestion is gone and the read of orders becomes a seek.
== tests/insights/mssql-parallel.sqlplan
[missing-index-hint] SQL Server estimates an index on shop.order_items (order_id) would make this 61% cheaper
  detail: The optimizer noticed it had to read shop.order_items without a fitting index (1.5M rows read by Clustered Index Scan). Its estimate is a guide, not a promise: check the columns against the other queries on order_items.
  fix: Create the index on order_id.
  sql: CREATE NONCLUSTERED INDEX [IX_order_items_order_id]
           ON [shop].[order_items] ([order_id]);
       -- Add WITH (ONLINE = ON) on Enterprise or Azure SQL to keep the table available while it builds.
  verify: Explain again: the suggestion is gone and the read of order_items becomes a seek.
[parallel-skew] One thread of 4 handled 100% of the rows in Sort
  detail: Parallel work is only as fast as its busiest thread; the others finished early and waited.
  fix: Uneven work usually means one key value holds most of the rows (check the distribution of the join or filter column) or a parallel scan over a small range; update statistics and consider filtering that value separately.
  verify: Run with actual rows: each thread handles a similar share.
== tests/insights/mssql-sort-spill.sqlplan
[spill] Sort spilled 5.4 MB to tempdb
  detail: Sort used tempdb for part of this execution’s work. The spill alone does not establish its elapsed cost or underlying cause.
  fix: Compare input rows and width with estimates, then inspect requested, granted and used memory, applicable limits and concurrent demand. Reduce unnecessary input work where possible; check native memory grant feedback eligibility and status before relying on it. For this sort, test whether an access path can supply the required order.
  verify: Compare representative runs under equivalent load: spill warnings or volume, elapsed time, CPU and grant waiting. Fewer spills alone do not establish a better result; a larger grant can increase concurrency pressure.
[memory-grant] Waited 3.0 s for memory before it could start
  detail: Other queries held the memory it needed. The run was slow before it did any work.
  fix: Find the queries with oversized grants (they show up with this same insight) and shrink them; adding memory only postpones it.
  verify: Run with an actual plan: GrantWaitTime is 0.
[parallelism-blocked] This plan runs on one CPU because its statement says OPTION (MAXDOP 1)
  detail: Its estimated cost is 19. SQL Server considers a parallel plan above the server's cost threshold for parallelism (5 by default, often raised to 50), so whether it would split depends on that setting; the whole run happens on a single core instead.
  fix: Remove OPTION (MAXDOP 1) from the statement, unless something needs it to run on one CPU.
  verify: Explain again: no NonParallelPlanReason, and Parallelism operators appear.
== tests/insights/mssql-spool.sqlplan
note: Estimated plan: misestimates, spills, unused memory grants and uneven threads need an actual plan.
[eager-spool] SQL Server builds a temporary index on ticket_events every time this runs
  detail: The Index Spool copies 800K rows of ticket_events into a temporary index in tempdb on each run and drops it afterwards, because no index fits the lookup on event_type and created_at.
  fix: Create that index permanently, so the work happens once instead of on every run.
  sql: CREATE NONCLUSTERED INDEX [IX_ticket_events_event_type_created_at]
           ON [shop].[ticket_events] ([event_type], [created_at]);
       -- Add WITH (ONLINE = ON) on Enterprise or Azure SQL to keep the table available while it builds.
  verify: Explain again: the Index Spool is gone and the lookup seeks the new index.
== tests/insights/mssql-tablevar.sqlplan
note: Estimated plan: misestimates, spills, unused memory grants and uneven threads need an actual plan.
== tests/insights/mssql-udf.sqlplan
note: Estimated plan: misestimates, spills, unused memory grants and uneven threads need an actual plan.
[missing-index-hint] SQL Server estimates an index on shop.orders (total) would make this 84% cheaper
  detail: The optimizer noticed it had to read shop.orders without a fitting index (1.0M rows read by Clustered Index Scan). Its estimate is a guide, not a promise: check the columns against the other queries on orders.
  fix: Create the index on total and include created_at.
  sql: CREATE NONCLUSTERED INDEX [IX_orders_total]
           ON [shop].[orders] ([total])
           INCLUDE ([created_at]);
       -- Add WITH (ONLINE = ON) on Enterprise or Azure SQL to keep the table available while it builds.
  verify: Explain again: the suggestion is gone and the read of orders becomes a seek.
[scalar-udf] shop.fn_age_label runs once for every row (about 443K calls per run)
  detail: A scalar function runs like a small separate query for each row, and it keeps the whole plan on one CPU. Its cost doesn't show in the plan's numbers, which makes it easy to miss.
  fix: Inline it: rewrite the function as an inline table-valued function or a plain expression in the query. SQL Server 2019+ inlines scalar functions automatically unless they use things like GETDATE(), table variables or loops.
  verify: Explain again: no UserDefinedFunction in the plan, and no NonParallelPlanReason.
== tests/insights/mysql-analyze-join.txt
note: MySQL plans don't record spills, memory grants or parallel threads, so those checks don't apply.
== tests/insights/mysql-analyze-scan.txt
note: MySQL plans don't record spills, memory grants or parallel threads, so those checks don't apply.
[scan-filter] Reads 500K rows of orders to keep 0
  detail: The Table scan reads the rows and checks `(orders.promo_code = 'SPRING3')` on each one, so most of the reading is thrown away. 100% of this plan's work.
  fix: An index on promo_code lets the database go straight to the matching rows; if one already exists, the optimizer didn't use it, so refresh the table's statistics first.
  sql: CREATE INDEX `ix_orders_promo_code`
           ON `orders` (`promo_code`)
           ALGORITHM=INPLACE LOCK=NONE;
  verify: Explain again: orders reads about as many rows as it keeps.
[misestimate] Expected 50K rows from orders, got 0 (50K× fewer)
  detail: Nothing above it in this plan was sized from that estimate, but the same statistics decide every plan that reads this table.
  fix: Refresh the statistics on orders and add a histogram on promo_code.
  sql: ANALYZE TABLE `orders` UPDATE HISTOGRAM ON `promo_code`;
  verify: Run with actual rows again: the estimate is within about 10× of the actual count.
== tests/insights/mysql-cast.json
note: MySQL's EXPLAIN has no actual rows, spills, memory or threads; EXPLAIN ANALYZE adds actual rows.
== tests/insights/mysql-dependent.json
note: MySQL's EXPLAIN has no actual rows, spills, memory or threads; EXPLAIN ANALYZE adds actual rows.
[correlated-subquery] A subquery runs 199K times, once per row of c
  detail: MySQL marks it DEPENDENT and not cacheable, so it re-runs the subquery for every row instead of once for the whole set, reading o each time.
  fix: Rewrite it as a join (or LEFT JOIN … GROUP BY for counts and sums) so it runs once; if it must stay correlated, index the columns it filters on so each run is a quick lookup.
  verify: Explain again: the subquery appears once, joined, instead of as a per-row SubPlan / DEPENDENT SUBQUERY.
== tests/insights/mysql-filesort-temp.json
note: MySQL's EXPLAIN has no actual rows, spills, memory or threads; EXPLAIN ANALYZE adds actual rows.
[temp-table-sort] Sorts the groups in a temporary table (filesort)
  detail: Grouping already reads an index in order; MySQL then sorts the grouped result, because the query orders by an aggregate or by columns the index doesn't provide.
  fix: Ordering by an aggregate (COUNT(*) DESC) always needs this sort: keep the grouped set small with a selective WHERE, or add a LIMIT so only the top groups are kept.
  verify: Explain again: the sort's input is the groups you expect, not every row.
== tests/insights/mysql-function.json
note: MySQL's EXPLAIN has no actual rows, spills, memory or threads; EXPLAIN ANALYZE adds actual rows.
== tests/insights/mysql-join-buffer.json
note: MySQL's EXPLAIN has no actual rows, spills, memory or threads; EXPLAIN ANALYZE adds actual rows.
[join-buffer] Joins i without an index (hash join over a full read)
  detail: MySQL has no index to look up matching rows of i for each row it joins, so it reads all 798K rows into a join buffer and matches them there.
  fix: Index i on price so MySQL can look rows up directly.
  sql: CREATE INDEX `ix_i_price`
           ON `i` (`price`)
           ALGORITHM=INPLACE LOCK=NONE;
  verify: Explain again: i is read with ref or eq_ref access, without a join buffer.
== tests/insights/mysql-like.json
note: MySQL's EXPLAIN has no actual rows, spills, memory or threads; EXPLAIN ANALYZE adds actual rows.
== tests/insights/mysql-notin.json
note: MySQL's EXPLAIN has no actual rows, spills, memory or threads; EXPLAIN ANALYZE adds actual rows.
== tests/insights/mysql-scan-filter.json
note: MySQL's EXPLAIN has no actual rows, spills, memory or threads; EXPLAIN ANALYZE adds actual rows.
== tests/insights/pg-cross.json
note: Without EXPLAIN ANALYZE there are no actual rows, spills or timings to check. PostgreSQL plans never carry index suggestions; Culprit reads filters instead.
[no-join-predicate] customers and products are joined without a condition: every row with every row
  detail: Nested Loop has no join condition, so its output is the product of both inputs (2.0B rows planned). That's almost always a missing ON clause.
  fix: Add the missing join condition; if you really want every combination, write CROSS JOIN so the intent is clear.
  verify: Explain again: the join has a condition and its row count is reasonable.
== tests/insights/pg-function.json
note: Without EXPLAIN ANALYZE there are no actual rows, spills or timings to check. PostgreSQL plans never carry index suggestions; Culprit reads filters instead.
== tests/insights/pg-hash-batches.json
note: PostgreSQL plans carry no index suggestions or memory grants; index advice comes from the filters.
[spill] Aggregate reported 7.4 MB for temporary work
  detail: This operator recorded temporary work under its native memory policy. Its volume or batch count alone does not establish elapsed cost or a suitable work_mem value.
  fix: Compare actual input rows, row width and operator memory use with work_mem and, for hash operations, hash_mem_multiplier. Reduce unnecessary input work where possible; test any session or role memory change under representative concurrency before choosing a value.
  verify: Compare representative executions under equivalent load: temporary-file use or batches, elapsed time, CPU and waiting. A larger work_mem can multiply memory demand across operators, workers and concurrent executions; fewer spills alone do not establish a better result.
[spill] The sort used 2.8 MB of temporary disk space
  detail: This operator recorded temporary work under its native memory policy. Its volume or batch count alone does not establish elapsed cost or a suitable work_mem value.
  fix: Compare actual input rows, row width and operator memory use with work_mem and, for hash operations, hash_mem_multiplier. Reduce unnecessary input work where possible; test any session or role memory change under representative concurrency before choosing a value. For this sort, test whether an access path can supply the required order.
  verify: Compare representative executions under equivalent load: temporary-file use or batches, elapsed time, CPU and waiting. A larger work_mem can multiply memory demand across operators, workers and concurrent executions; fewer spills alone do not establish a better result.
[spill] The hash processed its input in 32 batches
  detail: This operator recorded temporary work under its native memory policy. Its volume or batch count alone does not establish elapsed cost or a suitable work_mem value.
  fix: Compare actual input rows, row width and operator memory use with work_mem and, for hash operations, hash_mem_multiplier. Reduce unnecessary input work where possible; test any session or role memory change under representative concurrency before choosing a value.
  verify: Compare representative executions under equivalent load: temporary-file use or batches, elapsed time, CPU and waiting. A larger work_mem can multiply memory demand across operators, workers and concurrent executions; fewer spills alone do not establish a better result.
== tests/insights/pg-heap-fetches.json
note: PostgreSQL plans carry no index suggestions or memory grants; index advice comes from the filters.
[heap-fetches] The index-only scan on events still visits the table 8.0K times
  detail: PostgreSQL must visit table pages that the visibility map cannot certify as all-visible. Recent writes, an older snapshot or maintenance that has not yet caught up can explain this; heap fetches alone do not establish a vacuum failure.
  fix: Check recent writes, vacuum activity and cleanup holders on events. If vacuum can advance visibility and the saved reads justify it, test VACUUM; a frequently updated table can legitimately keep requiring heap fetches.
  sql: VACUUM (ANALYZE) public.events;
  verify: Compare heap fetches, buffers and execution time with equivalent parameters and write activity after maintenance. Fewer heap fetches may help; zero is not required on a table that keeps changing.
== tests/insights/pg-index-only.json
note: PostgreSQL plans carry no index suggestions or memory grants; index advice comes from the filters.
== tests/insights/pg-like.json
note: Without EXPLAIN ANALYZE there are no actual rows, spills or timings to check. PostgreSQL plans never carry index suggestions; Culprit reads filters instead.
== tests/insights/pg-loops.json
note: PostgreSQL plans carry no index suggestions or memory grants; index advice comes from the filters.
== tests/insights/pg-misestimate.json
note: PostgreSQL plans carry no index suggestions or memory grants; index advice comes from the filters.
[misestimate] Expected 438 rows from customers, got 50K (114× more)
  detail: An operator above it also recorded a spill. The row-estimate mismatch can affect planning, but these observations alone do not establish the spill’s cause or elapsed cost. The filter compares an expression, not a plain column, so the column's statistics can't predict it.
  fix: Analyze customers; on PostgreSQL 14+ statistics on the expression itself help (CREATE STATISTICS … ON (expression) FROM table).
  sql: ANALYZE public.customers;
  verify: Run with actual rows again: the estimate is within about 10× of the actual count.
[spill] The sort used 4.3 MB of temporary disk space
  detail: This operator recorded temporary work under its native memory policy. Its volume or batch count alone does not establish elapsed cost or a suitable work_mem value. Actual rows were also substantially underestimated in this subtree; that mismatch alone does not establish why temporary work occurred.
  fix: Compare actual input rows, row width and operator memory use with work_mem and, for hash operations, hash_mem_multiplier. Reduce unnecessary input work where possible; test any session or role memory change under representative concurrency before choosing a value. For this sort, test whether an access path can supply the required order.
  verify: Compare representative executions under equivalent load: temporary-file use or batches, elapsed time, CPU and waiting. A larger work_mem can multiply memory demand across operators, workers and concurrent executions; fewer spills alone do not establish a better result.
== tests/insights/pg-notin.json
note: Without EXPLAIN ANALYZE there are no actual rows, spills or timings to check. PostgreSQL plans never carry index suggestions; Culprit reads filters instead.
[not-in] NOT IN tests rows against a repeatedly searched subquery
  detail: The plan searches the subquery repeatedly instead of using a hashed membership test. Cost, correlation and NULL semantics can affect that choice; the plan alone does not establish that a larger work_mem or a rewrite would improve it. A NULL in a nonempty subquery prevents a nonmatching value from passing NOT IN.
  fix: First establish the intended NULL behavior: NOT IN and NOT EXISTS are not interchangeable when either comparison value can be NULL (an empty subquery is another boundary case). If both compared expressions are guaranteed NOT NULL and the subquery's conditions and correlations stay the same, test an equivalent NOT EXISTS rewrite. Otherwise preserve the original three-valued logic explicitly; do not remove NULL handling or add a NOT NULL constraint merely to change the plan.
  verify: Compare original and rewritten results with NULLs on either side, an empty subquery, duplicates and representative values. Then compare repeated subquery executions, reads and execution time in a fresh measured plan. An anti-join is one possible improvement, not a required operator.
== tests/insights/pg-seqscan-filter.json
note: PostgreSQL plans carry no index suggestions or memory grants; index advice comes from the filters.
[scan-filter] Reads 2.0M rows of orders to keep 14
  detail: The Seq Scan reads the rows and checks `(customer_id = 42)` on each one, so most of the reading is thrown away. 98% of this plan's work.
  fix: An index on customer_id lets the database go straight to the matching rows; if one already exists, the optimizer didn't use it, so refresh the table's statistics first.
  sql: CREATE INDEX CONCURRENTLY ix_orders_customer_id
           ON orders (customer_id);
  verify: Explain again: orders reads about as many rows as it keeps.
== tests/insights/pg-sort-spill.json
note: PostgreSQL plans carry no index suggestions or memory grants; index advice comes from the filters.
[spill] The sort used 72.0 MB of temporary disk space
  detail: This operator recorded temporary work under its native memory policy. Its volume or batch count alone does not establish elapsed cost or a suitable work_mem value.
  fix: Compare actual input rows, row width and operator memory use with work_mem and, for hash operations, hash_mem_multiplier. Reduce unnecessary input work where possible; test any session or role memory change under representative concurrency before choosing a value. For this sort, test whether an access path can supply the required order.
  verify: Compare representative executions under equivalent load: temporary-file use or batches, elapsed time, CPU and waiting. A larger work_mem can multiply memory demand across operators, workers and concurrent executions; fewer spills alone do not establish a better result.
== tests/plans/mariadb-index.json
note: MariaDB's EXPLAIN has no actual rows or timings; ANALYZE adds them, with pages read and sort spills.
== tests/plans/mariadb-scan.json
note: MariaDB's EXPLAIN has no actual rows or timings; ANALYZE adds them, with pages read and sort spills.
== tests/plans/mssql-actual.sqlplan
== tests/plans/mssql-scan.sqlplan
note: Estimated plan: misestimates, spills, unused memory grants and uneven threads need an actual plan.
[implicit-conversion] A type mismatch on customers.email stops SQL Server from seeking an index
  detail: The value it's compared with has a different type, so SQL Server converts customers.email to nvarchar(200) on every row (`CONVERT_IMPLICIT(nvarchar(200),email,0) = @email`) and, by its own warning, can't seek the index.
  fix: Make the types match: send the value as non-Unicode text (SqlDbType.VarChar in .NET, sql.VarChar in Node's mssql, a non-Unicode mapping in your ORM), or, if your schema keeps text as Unicode, change customers.email to nvarchar(200). Changing a column rewrites it and the indexes on it.
  verify: Explain again: no CONVERT_IMPLICIT in the plan, and the scan became a seek.
== tests/plans/mssql-seek.sqlplan
note: Estimated plan: misestimates, spills, unused memory grants and uneven threads need an actual plan.
== tests/plans/mysql-analyze.txt
note: MySQL plans don't record spills, memory grants or parallel threads, so those checks don't apply.
== tests/plans/mysql-index.json
note: MySQL's EXPLAIN has no actual rows, spills, memory or threads; EXPLAIN ANALYZE adds actual rows.
== tests/plans/mysql-scan.json
note: MySQL's EXPLAIN has no actual rows, spills, memory or threads; EXPLAIN ANALYZE adds actual rows.
== tests/plans/mysql-tree-g.txt
note: MySQL's EXPLAIN has no actual rows, spills, memory or threads; EXPLAIN ANALYZE adds actual rows.
== tests/plans/mysql-v2.json
note: MySQL's EXPLAIN has no actual rows, spills, memory or threads; EXPLAIN ANALYZE adds actual rows.
== tests/plans/pg-analyze.txt
note: PostgreSQL plans carry no index suggestions or memory grants; index advice comes from the filters.
== tests/plans/pg-index.json
note: Without EXPLAIN ANALYZE there are no actual rows, spills or timings to check. PostgreSQL plans never carry index suggestions; Culprit reads filters instead.
== tests/plans/pg-seqscan.json
note: Without EXPLAIN ANALYZE there are no actual rows, spills or timings to check. PostgreSQL plans never carry index suggestions; Culprit reads filters instead.
== tests/plans/pg-seqscan.txt
note: Without EXPLAIN ANALYZE there are no actual rows, spills or timings to check. PostgreSQL plans never carry index suggestions; Culprit reads filters instead.
== tests/plans/pg-subplans.txt
note: PostgreSQL plans carry no index suggestions or memory grants; index advice comes from the filters.
== tests/plans/sqlite-index.txt
note: SQLite's EXPLAIN QUERY PLAN shows the access path only: no estimated or actual rows, costs or timings.
== tests/plans/sqlite-scan.txt
note: SQLite's EXPLAIN QUERY PLAN shows the access path only: no estimated or actual rows, costs or timings.
== tests/plans/timescale-index.json
note: PostgreSQL plans carry no index suggestions or memory grants; index advice comes from the filters.
== tests/plans/timescale-scan.json
note: PostgreSQL plans carry no index suggestions or memory grants; index advice comes from the filters.
== tests/plans/yugabyte-index.json
note: DocDB storage RPCs and rows scanned are distinct from PostgreSQL buffer pages. RPC timings may overlap and are not exclusive shares of execution time.
== tests/plans/yugabyte-scan.json
note: DocDB storage RPCs and rows scanned are distinct from PostgreSQL buffer pages. RPC timings may overlap and are not exclusive shares of execution time.
"##;
