//! What the index advisor, the dashboard and drift say, word for word, in cases the demo sidecars (which the engine's
//! invariants compare) don't reach. Their words come from the language catalog (locales/en.json); these tests hold the
//! English to what the views said when they wrote it in code.

use culprit_checks::{analyze, index_advice};
use culprit_core::{IndexAdvice, IndexStat, MissingIndex, PlanNode, Snapshot};

fn snapshot(name: &str) -> Snapshot {
    let text =
        std::fs::read_to_string(format!("{}/../../fixtures/{name}", env!("CARGO_MANIFEST_DIR"))).expect("fixture");
    serde_json::from_str(&text).expect("snapshot")
}

/// Counters ten days old: absence and rates a day are said.
fn ten_days(mut s: Snapshot) -> Snapshot {
    s.server.started_at = Some("2026-10-01T12:00:00Z".into());
    s.taken_at = "2026-10-11T12:00:00Z".into();
    if s.server.engine == culprit_core::Engine::Postgres {
        s.pressure.get_or_insert_with(Default::default).uptime_hours = Some(240.0);
    }
    crate::counter_age::digests_counted_since_the_start(&mut s);
    s
}

#[track_caller]
fn same(actual: &str, expected: &str) {
    assert!(actual == expected, "said:\n{actual}\n\nwhere the English was:\n{expected}");
}

fn names(v: &[&str]) -> Vec<String> {
    v.iter().map(|c| c.to_string()).collect()
}

/// Everything the advisor says, one line a part, for the tables `only` names (every table when empty).
fn advice_words(a: &IndexAdvice, only: &[&str]) -> String {
    let shown = |table: &str| only.is_empty() || only.contains(&table);
    let mut out = vec![format!("counters: {}", a.counters)];
    for c in a.candidates.iter().filter(|c| shown(&c.table)) {
        out.push(format!("candidate {}({}) {:?}: {}", c.table, c.keys.join(","), c.source, c.reason));
        if let Some(cost) = &c.write_cost {
            out.push(format!("  write cost: {cost}"));
        }
        out.extend(c.verify.iter().map(|v| format!("  verify: {v}")));
    }
    for c in a.cleanups.iter().filter(|c| shown(&c.table)) {
        out.push(format!("cleanup {} {:?}: {}", c.index, c.kind, c.reason));
        out.extend(c.caveats.iter().map(|v| format!("  caveat: {v}")));
    }
    for t in a.tables.iter().filter(|t| shown(&t.table)) {
        out.push(format!("table {} {:?}: {}", t.table, t.kind, t.reason));
    }
    out.extend(a.notes.iter().map(|n| format!("note: {n}")));
    out.join("\n")
}

/// The SQL Server demo where the optimizer asked for an index on support_tickets for one query (or `queries` of them),
/// which compares is_deleted with `comparison`; `filtered` gives the table an index on those keys for the rows where
/// is_deleted = 0.
fn asked(
    equality: &[&str],
    inequality: &[&str],
    include: &[&str],
    comparison: &str,
    filtered: Option<&[&str]>,
    queries: usize,
) -> Snapshot {
    let mut s = ten_days(snapshot("mssql-demo.json"));
    let hash = "0x00C0FFEE00C0FFEE";
    s.missing_indexes.push(MissingIndex {
        schema: "shop".into(),
        table: "support_tickets".into(),
        equality: names(equality),
        inequality: names(inequality),
        include: names(include),
        seeks: 400.0,
        scans: 0.0,
        avg_query_cost: 0.5,
        avg_impact_pct: 95.0,
        last_used: None,
        query_hashes: vec![hash.into()],
    });
    let w = s.workload.as_mut().expect("workload");
    let template = w.queries.iter().find(|q| q.plan.is_none()).cloned().unwrap_or_else(|| w.queries[0].clone());
    for n in 0..queries {
        let mut q = template.clone();
        q.id = format!("424{n}");
        q.query_hash = Some(hash.into());
        q.plan = None;
        q.text = format!(
            "(@par0 int,@par1 bit)select [id], [status] from [shop].[support_tickets] where (([customer_id] = @par0) and ({comparison}))"
        );
        w.queries.push(q);
    }
    if let Some(keys) = filtered {
        let mut i = s.indexes.iter().find(|i| i.name == "ix_support_tickets_customer_id").expect("an index").clone();
        i.name = "ix_support_tickets_open".into();
        i.columns = names(keys);
        i.filter = Some("([is_deleted]=(0))".into());
        s.indexes.push(i);
    }
    s
}

#[test]
fn optimizer_requests_say_what_they_said() {
    let said = |s: Snapshot| advice_words(&index_advice(&analyze(s)), &["support_tickets"]);
    let mut out = Vec::new();
    // Passed as a parameter, the filtered index finds the rows by part of the request: the rest is an index.
    let partly = |queries| {
        asked(
            &["customer_id", "priority", "is_deleted"],
            &["due_at"],
            &[],
            "[is_deleted] = @par1",
            Some(&["priority", "customer_id"]),
            queries,
        )
    };
    out.push(said(partly(1)));
    out.push(said(partly(2)));
    // Passed as a parameter, the filtered index serves the lookups but not the columns: the rest is included columns.
    let covering = |include: &[&str], queries| {
        asked(
            &["customer_id", "is_deleted"],
            &[],
            include,
            "[is_deleted] = @par1",
            Some(&["customer_id", "is_deleted"]),
            queries,
        )
    };
    out.push(said(covering(&["status"], 1)));
    out.push(said(covering(&["status", "priority"], 2)));
    // More included columns than a statement keeps.
    let wide = ["c1", "c2", "c3", "c4", "c5", "c6", "c7", "c8"];
    out.push(said(asked(&["customer_id"], &[], &wide, "[priority] = @par1", None, 1)));
    // No query in the scan asked.
    let mut s = asked(&["customer_id", "is_deleted"], &[], &[], "[is_deleted] = 0", None, 1);
    s.workload.as_mut().unwrap().queries.retain(|q| q.id != "4240");
    out.push(said(s));
    same(&out.join("\n---\n"), OPTIMIZER);
}

const OPTIMIZER: &str = r#"counters: in the 10 days since the server last restarted
candidate support_tickets(priority,customer_id,due_at) Optimizer: SQL Server's optimizer asked for it 400 times in the 10 days since the server last restarted and estimates those lookups would be about 95% cheaper. The query behind it accounts for 54% of database time. The query passes is_deleted as a parameter, which keeps SQL Server off ix_support_tickets_open, filtered to is_deleted = 0; written as a literal, it finds the rows by priority and customer_id, not by due_at. This index is the rest of the request, filtered the same way.
  write cost: shop.support_tickets received no writes in the 10 days since the server last restarted, so this index would cost next to nothing to maintain.
  verify: Write is_deleted = 0 as a literal in the queries first, then re-scan: the request either disappears or asks for this.
  verify: The queries listed seek on the new index and read far fewer pages per run.
cleanup ix_support_tickets_customer_id Unused: Not read once in the 10 days since the server last restarted, yet updated with every write to shop.support_tickets.
  caveat: It supports the foreign key fk_support_tickets_customer; deletes on the parent scan without it.
cleanup ix_support_tickets_open Unused: Not read once in the 10 days since the server last restarted, yet updated with every write to shop.support_tickets.
---
counters: in the 10 days since the server last restarted
candidate support_tickets(priority,customer_id,due_at) Optimizer: SQL Server's optimizer asked for it 400 times in the 10 days since the server last restarted and estimates those lookups would be about 95% cheaper. The 2 queries behind it account for 100% of database time. The queries pass is_deleted as a parameter, which keeps SQL Server off ix_support_tickets_open, filtered to is_deleted = 0; written as a literal, it finds the rows by priority and customer_id, not by due_at. This index is the rest of the request, filtered the same way.
  write cost: shop.support_tickets received no writes in the 10 days since the server last restarted, so this index would cost next to nothing to maintain.
  verify: Write is_deleted = 0 as a literal in the queries first, then re-scan: the request either disappears or asks for this.
  verify: The queries listed seek on the new index and read far fewer pages per run.
cleanup ix_support_tickets_customer_id Unused: Not read once in the 10 days since the server last restarted, yet updated with every write to shop.support_tickets.
  caveat: It supports the foreign key fk_support_tickets_customer; deletes on the parent scan without it.
cleanup ix_support_tickets_open Unused: Not read once in the 10 days since the server last restarted, yet updated with every write to shop.support_tickets.
---
counters: in the 10 days since the server last restarted
candidate support_tickets(customer_id,is_deleted) Optimizer: SQL Server's optimizer asked for it 400 times in the 10 days since the server last restarted and estimates those lookups would be about 95% cheaper. The query behind it accounts for 54% of database time. The query passes is_deleted as a parameter, which keeps SQL Server off ix_support_tickets_open, filtered to is_deleted = 0; written as a literal, it serves them, reading status from the table for each row. Including it is the rest of the request.
  write cost: shop.support_tickets received no writes in the 10 days since the server last restarted, so this index would cost next to nothing to maintain.
  verify: Write is_deleted = 0 as a literal in the queries first, then re-scan: the request either disappears or asks for this.
  verify: The queries listed seek on ix_support_tickets_open and read far fewer pages per run.
cleanup ix_support_tickets_customer_id Unused: Not read once in the 10 days since the server last restarted, yet updated with every write to shop.support_tickets.
  caveat: It supports the foreign key fk_support_tickets_customer; deletes on the parent scan without it.
cleanup ix_support_tickets_open Unused: Not read once in the 10 days since the server last restarted, yet updated with every write to shop.support_tickets.
  caveat: It supports the foreign key fk_support_tickets_customer; deletes on the parent scan without it.
---
counters: in the 10 days since the server last restarted
candidate support_tickets(customer_id,is_deleted) Optimizer: SQL Server's optimizer asked for it 400 times in the 10 days since the server last restarted and estimates those lookups would be about 95% cheaper. The 2 queries behind it account for 100% of database time. The queries pass is_deleted as a parameter, which keeps SQL Server off ix_support_tickets_open, filtered to is_deleted = 0; written as a literal, it serves them, reading status and priority from the table for each row. Including them is the rest of the request.
  write cost: shop.support_tickets received no writes in the 10 days since the server last restarted, so this index would cost next to nothing to maintain.
  verify: Write is_deleted = 0 as a literal in the queries first, then re-scan: the request either disappears or asks for this.
  verify: The queries listed seek on ix_support_tickets_open and read far fewer pages per run.
cleanup ix_support_tickets_customer_id Unused: Not read once in the 10 days since the server last restarted, yet updated with every write to shop.support_tickets.
  caveat: It supports the foreign key fk_support_tickets_customer; deletes on the parent scan without it.
cleanup ix_support_tickets_open Unused: Not read once in the 10 days since the server last restarted, yet updated with every write to shop.support_tickets.
  caveat: It supports the foreign key fk_support_tickets_customer; deletes on the parent scan without it.
---
counters: in the 10 days since the server last restarted
candidate support_tickets(customer_id) Optimizer: SQL Server's optimizer asked for it 400 times in the 10 days since the server last restarted and estimates those lookups would be about 95% cheaper. The query behind it accounts for 54% of database time.
  verify: Obtain a newly compiled plan after representative runs; an old stored request need not disappear.
  verify: Check whether the queries listed use ix_support_tickets_customer_id and compare reads and time per run under equivalent parameters and load.
  verify: SQL Server also wanted 8 more columns included; the statement keeps the keys only, so add INCLUDE columns just for what the hot queries select.
cleanup ix_support_tickets_customer_id Unused: Not read once in the 10 days since the server last restarted, yet updated with every write to shop.support_tickets.
  caveat: It supports the foreign key fk_support_tickets_customer; deletes on the parent scan without it.
---
counters: in the 10 days since the server last restarted
cleanup ix_support_tickets_customer_id Unused: Not read once in the 10 days since the server last restarted, yet updated with every write to shop.support_tickets.
  caveat: It supports the foreign key fk_support_tickets_customer; deletes on the parent scan without it.
note: Left out 1 candidate without sufficient current measured workload, or for a table that is gone or queries that are history."#;

/// An index like `template` on `table`, named `name` on `columns`, read `reads` times.
fn index_like(template: &IndexStat, name: &str, columns: &[&str], reads: f64) -> IndexStat {
    let mut i = template.clone();
    i.name = name.into();
    i.columns = names(columns);
    i.include.clear();
    i.filter = None;
    (i.is_primary_key, i.is_unique, i.is_clustered, i.is_disabled) = (false, false, false, false);
    i.reads = Some(reads);
    i.writes = Some(5_000_000.0);
    i.size_bytes = Some(64.0 * 1024.0 * 1024.0);
    i.created_at_latest = None;
    i.partition_of = None;
    i.state = None;
    i
}

#[test]
fn cleanups_and_tables_say_what_they_said() {
    let mut out = Vec::new();
    for name in ["mssql-demo.json", "postgres-demo.json"] {
        let mut s = ten_days(snapshot(name));
        let (schema, table) = {
            let t = s.tables.iter().max_by(|a, b| a.rows.unwrap_or(0.0).total_cmp(&b.rows.unwrap_or(0.0))).unwrap();
            (t.schema.clone(), t.name.clone())
        };
        let template = s.indexes.iter().find(|i| i.schema == schema && i.table == table).cloned().expect("an index");
        // An index read beside one that holds its keys, two indexes nothing reads with the same keys, one that
        // nothing reads but writes maintain, and many more: overlapping, duplicates, unused and over-indexed.
        s.indexes.push(index_like(&template, "ix_busy", &["zz_tenant", "zz_created", "zz_status"], 900_000.0));
        s.indexes.push(index_like(&template, "ix_rare", &["zz_tenant", "zz_status"], 12.0));
        s.indexes.push(index_like(&template, "ix_twin_a", &["zz_twin"], 0.0));
        s.indexes.push(index_like(&template, "ix_twin_b", &["zz_twin"], 0.0));
        s.indexes.push(index_like(&template, "ix_prefix", &["zz_pre"], 0.0));
        s.indexes.push(index_like(&template, "ix_prefix_long", &["zz_pre", "zz_more"], 0.0));
        let mut quiet = index_like(&template, "ix_quiet", &["zz_quiet"], 0.0);
        quiet.writes = Some(0.0);
        s.indexes.push(quiet);
        for n in 0..7 {
            s.indexes.push(index_like(&template, &format!("ix_extra_{n}"), &[&format!("zz_col_{n}")], 5000.0));
        }
        // A PostgreSQL index left invalid by a build that didn't finish, maintained or not.
        let mut invalid = index_like(&template, "ix_invalid", &["zz_invalid"], 0.0);
        (invalid.is_disabled, invalid.maintained) = (true, Some(true));
        s.indexes.push(invalid.clone());
        (invalid.name, invalid.columns, invalid.maintained) =
            ("ix_invalid_idle".into(), names(&["zz_idle"]), Some(false));
        s.indexes.push(invalid);
        // Partitions' copies of an index.
        for n in 0..2 {
            let mut copy = index_like(&template, &format!("ix_part_{n}"), &["zz_part"], 0.0);
            copy.partition_of = Some(format!("{schema}.ix_parent"));
            s.indexes.push(copy);
        }
        let t = s.tables.iter_mut().find(|t| t.schema == schema && t.name == table).unwrap();
        (t.inserts, t.updates, t.deletes) = (Some(9_000_000.0), Some(1_000_000.0), Some(0.0));
        for i in s
            .indexes
            .iter_mut()
            .filter(|i| i.schema == schema && i.table == table && (i.is_clustered || i.is_primary_key))
        {
            i.writes = Some(10_000_000.0);
        }
        // A heap whose reads follow forwarding pointers.
        if let Some(t) = s.tables.iter_mut().find(|t| !(t.schema == schema && t.name == table)) {
            (t.is_heap, t.rows, t.forwarded_fetches) = (Some(true), Some(500_000.0), Some(3_000_000.0));
        }
        out.push(advice_words(&index_advice(&analyze(s)), &[]));
    }
    same(&out.join("\n---\n"), CLEANUPS);
}

const CLEANUPS: &str = r#"counters: in the 10 days since the server last restarted
candidate order_items(order_id) Optimizer: SQL Server's optimizer asked for it 300 times in the 10 days since the server last restarted and estimates those lookups would be about 99% cheaper. The query behind it accounts for 17% of database time.
  write cost: shop.order_items receives about 1.0M writes a day; each one will also update this index.
  verify: Obtain a newly compiled plan after representative runs; an old stored request need not disappear.
  verify: Check whether the queries listed use the new index and compare reads and time per run under equivalent parameters and load.
candidate orders(shipped_at) Optimizer: SQL Server's optimizer asked for it 150 times in the 10 days since the server last restarted and estimates those lookups would be about 81% cheaper. The query behind it accounts for 12% of database time.
  write cost: shop.orders received no writes in the 10 days since the server last restarted, so this index would cost next to nothing to maintain.
  verify: Obtain a newly compiled plan after representative runs; an old stored request need not disappear.
  verify: Check whether the queries listed use the new index and compare reads and time per run under equivalent parameters and load.
candidate ticket_events(ticket_id) ForeignKey: ticket_id is the foreign key to support_tickets. Without an index, deleting or re-keying a row in support_tickets reads all 800K rows of shop.ticket_events looking for matches, and joins from support_tickets can't seek.
  write cost: shop.ticket_events received no writes in the 10 days since the server last restarted, so this index would cost next to nothing to maintain.
  verify: Deletes from support_tickets no longer scan ticket_events.
cleanup ix_twin_b Unused: Not read once in the 10 days since the server last restarted, and it has the same keys as ix_twin_a (which isn't read either).
cleanup ix_prefix Unused: Not read once in the 10 days since the server last restarted, and its keys are the start of ix_prefix_long (which isn't read either).
cleanup ix_rare Overlap: Read 12 times next to ix_busy's 900K; both start with zz_tenant, so its queries can almost always seek on ix_busy instead.
  caveat: Check the plans of the queries that read ix_rare before hiding it.
cleanup ix_twin_a Unused: Not read once in the 10 days since the server last restarted while maintained 5.0M times, yet updated with every write to shop.order_items.
cleanup ix_prefix_long Unused: Not read once in the 10 days since the server last restarted while maintained 5.0M times, yet updated with every write to shop.order_items.
cleanup ix_quiet Unused: Not read once in the 10 days since the server last restarted, yet updated with every write to shop.order_items.
cleanup ix_orders_status_created Unused: Not read once in the 10 days since the server last restarted, yet updated with every write to shop.orders.
cleanup ix_orders_status Unused: Not read once in the 10 days since the server last restarted, and its keys are the start of ix_orders_status_created (which isn't read either).
cleanup ix_order_items_product_id Unused: Not read once in the 10 days since the server last restarted, yet updated with every write to shop.order_items.
  caveat: It supports the foreign key fk_order_items_product; deletes on the parent scan without it.
cleanup ix_support_tickets_customer_id Unused: Not read once in the 10 days since the server last restarted, yet updated with every write to shop.support_tickets.
  caveat: It supports the foreign key fk_support_tickets_customer; deletes on the parent scan without it.
cleanup ix_products_sku Duplicate: Same keys as uq_products_sku: the same index twice, both maintained on every write.
table customers ForwardedFetches: A heap: reads followed a forwarding pointer to another page 3.0M times in the 10 days since the server last restarted. A clustered index ends that.
table order_items OverIndexed: 18 secondary indexes and about 1.0M write statements a day: every insert updates all of them.
note: 2 indexes on partitions are left out of the clean-ups: each is a partition's copy of its partitioned table's index, a cold partition reads as unused because queries skip it, and a copy can't be dropped on its own.
---
counters: in the 10 days since the server last restarted
candidate orders(customer_id) PlanScan: One captured query plan scans public.orders and keeps only those matching (orders.customer_id = $1); no enabled index starts with customer_id. The captured scan uses 2.0M rows as its size basis; this scan's catalog reports 2.0M rows in the table. Captured estimates can predate the current table size.
  write cost: public.orders receives about 1.0M writes a day; each one will also update this index.
  verify: Obtain a newly compiled plan and check whether orders uses the candidate index to read fewer rows; a scan can still be appropriate for some parameters.
  verify: Compare reads and time per run after equivalent workload, including storage and write cost. Old saved plans and a changing share of database time alone do not verify improvement.
candidate order_items(product_id) PlanScan: One captured query plan scans public.order_items and keeps only those matching (order_items.product_id = $1); no enabled index starts with product_id. The captured scan uses 1.0M rows as its size basis; this scan's catalog reports 1.0M rows in the table. Captured estimates can predate the current table size.
  write cost: public.order_items received no writes in the 10 days since the server last restarted, so this index would cost next to nothing to maintain.
  verify: Obtain a newly compiled plan and check whether order_items uses the candidate index to read fewer rows; a scan can still be appropriate for some parameters.
  verify: Compare reads and time per run after equivalent workload, including storage and write cost. Old saved plans and a changing share of database time alone do not verify improvement.
candidate order_items(order_id) ForeignKey: order_id is the foreign key to orders. Without an index, deleting or re-keying a row in orders reads all 1.0M rows of public.order_items looking for matches, and joins from orders can't seek. public.orders has had no deletes in the 10 days since the server last restarted, though: the database looks for matching rows only when a parent row is deleted or its key changes, so the index would mostly serve joins.
  write cost: public.order_items received no writes in the 10 days since the server last restarted, so this index would cost next to nothing to maintain.
  verify: Deletes from orders no longer scan order_items.
cleanup ix_twin_b Unused: Not read once in the 10 days since the server last restarted, and it has the same keys as ix_twin_a (which isn't read either).
cleanup ix_prefix Unused: Not read once in the 10 days since the server last restarted, and its keys are the start of ix_prefix_long (which isn't read either).
cleanup ix_rare Overlap: Read 12 times next to ix_busy's 900K; both start with zz_tenant, so its queries can almost always seek on ix_busy instead.
  caveat: Check the plans of the queries that read ix_rare before hiding it.
cleanup ix_twin_a Unused: Not read once in the 10 days since the server last restarted while maintained 5.0M times, yet updated with every write to public.orders.
cleanup ix_prefix_long Unused: Not read once in the 10 days since the server last restarted while maintained 5.0M times, yet updated with every write to public.orders.
cleanup ix_quiet Unused: Not read once in the 10 days since the server last restarted, yet updated with every write to public.orders.
cleanup ix_invalid Unused: Marked invalid after an unfinished concurrent build; the planner never reads it, yet every write to public.orders still updates it.
cleanup ix_invalid_idle Unused: Marked invalid after an unfinished concurrent build; the planner never reads it.
cleanup orders_status_idx Prefix: Its keys are the start of orders_status_created_idx's, so any seek it serves orders_status_created_idx serves too.
  caveat: orders_status_idx is narrower; keep it if a hot query scans it rather than seeking.
cleanup customers_region_idx Unused: Not read once in the 10 days since the server last restarted, yet updated with every write to public.customers.
cleanup products_sku_idx Duplicate: Same keys as products_sku_key: the same index twice, both maintained on every write.
table orders OverIndexed: 19 secondary indexes and about 1.0M rows written a day: every insert updates all of them.
note: PostgreSQL doesn't record missing-index requests. Candidates come from captured plans that read whole tables, foreign keys, and the table scan counters.
note: 2 indexes on partitions are left out of the clean-ups: each is a partition's copy of its partitioned table's index, a cold partition reads as unused because queries skip it, and a copy can't be dropped on its own."#;

#[test]
fn tables_read_in_full_and_engine_built_indexes_say_what_they_said() {
    let mut out = Vec::new();
    // A table read in full by two statements the advisor lists.
    let mut s = ten_days(snapshot("postgres-demo.json"));
    let t = s.tables.iter_mut().find(|t| t.name == "events").expect("events");
    let rows = t.rows.unwrap();
    (t.seq_scans, t.seq_rows_read) = (Some(4000.0), Some(4000.0 * rows));
    let w = s.workload.as_mut().unwrap();
    for (id, text) in
        [("4242", "SELECT count(*) FROM events WHERE kind = $1"), ("4243", "SELECT * FROM events WHERE id > $1")]
    {
        let mut q = w.queries.iter().find(|q| q.id == "-3722457391793582113").unwrap().clone();
        q.id = id.into();
        q.text = text.into();
        q.executions = 4000.0;
        fn retarget(n: &mut PlanNode) {
            if n.object.is_some() {
                n.object = Some("public.events".into());
                n.predicate = None;
            }
            n.children.iter_mut().for_each(retarget);
        }
        retarget(&mut q.plan.as_mut().unwrap().root);
        w.queries.push(q);
    }
    out.push(advice_words(&index_advice(&analyze(s)), &["events"]));
    // SQLite builds an index for itself for two statements each: every one is run twice under another id.
    let mut s = ten_days(snapshot("sqlite-demo.json"));
    let w = s.workload.as_mut().unwrap();
    let copies: Vec<_> = w
        .queries
        .iter()
        .filter(|q| q.plan.is_some())
        .map(|q| {
            let mut q = q.clone();
            q.id = format!("{}-again", q.id);
            q
        })
        .collect();
    w.queries.extend(copies);
    out.push(advice_words(&index_advice(&analyze(s)), &[]));
    same(&out.join("\n---\n"), FULL_READS);
}

const FULL_READS: &str = r#"counters: in the 10 days since the server last restarted
table events ScanHeavy: Read in full about 400 times a day (1.2B rows without an index). Explain the statements listed with it to check their access paths, then measure rows kept in representative runs.
note: PostgreSQL doesn't record missing-index requests. Candidates come from captured plans that read whole tables, foreign keys, and the table scan counters.
---
counters: 
candidate orders(customer_id) EngineBuilt: SQLite builds an index on customer_id of main.orders for itself every time one of 2 queries runs, because none fits, and drops it after. Created once, it saves that work on every run. They take 38% of database time. It is also the foreign key to customers, which 4 more queries filter or join on: together they take 100% of database time.
  verify: Explain one of the queries again: no automatic index on orders, and a search using the new one.
  verify: Re-scan after the workload has run: the queries' share of database time drops.
candidate order_items(product_id) ForeignKey: product_id is the foreign key to products. Without an index, deleting or re-keying a row in products reads all 200K rows of main.order_items looking for matches, and joins from products can't seek.
  verify: Deletes from products no longer scan order_items.
cleanup ix_orders_status_again Duplicate: Same keys as ix_orders_status: the same index twice, both maintained on every write.
note: SQLite doesn't record missing-index requests. Candidates come from the indexes it builds for itself on every run of the logged statements, and foreign keys.
note: Index usage counters aren't readable: unused indexes and the runtime impact of overlaps are unknown. Duplicate and prefix coverage checks use the recorded definitions."#;

#[test]
fn advisor_notes_say_what_they_said() {
    // Every statement with a plan runs twice under another id; three small tables declare foreign keys no index
    // serves; one index is a partition's copy.
    let mut s = ten_days(snapshot("postgres-demo.json"));
    let w = s.workload.as_mut().unwrap();
    let copies: Vec<_> = w
        .queries
        .iter()
        .filter(|q| q.plan.is_some())
        .map(|q| {
            let mut q = q.clone();
            q.id = format!("{}-again", q.id);
            q
        })
        .collect();
    w.queries.extend(copies);
    let template = s.tables.iter().find(|t| t.name == "products").unwrap().clone();
    for name in ["tags", "labels", "notes"] {
        let mut t = template.clone();
        t.name = name.into();
        t.rows = Some(500.0);
        s.tables.push(t);
        s.foreign_keys.push(culprit_core::ForeignKey {
            name: format!("{name}_customer_fk"),
            schema: "public".into(),
            table: name.into(),
            columns: names(&["customer_id"]),
            ref_schema: "public".into(),
            ref_table: "customers".into(),
            ref_columns: names(&["id"]),
            delete_rule: None,
        });
    }
    let mut copy = s.indexes.iter().find(|i| !i.is_primary_key).unwrap().clone();
    (copy.name, copy.partition_of) = ("orders_part_idx".into(), Some("public.orders_idx".into()));
    s.indexes.push(copy);
    same(&advice_words(&index_advice(&analyze(s)), &[]), NOTES);
}

const NOTES: &str = r#"counters: in the 10 days since the server last restarted
candidate orders(customer_id) PlanScan: 2 captured query plans scan public.orders and keep only those matching (orders.customer_id = $1); no enabled index starts with customer_id. The captured scan uses 2.0M rows as its size basis; this scan's catalog reports 2.0M rows in the table. Captured estimates can predate the current table size.
  write cost: public.orders received no writes in the 10 days since the server last restarted, so this index would cost next to nothing to maintain.
  verify: Obtain a newly compiled plan and check whether orders uses the candidate index to read fewer rows; a scan can still be appropriate for some parameters.
  verify: Compare reads and time per run after equivalent workload, including storage and write cost. Old saved plans and a changing share of database time alone do not verify improvement.
candidate order_items(product_id) PlanScan: 2 captured query plans scan public.order_items and keep only those matching (order_items.product_id = $1); no enabled index starts with product_id. The captured scan uses 1.0M rows as its size basis; this scan's catalog reports 1.0M rows in the table. Captured estimates can predate the current table size.
  write cost: public.order_items received no writes in the 10 days since the server last restarted, so this index would cost next to nothing to maintain.
  verify: Obtain a newly compiled plan and check whether order_items uses the candidate index to read fewer rows; a scan can still be appropriate for some parameters.
  verify: Compare reads and time per run after equivalent workload, including storage and write cost. Old saved plans and a changing share of database time alone do not verify improvement.
candidate order_items(order_id) ForeignKey: order_id is the foreign key to orders. Without an index, deleting or re-keying a row in orders reads all 1.0M rows of public.order_items looking for matches, and joins from orders can't seek. public.orders has had no deletes in the 10 days since the server last restarted and no updates, though: the database looks for matching rows only when a parent row is deleted or its key changes, so the index would mostly serve joins.
  write cost: public.order_items received no writes in the 10 days since the server last restarted, so this index would cost next to nothing to maintain.
  verify: Deletes from orders no longer scan order_items.
cleanup orders_status_idx Prefix: Its keys are the start of orders_status_created_idx's, so any seek it serves orders_status_created_idx serves too.
  caveat: orders_status_idx is narrower; keep it if a hot query scans it rather than seeking.
cleanup customers_region_idx Unused: Not read once in the 10 days since the server last restarted, yet updated with every write to public.customers.
cleanup products_sku_idx Duplicate: Same keys as products_sku_key: the same index twice, both maintained on every write.
note: PostgreSQL doesn't record missing-index requests. Candidates come from captured plans that read whole tables, foreign keys, and the table scan counters.
note: 1 index on partitions is left out of the clean-ups: each is a partition's copy of its partitioned table's index, a cold partition reads as unused because queries skip it, and a copy can't be dropped on its own.
note: 3 more foreign keys on tables under 10K rows have no usable index; the finding “None of the 6 foreign keys has an index that can serve it” lists them with the SQL."#;
