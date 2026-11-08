//! What the index checks say, word for word, in shapes the demo goldens don't reach. Their words come from the language
//! catalog (locales/en.json); these tests hold the English to what the checks said when they wrote it in code.

use culprit_core::{ColumnInfo, Finding, ForeignKey, IndexStat, MissingIndex, NodeKind, PlanNode, QueryStat, Snapshot};

fn snapshot(name: &str) -> Snapshot {
    let text =
        std::fs::read_to_string(format!("{}/../../fixtures/{name}", env!("CARGO_MANIFEST_DIR"))).expect("fixture");
    serde_json::from_str(&text).expect("snapshot")
}

/// Counters ten days old: absence and rates a day are said.
fn ten_days(mut s: Snapshot) -> Snapshot {
    s.server.started_at = Some("2026-10-01T12:00:00Z".into());
    s.taken_at = "2026-10-11T12:00:00Z".into();
    s.pressure.get_or_insert_with(Default::default).uptime_hours = Some(240.0);
    crate::counter_age::digests_counted_since_the_start(&mut s);
    s
}

/// Counters ten hours old: rates, none of them said a day, and no absence.
fn ten_hours(mut s: Snapshot) -> Snapshot {
    s.pressure.get_or_insert_with(Default::default).uptime_hours = Some(10.0);
    s
}

/// One check's findings on a snapshot, as the check makes them (before the pipeline refines them).
fn run(check: &str, s: &Snapshot) -> Vec<Finding> {
    culprit_checks::all().into_iter().find(|c| c.info().id == check).expect("a check").run(s)
}

/// Everything a finding says, one line a part.
fn words(f: &Finding) -> String {
    let mut out = vec![
        format!("{} · {:?} · {:?} · {:?}", f.id, f.severity, f.confidence, f.actionability),
        format!("title: {}", f.title),
        format!("summary: {}", f.summary),
        format!("impact: {}", f.impact.label),
    ];
    for m in &f.metrics {
        out.push(match &m.text {
            Some(text) => format!("metric: {}: {text}", m.label),
            None => format!("metric: {}: {}", m.label, m.value),
        });
    }
    for q in &f.queries {
        if let Some(note) = &q.note {
            out.push(format!("query {}: {note}", q.id));
        }
    }
    // The fix's SQL is code, not words: the goldens hold it.
    if let Some(fix) = &f.fix {
        out.push(format!("fix: {}", fix.summary));
        if let Some(cost) = &fix.cost {
            out.push(format!("cost: {cost}"));
        }
    }
    out.extend(f.verify.iter().map(|v| format!("verify: {v}")));
    out.extend(f.caveats.iter().map(|c| format!("caveat: {c}")));
    out.join("\n")
}

/// `check`'s finding `id` on `s` says `expected`, word for word.
#[track_caller]
fn pin(check: &str, s: &Snapshot, id: &str, expected: &str) {
    let findings = run(check, s);
    let f = findings
        .iter()
        .find(|f| f.id == id)
        .unwrap_or_else(|| panic!("no {id}: {:?}", findings.iter().map(|f| f.id.as_str()).collect::<Vec<_>>()));
    same(&words(f), expected);
}

#[track_caller]
fn same(actual: &str, expected: &str) {
    assert!(actual == expected, "said:\n{actual}\n\nwhere the English was:\n{expected}");
}

fn names(v: &[&str]) -> Vec<String> {
    v.iter().map(|c| c.to_string()).collect()
}

fn index<'a>(s: &'a mut Snapshot, name: &str) -> &'a mut IndexStat {
    s.indexes.iter_mut().find(|i| i.name == name).unwrap_or_else(|| panic!("index {name}"))
}

fn table<'a>(s: &'a mut Snapshot, name: &str) -> &'a mut culprit_core::TableStat {
    s.tables.iter_mut().find(|t| t.name == name).unwrap_or_else(|| panic!("table {name}"))
}

fn query<'a>(s: &'a mut Snapshot, id: &str) -> &'a mut QueryStat {
    s.workload.as_mut().unwrap().queries.iter_mut().find(|q| q.id == id).unwrap_or_else(|| panic!("query {id}"))
}

/// A plain secondary index like `template`, named `name` on `columns`.
fn index_like(s: &Snapshot, template: &str, name: &str, columns: &[&str]) -> IndexStat {
    let mut i = s.indexes.iter().find(|i| i.name == template).expect("template index").clone();
    i.name = name.into();
    i.columns = names(columns);
    i.include.clear();
    i.filter = None;
    i.definition = None;
    i.created_at_latest = None;
    (i.is_unique, i.is_primary_key, i.is_clustered, i.is_disabled) = (false, false, false, false);
    i
}

fn foreign_key(
    name: &str,
    schema: &str,
    table: &str,
    columns: &[&str],
    parent: &str,
    parent_columns: &[&str],
) -> ForeignKey {
    ForeignKey {
        name: name.into(),
        schema: schema.into(),
        table: table.into(),
        columns: names(columns),
        ref_schema: schema.into(),
        ref_table: parent.into(),
        ref_columns: names(parent_columns),
        delete_rule: None,
    }
}

fn scan_node(q: &mut QueryStat) -> Option<&mut PlanNode> {
    fn find(node: &mut PlanNode) -> Option<&mut PlanNode> {
        if node.kind == NodeKind::Scan && node.object.is_some() {
            return Some(node);
        }
        node.children.iter_mut().find_map(find)
    }
    find(&mut q.plan.as_mut()?.root)
}

#[test]
fn every_index_check_describes_itself() {
    let infos: Vec<String> = culprit_checks::infos()
        .into_iter()
        .filter(|i| {
            [
                "missing-index",
                "filtered-scan",
                "unused-index",
                "duplicate-index",
                "unindexed-fk",
                "overlapping-index",
                "over-indexed",
                "scan-heavy",
            ]
            .contains(&i.id.as_str())
        })
        .map(|i| format!("{}: {} — {}", i.id, i.title, i.description))
        .collect();
    same(
        &infos.join("\n"),
        r#"missing-index: Missing indexes — Index suggestions recorded by SQL Server's optimizer, merged with each other and checked against existing indexes.
filtered-scan: Scans an index would avoid — Heavy queries whose plan reads a large table in full and keeps only the rows matching a filter that no index supports.
unused-index: Unused indexes — Indexes that are maintained on every write but no query has read since usage counters were reset. One that may be less than a week old (a scan saw it appear, or its table changed before Culprit's first scan) isn't called unused yet.
duplicate-index: Duplicate and redundant indexes — Indexes with the same keys as another index, or whose keys are a leading part of a wider one.
unindexed-fk: Foreign keys without an index — Foreign key columns with no index starting with them, which makes deletes on the parent and joins from the child slow.
overlapping-index: Overlapping indexes — Two indexes that lead with the same column where one is barely read and the other holds all its key columns: queries that use the first can seek the second and filter the rest there.
over-indexed: Many indexes on write-heavy tables — Tables with many secondary indexes that are written a lot: every insert updates each index.
scan-heavy: Tables read in full over and over — Large tables that queries read in full many times a day, from the table scan counters."#,
    );
}

// duplicate-index ---------------------------------------------------------------------------------------------------

// Over a week of counters, the reads of the index to drop are shown, with how long they were counted.
#[test]
fn duplicate_index_with_its_reads_counted() {
    pin(
        "duplicate-index",
        &ten_days(snapshot("mssql-demo.json")),
        "duplicate-index:shop.products.ix_products_sku",
        r#"duplicate-index:shop.products.ix_products_sku · Low · High · OptionalImprovement
title: ix_products_sku duplicates uq_products_sku on products
summary: ix_products_sku has keys (sku) and uq_products_sku has keys (sku). They are the same index twice. Every write to shop.products maintains both.
impact: 648 KB redundant
metric: Size: 663552
metric: Reads of ix_products_sku: 1
metric: Counting for: 10
fix: Drop ix_products_sku and keep uq_products_sku.
cost: Frees 648 KB.
verify: Re-scan after removing ix_products_sku: this finding should be gone, and uq_products_sku should still exist.
verify: Compare queries on shop.products over comparable periods before and after the change, under a similar workload: their time per run should stay the same or drop. If a query gets slower, check its plan before keeping the index removed."#,
    );
    pin(
        "duplicate-index",
        &ten_days(snapshot("mssql-demo.json")),
        "duplicate-index:shop.orders.ix_orders_status",
        r#"duplicate-index:shop.orders.ix_orders_status · Low · Medium · OptionalImprovement
title: ix_orders_status is covered by the wider ix_orders_status_created on orders
summary: ix_orders_status has keys (status) and ix_orders_status_created has keys (status, created_at). Any query that can seek on ix_orders_status can seek on ix_orders_status_created instead. Every write to shop.orders maintains both.
impact: 26.5 MB redundant
metric: Size: 27795456
metric: Reads of ix_orders_status: 0
metric: Counting for: 10
fix: Drop ix_orders_status and keep ix_orders_status_created.
cost: Frees 26.5 MB.
verify: Re-scan after removing ix_orders_status: this finding should be gone, and ix_orders_status_created should still exist.
verify: Compare queries on shop.orders over comparable periods before and after the change, under a similar workload: their time per run should stay the same or drop. If a query gets slower, check its plan before keeping the index removed.
caveat: ix_orders_status is narrower, so scans of it read fewer pages. Keep it if a hot query scans it rather than seeking."#,
    );
}

// unused-index ------------------------------------------------------------------------------------------------------

// SQL Server counts the writes that maintained each index: none, here, and each unread index backs a foreign key.
#[test]
fn unused_indexes_nothing_wrote_to() {
    let mut s = ten_days(snapshot("mssql-demo.json"));
    // One of them written to.
    index(&mut s, "ix_support_tickets_customer_id").writes = Some(50_000.0);
    pin(
        "unused-index",
        &s,
        "unused-index:shop.order_items",
        r#"unused-index:shop.order_items · Low · Medium · OptionalImprovement
title: ix_order_items_product_id on order_items is never read
summary: No query has read ix_order_items_product_id in the 10 days since the server last restarted, and no write has had to update it either. It takes 26.4 MB of storage and buffer memory.
impact: 26.4 MB unused
metric: Size: 27729920
metric: Writes that updated the index: 0
metric: Counting for: 10
fix: Disable first: the definition stays, nothing reads it, and `ALTER INDEX … REBUILD` brings it back. Drop it after a full business cycle without complaints.
cost: Frees 26.4 MB and makes writes to shop.order_items cheaper.
verify: Watch the queries on this table for a week after disabling; none should get slower.
caveat: Usage counters reset when the server restarts or fails over, so an index used only by a monthly job can look unused.
caveat: Reads on read replicas or secondaries are not counted here. Check them before dropping.
caveat: ix_order_items_product_id (foreign key fk_order_items_product) also supports a foreign key: without it, deleting or updating parent rows scans this table."#,
    );
    pin(
        "unused-index",
        &s,
        "unused-index:shop.support_tickets",
        r#"unused-index:shop.support_tickets · Low · Medium · OptionalImprovement
title: ix_support_tickets_customer_id on support_tickets is never read
summary: No query has read ix_support_tickets_customer_id in the 10 days since the server last restarted, while writes had to update it 50K times. It still has to be updated on every insert, and on updates that touch its columns, and it takes 4.3 MB of storage and buffer memory.
impact: 4.3 MB unused
metric: Size: 4464640
metric: Writes that updated the index: 50000
metric: Counting for: 10
fix: Disable first: the definition stays, nothing reads it, and `ALTER INDEX … REBUILD` brings it back. Drop it after a full business cycle without complaints.
cost: Frees 4.3 MB and makes writes to shop.support_tickets cheaper.
verify: Watch the queries on this table for a week after disabling; none should get slower.
caveat: Usage counters reset when the server restarts or fails over, so an index used only by a monthly job can look unused.
caveat: Reads on read replicas or secondaries are not counted here. Check them before dropping.
caveat: ix_support_tickets_customer_id (foreign key fk_support_tickets_customer) also supports a foreign key: without it, deleting or updating parent rows scans this table."#,
    );
    // Two on one table, neither written.
    let mut s = ten_days(snapshot("mssql-demo.json"));
    index(&mut s, "ix_orders_channel").reads = Some(0.0);
    s.tables.retain(|t| t.name == "orders");
    s.indexes.retain(|i| i.table == "orders");
    pin(
        "unused-index",
        &s,
        "unused-index:shop.orders",
        r#"unused-index:shop.orders · Low · Medium · OptionalImprovement
title: 2 indexes on orders are never read
summary: No query has read ix_orders_status_created and ix_orders_channel in the 10 days since the server last restarted, and no write has had to update them either. Together they take 53.6 MB of storage and buffer memory.
impact: 53.6 MB unused
metric: Size: 56180736
metric: Writes that updated the index: 0
metric: Counting for: 10
fix: Disable first: the definition stays, nothing reads it, and `ALTER INDEX … REBUILD` brings it back. Drop it after a full business cycle without complaints.
cost: Frees 53.6 MB and makes writes to shop.orders cheaper.
verify: Watch the queries on this table for a week after disabling; none should get slower.
caveat: Usage counters reset when the server restarts or fails over, so an index used only by a monthly job can look unused.
caveat: Reads on read replicas or secondaries are not counted here. Check them before dropping."#,
    );
}

// Two unread indexes on one table, both written, one of them created eight days ago, both backing foreign keys.
#[test]
fn unused_indexes_written_and_young() {
    let mut s = ten_days(snapshot("mssql-demo.json"));
    let channel = index(&mut s, "ix_orders_channel");
    (channel.reads, channel.writes) = (Some(0.0), Some(1_000_000.0));
    channel.created_at_latest = Some("2026-10-03T12:00:00Z".into());
    channel.definition = Some("CREATE INDEX [ix_orders_channel] ON [shop].[orders] ([channel])".into());
    let created = index(&mut s, "ix_orders_status_created");
    created.writes = Some(2_000_000.0);
    created.definition =
        Some("CREATE INDEX [ix_orders_status_created] ON [shop].[orders] ([status], [created_at])".into());
    s.foreign_keys.push(foreign_key("fk_orders_channel", "shop", "orders", &["channel"], "channels", &["code"]));
    s.foreign_keys.push(foreign_key("fk_orders_status", "shop", "orders", &["status"], "statuses", &["code"]));
    s.tables.retain(|t| t.name != "order_items" && t.name != "support_tickets");
    s.indexes.retain(|i| i.table != "order_items" && i.table != "support_tickets");
    pin(
        "unused-index",
        &s,
        "unused-index:shop.orders",
        r#"unused-index:shop.orders · Medium · Medium · OptionalImprovement
title: 2 indexes on orders are never read
summary: No query has read ix_orders_status_created and ix_orders_channel in the 8 days since Oct 3, before which some of them may not have existed, while writes had to update the busiest of them 2.0M times. Each one still has to be updated on every insert, and on updates that touch its columns, and it takes 53.6 MB of storage and buffer memory.
impact: 53.6 MB unused
metric: Size: 56180736
metric: Writes that updated the index: 2000000
metric: Counting for: 8
fix: Disable first: the definition stays, nothing reads it, and `ALTER INDEX … REBUILD` brings it back. Drop it after a full business cycle without complaints.
cost: Frees 53.6 MB and makes writes to shop.orders cheaper.
verify: Watch the queries on this table for a week after disabling; none should get slower.
caveat: Usage counters reset when the server restarts or fails over, so an index used only by a monthly job can look unused.
caveat: Reads on read replicas or secondaries are not counted here. Check them before dropping.
caveat: ix_orders_status_created (foreign key fk_orders_status) and ix_orders_channel (foreign key fk_orders_channel) also support a foreign key: without it, deleting or updating parent rows scans this table.
caveat: Definitions, to recreate if needed:
CREATE INDEX [ix_orders_status_created] ON [shop].[orders] ([status], [created_at]);
CREATE INDEX [ix_orders_channel] ON [shop].[orders] ([channel]);"#,
    );
}

// PostgreSQL keeps only the table's writes; one index may be younger than the counters.
#[test]
fn unused_index_of_a_written_table() {
    let mut s = ten_days(snapshot("postgres-demo.json"));
    let customers = table(&mut s, "customers");
    (customers.inserts, customers.updates, customers.deletes) = (Some(5_000.0), Some(100.0), Some(0.0));
    index(&mut s, "customers_region_idx").created_at_latest = Some("2026-10-03T12:00:00Z".into());
    // An index left invalid by a build that stopped, no longer maintained, of no known size.
    let mut invalid = index_like(&s, "orders_status_idx", "orders_note_ccnew", &["note"]);
    (invalid.is_disabled, invalid.maintained, invalid.size_bytes) = (true, Some(false), None);
    invalid.definition = Some("CREATE INDEX orders_note_ccnew ON public.orders USING btree (note)".into());
    s.indexes.push(invalid);
    pin(
        "unused-index",
        &s,
        "unused-index:public.customers",
        r#"unused-index:public.customers · Low · Medium · OptionalImprovement
title: customers_region_idx on customers is never read
summary: No query has read customers_region_idx in the 8 days since Oct 3, before which it may not have existed, while public.customers was written 5.1K times. It still has to be updated on every insert, and on updates that touch its columns, and it takes 1.3 MB of storage and buffer memory.
impact: 1.3 MB unused
metric: Size: 1400832
metric: Writes to the table: 5100
metric: Counting for: 8
fix: Drop it concurrently so writes aren't blocked. Save the definition first so you can recreate it.
cost: Frees 1.3 MB and makes writes to public.customers cheaper.
verify: Watch the queries on this table for a week after disabling; none should get slower.
caveat: Usage counters reset when the server restarts or fails over, so an index used only by a monthly job can look unused.
caveat: Reads on read replicas or secondaries are not counted here. Check them before dropping.
caveat: Definitions, to recreate if needed:
CREATE INDEX customers_region_idx ON public.customers USING btree (region);"#,
    );
    pin(
        "unused-index",
        &s,
        "unused-index:invalid:public.orders.orders_note_ccnew",
        r#"unused-index:invalid:public.orders.orders_note_ccnew · Low · High · OptionalImprovement
title: orders_note_ccnew on orders is invalid after an unfinished index build
summary: PostgreSQL marks orders_note_ccnew invalid (indisvalid = false), which happens when CREATE INDEX CONCURRENTLY or REINDEX CONCURRENTLY stops before finishing. The planner never reads an invalid index.
impact: Invalid index
metric: Writes to the table: 0
fix: Drop the failed index concurrently so writes to public.orders stop maintaining it, or rebuild it with REINDEX INDEX CONCURRENTLY if queries still need it.
verify: Re-scan: orders_note_ccnew is no longer listed as an invalid index on public.orders.
caveat: Definition, to recreate if needed:
CREATE INDEX orders_note_ccnew ON public.orders USING btree (note);"#,
    );
}

// Two unread indexes on a table nothing wrote to, and an invalid index writes still maintain.
#[test]
fn unused_indexes_of_an_unwritten_table() {
    let mut s = ten_days(snapshot("postgres-demo.json"));
    let customers = table(&mut s, "customers");
    (customers.inserts, customers.updates, customers.deletes) = (Some(0.0), Some(0.0), Some(0.0));
    let mut name = index_like(&s, "customers_region_idx", "customers_name_idx", &["name"]);
    name.reads = Some(0.0);
    name.created_at_latest = Some("2026-10-03T12:00:00Z".into());
    s.indexes.push(name);
    let mut invalid = index_like(&s, "orders_status_idx", "orders_note_ccnew", &["note"]);
    (invalid.is_disabled, invalid.maintained, invalid.size_bytes) = (true, Some(true), Some(8.0 * 1024.0 * 1024.0));
    s.indexes.push(invalid);
    let orders = table(&mut s, "orders");
    (orders.inserts, orders.updates, orders.deletes) = (Some(2_000_000.0), Some(100.0), Some(0.0));
    pin(
        "unused-index",
        &s,
        "unused-index:public.customers",
        r#"unused-index:public.customers · Low · Medium · OptionalImprovement
title: 2 indexes on customers are never read
summary: No query has read customers_region_idx and customers_name_idx in the 8 days since Oct 3, before which some of them may not have existed, and public.customers hasn't been written to either. Together they take 2.7 MB of storage and buffer memory.
impact: 2.7 MB unused
metric: Size: 2801664
metric: Writes to the table: 0
metric: Counting for: 8
fix: Drop it concurrently so writes aren't blocked. Save the definition first so you can recreate it.
cost: Frees 2.7 MB and makes writes to public.customers cheaper.
verify: Watch the queries on this table for a week after disabling; none should get slower.
caveat: Usage counters reset when the server restarts or fails over, so an index used only by a monthly job can look unused.
caveat: Reads on read replicas or secondaries are not counted here. Check them before dropping.
caveat: Definitions, to recreate if needed:
CREATE INDEX customers_region_idx ON public.customers USING btree (region);"#,
    );
    pin(
        "unused-index",
        &s,
        "unused-index:invalid:public.orders.orders_note_ccnew",
        r#"unused-index:invalid:public.orders.orders_note_ccnew · Medium · High · OptionalImprovement
title: orders_note_ccnew on orders is invalid after an unfinished index build
summary: PostgreSQL marks orders_note_ccnew invalid (indisvalid = false), which happens when CREATE INDEX CONCURRENTLY or REINDEX CONCURRENTLY stops before finishing. The planner never reads an invalid index, yet every write to public.orders still updates it and it takes 8.0 MB on disk.
impact: 8.0 MB invalid
metric: Size: 8388608
metric: Writes to the table: 2000100
fix: Drop the failed index concurrently so writes to public.orders stop maintaining it, or rebuild it with REINDEX INDEX CONCURRENTLY if queries still need it.
cost: Frees 8.0 MB and makes writes to public.orders cheaper.
verify: Re-scan: orders_note_ccnew is no longer listed as an invalid index on public.orders."#,
    );
}

// A table whose writes the engine doesn't count: nothing is said of them.
#[test]
fn unused_index_of_a_table_whose_writes_are_unknown() {
    let mut s = ten_days(snapshot("postgres-demo.json"));
    table(&mut s, "customers").inserts = None;
    pin(
        "unused-index",
        &s,
        "unused-index:public.customers",
        r#"unused-index:public.customers · Low · Medium · OptionalImprovement
title: customers_region_idx on customers is never read
summary: No query has read customers_region_idx in the 10 days since the server last restarted. It still has to be updated on every insert, and on updates that touch its columns, and it takes 1.3 MB of storage and buffer memory.
impact: 1.3 MB unused
metric: Size: 1400832
metric: Counting for: 10
fix: Drop it concurrently so writes aren't blocked. Save the definition first so you can recreate it.
cost: Frees 1.3 MB and makes writes to public.customers cheaper.
verify: Watch the queries on this table for a week after disabling; none should get slower.
caveat: Usage counters reset when the server restarts or fails over, so an index used only by a monthly job can look unused.
caveat: Reads on read replicas or secondaries are not counted here. Check them before dropping.
caveat: Definitions, to recreate if needed:
CREATE INDEX customers_region_idx ON public.customers USING btree (region);"#,
    );
}

// MySQL counts each index's writes as the table's.
#[test]
fn unused_index_on_mysql() {
    pin(
        "unused-index",
        &ten_days(snapshot("mysql-demo.json")),
        "unused-index:shop.customers",
        r#"unused-index:shop.customers · Low · Medium · OptionalImprovement
title: ix_customers_region on customers is never read
summary: No query has read ix_customers_region in the 10 days since the server last restarted, and shop.customers hasn't been written to either. It takes 4.5 MB of storage and buffer memory.
impact: 4.5 MB unused
metric: Size: 4734976
metric: Writes to the table: 0
metric: Counting for: 10
fix: Make it invisible first: the optimizer stops using it but it stays maintained, and `ALTER INDEX … VISIBLE` brings it back instantly. Drop it after a full business cycle without complaints.
cost: Frees 4.5 MB and makes writes to shop.customers cheaper.
verify: Watch the queries on this table for a week after disabling; none should get slower.
caveat: Usage counters reset when the server restarts or fails over, so an index used only by a monthly job can look unused.
caveat: Reads on read replicas or secondaries are not counted here. Check them before dropping.
caveat: Definitions, to recreate if needed:
CREATE INDEX `ix_customers_region` ON `shop`.`customers` (region);"#,
    );
}

// overlapping-index -------------------------------------------------------------------------------------------------

#[test]
fn an_index_barely_read_next_to_a_wider_one() {
    let mut s = ten_days(snapshot("mssql-demo.json"));
    let channel = index(&mut s, "ix_orders_channel");
    (channel.columns, channel.reads) = (names(&["channel", "created_at"]), Some(5.0));
    let mut wide =
        index_like(&s, "ix_orders_channel", "ix_orders_channel_status", &["channel", "status", "created_at"]);
    wide.reads = Some(50_000.0);
    s.indexes.push(wide);
    pin(
        "overlapping-index",
        &s,
        "overlapping-index:shop.orders.ix_orders_channel",
        r#"overlapping-index:shop.orders.ix_orders_channel · Low · Low · OptionalImprovement
title: ix_orders_channel is barely read next to ix_orders_channel_status, which also starts with channel
summary: ix_orders_channel (channel, created_at) and ix_orders_channel_status (channel, status, created_at) both lead with channel on shop.orders, and the second has every key column of the first among its keys. ix_orders_channel was read 5 times in the 10 days since the server last restarted, ix_orders_channel_status 50K times. Queries that use the first can seek the second on channel and filter the rest inside it, while every write to shop.orders keeps both up to date.
impact: 21.3 MB barely used
metric: Reads of ix_orders_channel: 5
metric: Reads of ix_orders_channel_status: 50000
metric: Size: 22290432
fix: Find the queries that read ix_orders_channel and check their plans against ix_orders_channel_status; if they seek it just as well, hide ix_orders_channel first and drop it after a business cycle.
cost: Frees 21.3 MB and one index's worth of work on every write.
verify: After hiding ix_orders_channel, the queries on orders keep their speed and ix_orders_channel_status's reads grow by about 5.
caveat: ix_orders_channel may cover columns ix_orders_channel_status lacks; a query that only reads it for those columns would need a key lookup instead."#,
    );
    s.foreign_keys.push(foreign_key("fk_orders_channel", "shop", "orders", &["channel"], "channels", &["code"]));
    pin(
        "overlapping-index",
        &s,
        "overlapping-index:shop.orders.ix_orders_channel",
        r#"overlapping-index:shop.orders.ix_orders_channel · Low · Low · OptionalImprovement
title: ix_orders_channel is barely read next to ix_orders_channel_status, which also starts with channel
summary: ix_orders_channel (channel, created_at) and ix_orders_channel_status (channel, status, created_at) both lead with channel on shop.orders, and the second has every key column of the first among its keys. ix_orders_channel was read 5 times in the 10 days since the server last restarted, ix_orders_channel_status 50K times. Queries that use the first can seek the second on channel and filter the rest inside it, while every write to shop.orders keeps both up to date.
impact: 21.3 MB barely used
metric: Reads of ix_orders_channel: 5
metric: Reads of ix_orders_channel_status: 50000
metric: Size: 22290432
fix: Find the queries that read ix_orders_channel and check their plans against ix_orders_channel_status; if they seek it just as well, hide ix_orders_channel first and drop it after a business cycle.
cost: Frees 21.3 MB and one index's worth of work on every write.
verify: After hiding ix_orders_channel, the queries on orders keep their speed and ix_orders_channel_status's reads grow by about 5.
caveat: ix_orders_channel may cover columns ix_orders_channel_status lacks; a query that only reads it for those columns would need a key lookup instead.
caveat: ix_orders_channel also supports the foreign key fk_orders_channel."#,
    );
}

// over-indexed ------------------------------------------------------------------------------------------------------

/// Seven more indexes on `table`, each read.
fn many_indexes(s: &mut Snapshot, table: &str, template: &str) {
    for n in 0..7 {
        let mut i = index_like(s, template, &format!("ix_extra_{n}"), &[&format!("zz_col_{n}")]);
        i.table = table.into();
        i.reads = Some(5000.0);
        s.indexes.push(i);
    }
}

#[test]
fn many_indexes_on_postgres() {
    let mut s = ten_days(snapshot("postgres-demo.json"));
    many_indexes(&mut s, "orders", "orders_status_idx");
    let orders = table(&mut s, "orders");
    (orders.inserts, orders.updates) = (Some(100_000.0), Some(9_900_000.0));
    pin(
        "over-indexed",
        &s,
        "over-indexed:public.orders",
        r#"over-indexed:public.orders · Medium · Medium · OptionalImprovement
title: orders has 9 indexes and gets about 1.0M rows written a day
summary: Every insert into public.orders adds an entry to all 9 secondary indexes, and so does every update that isn't HOT (HOT updates change no indexed column and stay on their page): up to about 9.0M index entries a day. orders_status_idx of them is never read or duplicated by another; start there.
impact: 9 indexes · 1.0M rows written/day
metric: Secondary indexes: 9
metric: Rows written per day: 1000000
metric: Never read or duplicated: 1
fix: Hide the indexes nothing reads first (reversible), then drop them after a business cycle.
verify: Write latency on orders drops; the reads that used the hidden indexes stay as fast."#,
    );
    // The one index nothing needs is invalid, and writes still maintain it.
    let status = index(&mut s, "orders_status_idx");
    (status.is_disabled, status.maintained) = (true, Some(true));
    pin(
        "over-indexed",
        &s,
        "over-indexed:public.orders",
        r#"over-indexed:public.orders · Medium · Medium · OptionalImprovement
title: orders has 9 indexes and gets about 1.0M rows written a day
summary: Every insert into public.orders adds an entry to all 9 secondary indexes, and so does every update that isn't HOT (HOT updates change no indexed column and stay on their page): up to about 9.0M index entries a day. orders_status_idx of them is never read or duplicated by another; start there.
impact: 9 indexes · 1.0M rows written/day
metric: Secondary indexes: 9
metric: Rows written per day: 1000000
metric: Never read or duplicated: 1
fix: Drop the hidden or invalid indexes that writes still maintain.
verify: Write latency on orders drops; the reads that used the hidden indexes stay as fast."#,
    );
}

#[test]
fn many_indexes_on_sql_server() {
    let mut s = ten_days(snapshot("mssql-demo.json"));
    many_indexes(&mut s, "orders", "ix_orders_channel");
    index(&mut s, "pk_orders").writes = Some(5_000_000.0);
    pin(
        "over-indexed",
        &s,
        "over-indexed:shop.orders",
        r#"over-indexed:shop.orders · Medium · Medium · OptionalImprovement
title: orders has 11 indexes and gets about 500K write statements a day
summary: Every insert into shop.orders updates all 11 secondary indexes, and every update touching their columns updates them too: about 500K write statements a day, each one doing that for every row it changes. 2 of them (ix_orders_status and ix_orders_status_created) are never read or duplicated by another; start there.
impact: 11 indexes · 500K write statements/day
metric: Secondary indexes: 11
metric: Write statements per day: 500000
metric: Never read or duplicated: 2
fix: Hide the indexes nothing reads first (reversible), then drop them after a business cycle.
verify: Write latency on orders drops; the reads that used the hidden indexes stay as fast."#,
    );
    let mut s = ten_hours(snapshot("mssql-demo.json"));
    many_indexes(&mut s, "orders", "ix_orders_channel");
    index(&mut s, "pk_orders").writes = Some(500_000.0);
    pin(
        "over-indexed",
        &s,
        "over-indexed:shop.orders",
        r#"over-indexed:shop.orders · Medium · Medium · OptionalImprovement
title: orders has 11 indexes and gets 500K write statements in the 10 hours since the server last restarted
summary: Every insert into shop.orders updates all 11 secondary indexes, and every update touching their columns updates them too: 500K write statements in the 10 hours since the server last restarted, each one doing that for every row it changes. ix_orders_status of them is never read or duplicated by another; start there.
impact: 11 indexes · 500K write statements
metric: Secondary indexes: 11
metric: Write statements: 500000
metric: Never read or duplicated: 1
fix: Hide the indexes nothing reads first (reversible), then drop them after a business cycle.
verify: Write latency on orders drops; the reads that used the hidden indexes stay as fast."#,
    );
}

#[test]
fn many_indexes_on_mysql_all_read() {
    let mut s = ten_days(snapshot("mysql-demo.json"));
    s.indexes.retain(|i| !(i.table == "orders" && i.name == "ix_orders_status"));
    many_indexes(&mut s, "orders", "fk_orders_customer");
    s.indexes.iter_mut().find(|i| i.table == "orders" && i.name == "PRIMARY").unwrap().writes = Some(5_000_000.0);
    pin(
        "over-indexed",
        &s,
        "over-indexed:shop.orders",
        r#"over-indexed:shop.orders · Low · Low · OptionalImprovement
title: orders has 9 indexes and gets about 500K rows written a day
summary: Every insert into shop.orders updates all 9 secondary indexes, and every update touching their columns updates them too: up to about 4.5M index changes a day. All of them are read, so trim only after checking which queries rely on each.
impact: 9 indexes · 500K rows written/day
metric: Secondary indexes: 9
metric: Rows written per day: 500000
metric: Never read or duplicated: 0
fix: Review which queries need each index; fold indexes that share a leading column into one.
verify: Write latency on orders drops; the reads that used the hidden indexes stay as fast."#,
    );
}

// Under 20 hours of counters, the writes are said as counted, not a day.
#[test]
fn many_indexes_over_hours_of_counters() {
    let mut s = ten_hours(snapshot("postgres-demo.json"));
    many_indexes(&mut s, "orders", "orders_status_idx");
    let orders = table(&mut s, "orders");
    (orders.inserts, orders.updates) = (Some(100_000.0), Some(900_000.0));
    pin(
        "over-indexed",
        &s,
        "over-indexed:public.orders",
        r#"over-indexed:public.orders · Medium · Medium · OptionalImprovement
title: orders has 9 indexes and gets 1.0M rows written in the 10 hours since its statistics were last reset
summary: Every insert into public.orders adds an entry to all 9 secondary indexes, and so does every update that isn't HOT (HOT updates change no indexed column and stay on their page): up to 9.0M index entries in the 10 hours since its statistics were last reset. orders_status_idx of them is never read or duplicated by another; start there.
impact: 9 indexes · 1.0M rows written
metric: Secondary indexes: 9
metric: Rows written: 1000000
metric: Never read or duplicated: 1
fix: Hide the indexes nothing reads first (reversible), then drop them after a business cycle.
verify: Write latency on orders drops; the reads that used the hidden indexes stay as fast."#,
    );
}

// scan-heavy --------------------------------------------------------------------------------------------------------

/// The PostgreSQL demo's events table, read in full `full` times (1,000 times its rows read in all).
fn events_read_in_full(s: &mut Snapshot, full: f64) {
    let t = table(s, "events");
    t.seq_scans = Some(full);
    t.seq_rows_read = Some(1000.0 * t.rows.unwrap());
}

/// A statement on events like the demo's, with `text`; its plan scans events when `scans`, and it has none otherwise.
fn events_statement(s: &Snapshot, id: &str, text: &str, scans: bool, time_ms: f64) -> QueryStat {
    let mut q = s.queries().iter().find(|q| q.id == "2885972271926804069").expect("the events query").clone();
    q.id = id.into();
    q.text = text.into();
    q.total_time_ms = time_ms;
    if scans {
        let plan = q.plan.as_mut().expect("a plan");
        let mut scan = plan.root.clone();
        scan.children.clear();
        (scan.kind, scan.op, scan.object, scan.index) =
            (NodeKind::Scan, "Seq Scan".into(), Some("public.events".into()), None);
        scan.predicate = None;
        plan.root = scan;
    } else {
        q.plan = None;
    }
    q
}

fn add(s: &mut Snapshot, q: QueryStat) {
    let w = s.workload.as_mut().unwrap();
    w.total_time_ms += q.total_time_ms;
    w.queries.push(q);
}

// The statements that name events reach it through an index.
#[test]
fn a_table_read_in_full_by_statements_outside_the_workload() {
    let mut s = ten_days(snapshot("postgres-demo.json"));
    events_read_in_full(&mut s, 1000.0);
    pin(
        "scan-heavy",
        &s,
        "scan-heavy:public.events",
        r#"scan-heavy:public.events · Low · Low · OptionalImprovement
title: events is read in full about 100 times a day (30.0M rows a day)
summary: public.events has 300K rows, and queries have read all of it about 100 times a day in the 10 days since the server last restarted: 30.0M rows a day read without an index. The captured statements that name it reach it through an index, so the full reads come from statements outside the top of the workload.
impact: 30.0M rows scanned/day
metric: Full reads per day: 100
metric: Rows read without an index per day: 30000000
metric: Rows in table: 300000
fix: Find the statements that read it in full (they aren't among the heaviest this scan captured), explain them (Culprit can show a generic plan, which does not measure actual rows) to check their access paths. To measure each filter, capture an actual plan with representative values in a controlled environment: this runs the statement. Compare rows kept with rows read. If a filter keeps at most 10% of the rows read, consider an index on the columns it searches by; if it keeps more, read fewer rows or columns instead. Small lookup tables are fine to scan.
cost: public.events received no writes in the 10 days since the server last restarted, so any index you add would cost next to nothing to maintain.
verify: After adding an index for a selective filter, the full reads of events should grow far slower. Compare how much its scan counters grow between a scan taken once the change is live and one a day or more later (1.0K full reads and 1.7K index scans in this scan).
caveat: Counters accumulate since statistics were last reset; one nightly report can make up much of them."#,
    );
    s.workload.as_mut().unwrap().queries.retain(|q| q.id != "2885972271926804069");
    table(&mut s, "events").index_scans = None;
    pin(
        "scan-heavy",
        &s,
        "scan-heavy:public.events",
        r#"scan-heavy:public.events · Low · Low · OptionalImprovement
title: events is read in full about 100 times a day (30.0M rows a day)
summary: public.events has 300K rows, and queries have read all of it about 100 times a day in the 10 days since the server last restarted: 30.0M rows a day read without an index. None of the captured statements names it, so the scans come from statements outside the top of the workload.
impact: 30.0M rows scanned/day
metric: Full reads per day: 100
metric: Rows read without an index per day: 30000000
metric: Rows in table: 300000
fix: Find the statements that read it in full (they aren't among the heaviest this scan captured), explain them (Culprit can show a generic plan, which does not measure actual rows) to check their access paths. To measure each filter, capture an actual plan with representative values in a controlled environment: this runs the statement. Compare rows kept with rows read. If a filter keeps at most 10% of the rows read, consider an index on the columns it searches by; if it keeps more, read fewer rows or columns instead. Small lookup tables are fine to scan.
cost: public.events received no writes in the 10 days since the server last restarted, so any index you add would cost next to nothing to maintain.
verify: After adding an index for a selective filter, the full reads of events should grow far slower. Compare how much its scan counters grow between a scan taken once the change is live and one a day or more later (1.0K full reads in this scan).
caveat: Counters accumulate since statistics were last reset; one nightly report can make up much of them."#,
    );
}

// Statements whose plans read it in full, and some with no WHERE that do so on purpose.
#[test]
fn a_table_read_in_full_by_plans() {
    let mut s = ten_days(snapshot("postgres-demo.json"));
    events_read_in_full(&mut s, 1000.0);
    let q = events_statement(&s, "scans-events", "SELECT kind FROM events WHERE note = $1", true, 10.0);
    add(&mut s, q);
    pin(
        "scan-heavy",
        &s,
        "scan-heavy:public.events",
        r#"scan-heavy:public.events · Low · Medium · OptionalImprovement
title: events is read in full about 100 times a day (30.0M rows a day)
summary: public.events has 300K rows, and queries have read all of it about 100 times a day in the 10 days since the server last restarted: 30.0M rows a day read without an index. 1 of the captured statements reads it in full (its plan scans it); explain it to check the access path, then measure rows kept in representative runs.
impact: 30.0M rows scanned/day
metric: Full reads per day: 100
metric: Rows read without an index per day: 30000000
metric: Rows in table: 300000
fix: Explain the statements listed with this finding (Culprit can show a generic plan, which does not measure actual rows) to check their access paths. To measure each filter, capture an actual plan with representative values in a controlled environment: this runs the statement. Compare rows kept with rows read. If a filter keeps at most 10% of the rows read, consider an index on the columns it searches by; if it keeps more, read fewer rows or columns instead. Small lookup tables are fine to scan.
cost: public.events received no writes in the 10 days since the server last restarted, so any index you add would cost next to nothing to maintain.
verify: After adding an index for a selective filter, the full reads of events should grow far slower. Compare how much its scan counters grow between a scan taken once the change is live and one a day or more later (1.0K full reads and 1.7K index scans in this scan).
verify: Whether the fix adds an index or reads fewer rows or columns: pg_stat_statements keeps running totals, so a later scan's runs, share and averages for the query still hold every run from before the change. Scan when the change ships and again a day or more later, then compare the two in What changed, with Since at the ship time (`culprit changes <connection> --since <ship time>`): there its time per run should drop.
caveat: Counters accumulate since statistics were last reset; one nightly report can make up much of them."#,
    );
    let q = events_statement(&s, "scans-events-2", "SELECT id FROM events WHERE kind = $1", true, 30_000.0);
    add(&mut s, q);
    let q = events_statement(&s, "maybe-events", "SELECT id FROM events WHERE created_at > $1", false, 10.0);
    add(&mut s, q);
    let q = events_statement(&s, "all-events", "SELECT count(*) FROM events", true, 10.0);
    add(&mut s, q);
    pin(
        "scan-heavy",
        &s,
        "scan-heavy:public.events",
        r#"scan-heavy:public.events · Medium · Medium · OptionalImprovement
title: events is read in full about 100 times a day (30.0M rows a day)
summary: public.events has 300K rows, and queries have read all of it about 100 times a day in the 10 days since the server last restarted: 30.0M rows a day read without an index. 2 of the captured statements read it in full (their plans scan it), and 1 more without a captured plan may; explain them to check the access path, then measure rows kept in representative runs. A read from events (query all-events) reads it in full too, on purpose: it has no WHERE clause, so no index on a filter changes that.
impact: 16% of DB time
metric: Full reads per day: 100
metric: Rows read without an index per day: 30000000
metric: Rows in table: 300000
fix: Explain the statements listed with this finding (Culprit can show a generic plan, which does not measure actual rows) to check their access paths. To measure each filter, capture an actual plan with representative values in a controlled environment: this runs the statement. Compare rows kept with rows read. If a filter keeps at most 10% of the rows read, consider an index on the columns it searches by; if it keeps more, read fewer rows or columns instead. Small lookup tables are fine to scan.
cost: public.events received no writes in the 10 days since the server last restarted, so any index you add would cost next to nothing to maintain.
verify: After adding an index for a selective filter, the full reads of events should grow far slower. Compare how much its scan counters grow between a scan taken once the change is live and one a day or more later (1.0K full reads and 1.7K index scans in this scan).
verify: Whether the fix adds an index or reads fewer rows or columns: pg_stat_statements keeps running totals, so a later scan's runs, share and averages for the queries still hold every run from before the change. Scan when the change ships and again a day or more later, then compare the two in What changed, with Since at the ship time (`culprit changes <connection> --since <ship time>`): there their time per run should drop.
caveat: Counters accumulate since statistics were last reset; one nightly report can make up much of them."#,
    );
    let q = events_statement(&s, "all-events-2", "SELECT kind, count(*) FROM events GROUP BY kind", true, 10.0);
    add(&mut s, q);
    pin(
        "scan-heavy",
        &s,
        "scan-heavy:public.events",
        r#"scan-heavy:public.events · Medium · Medium · OptionalImprovement
title: events is read in full about 100 times a day (30.0M rows a day)
summary: public.events has 300K rows, and queries have read all of it about 100 times a day in the 10 days since the server last restarted: 30.0M rows a day read without an index. 2 of the captured statements read it in full (their plans scan it), and 1 more without a captured plan may; explain them to check the access path, then measure rows kept in representative runs. 2 statements with no WHERE clause (queries all-events and all-events-2) read it in full too, on purpose: no index on a filter changes that.
impact: 16% of DB time
metric: Full reads per day: 100
metric: Rows read without an index per day: 30000000
metric: Rows in table: 300000
fix: Explain the statements listed with this finding (Culprit can show a generic plan, which does not measure actual rows) to check their access paths. To measure each filter, capture an actual plan with representative values in a controlled environment: this runs the statement. Compare rows kept with rows read. If a filter keeps at most 10% of the rows read, consider an index on the columns it searches by; if it keeps more, read fewer rows or columns instead. Small lookup tables are fine to scan.
cost: public.events received no writes in the 10 days since the server last restarted, so any index you add would cost next to nothing to maintain.
verify: After adding an index for a selective filter, the full reads of events should grow far slower. Compare how much its scan counters grow between a scan taken once the change is live and one a day or more later (1.0K full reads and 1.7K index scans in this scan).
verify: Whether the fix adds an index or reads fewer rows or columns: pg_stat_statements keeps running totals, so a later scan's runs, share and averages for the queries still hold every run from before the change. Scan when the change ships and again a day or more later, then compare the two in What changed, with Since at the ship time (`culprit changes <connection> --since <ship time>`): there their time per run should drop.
caveat: Counters accumulate since statistics were last reset; one nightly report can make up much of them."#,
    );
    // A windowed source: its next scan shows the runs since.
    s.workload.as_mut().unwrap().windowed = true;
    pin(
        "scan-heavy",
        &s,
        "scan-heavy:public.events",
        r#"scan-heavy:public.events · Medium · Medium · OptionalImprovement
title: events is read in full about 100 times a day (30.0M rows a day)
summary: public.events has 300K rows, and queries have read all of it about 100 times a day in the 10 days since the server last restarted: 30.0M rows a day read without an index. 2 of the captured statements read it in full (their plans scan it), and 1 more without a captured plan may; explain them to check the access path, then measure rows kept in representative runs. 2 statements with no WHERE clause (queries all-events and all-events-2) read it in full too, on purpose: no index on a filter changes that.
impact: 16% of DB time
metric: Full reads per day: 100
metric: Rows read without an index per day: 30000000
metric: Rows in table: 300000
fix: Explain the statements listed with this finding (Culprit can show a generic plan, which does not measure actual rows) to check their access paths. To measure each filter, capture an actual plan with representative values in a controlled environment: this runs the statement. Compare rows kept with rows read. If a filter keeps at most 10% of the rows read, consider an index on the columns it searches by; if it keeps more, read fewer rows or columns instead. Small lookup tables are fine to scan.
cost: public.events received no writes in the 10 days since the server last restarted, so any index you add would cost next to nothing to maintain.
verify: After adding an index for a selective filter, the full reads of events should grow far slower. Compare how much its scan counters grow between a scan taken once the change is live and one a day or more later (1.0K full reads and 1.7K index scans in this scan).
verify: Whether the fix adds an index or reads fewer rows or columns: Re-scan after the workload has run: their time per run should drop.
caveat: Counters accumulate since statistics were last reset; one nightly report can make up much of them."#,
    );
}

// Statements without a captured plan that may read it in full, or only statements with no WHERE.
#[test]
fn a_table_read_in_full_without_plans() {
    let mut s = ten_days(snapshot("postgres-demo.json"));
    events_read_in_full(&mut s, 1000.0);
    let q = events_statement(&s, "maybe-events", "SELECT id FROM events WHERE created_at > $1", false, 10.0);
    add(&mut s, q);
    pin(
        "scan-heavy",
        &s,
        "scan-heavy:public.events",
        r#"scan-heavy:public.events · Low · Low · OptionalImprovement
title: events is read in full about 100 times a day (30.0M rows a day)
summary: public.events has 300K rows, and queries have read all of it about 100 times a day in the 10 days since the server last restarted: 30.0M rows a day read without an index. 1 of the captured statements reads or changes it without a captured plan; explain it to see whether it reads it in full.
impact: 30.0M rows scanned/day
metric: Full reads per day: 100
metric: Rows read without an index per day: 30000000
metric: Rows in table: 300000
fix: Explain the statements listed with this finding (Culprit can show a generic plan, which does not measure actual rows) to check their access paths. To measure each filter, capture an actual plan with representative values in a controlled environment: this runs the statement. Compare rows kept with rows read. If a filter keeps at most 10% of the rows read, consider an index on the columns it searches by; if it keeps more, read fewer rows or columns instead. Small lookup tables are fine to scan.
cost: public.events received no writes in the 10 days since the server last restarted, so any index you add would cost next to nothing to maintain.
verify: After adding an index for a selective filter, the full reads of events should grow far slower. Compare how much its scan counters grow between a scan taken once the change is live and one a day or more later (1.0K full reads and 1.7K index scans in this scan).
verify: Whether the fix adds an index or reads fewer rows or columns: pg_stat_statements keeps running totals, so a later scan's runs, share and averages for the query still hold every run from before the change. Scan when the change ships and again a day or more later, then compare the two in What changed, with Since at the ship time (`culprit changes <connection> --since <ship time>`): there its time per run should drop.
caveat: Counters accumulate since statistics were last reset; one nightly report can make up much of them."#,
    );
    let q = events_statement(&s, "maybe-events-2", "DELETE FROM events WHERE kind = $1", false, 10.0);
    add(&mut s, q);
    pin(
        "scan-heavy",
        &s,
        "scan-heavy:public.events",
        r#"scan-heavy:public.events · Low · Low · OptionalImprovement
title: events is read in full about 100 times a day (30.0M rows a day)
summary: public.events has 300K rows, and queries have read all of it about 100 times a day in the 10 days since the server last restarted: 30.0M rows a day read without an index. 2 of the captured statements read or change it without a captured plan; explain them to see whether they read it in full.
impact: 30.0M rows scanned/day
metric: Full reads per day: 100
metric: Rows read without an index per day: 30000000
metric: Rows in table: 300000
fix: Explain the statements listed with this finding (Culprit can show a generic plan, which does not measure actual rows) to check their access paths. To measure each filter, capture an actual plan with representative values in a controlled environment: this runs the statement. Compare rows kept with rows read. If a filter keeps at most 10% of the rows read, consider an index on the columns it searches by; if it keeps more, read fewer rows or columns instead. Small lookup tables are fine to scan.
cost: public.events received no writes in the 10 days since the server last restarted, so any index you add would cost next to nothing to maintain.
verify: After adding an index for a selective filter, the full reads of events should grow far slower. Compare how much its scan counters grow between a scan taken once the change is live and one a day or more later (1.0K full reads and 1.7K index scans in this scan).
verify: Whether the fix adds an index or reads fewer rows or columns: pg_stat_statements keeps running totals, so a later scan's runs, share and averages for the queries still hold every run from before the change. Scan when the change ships and again a day or more later, then compare the two in What changed, with Since at the ship time (`culprit changes <connection> --since <ship time>`): there their time per run should drop.
caveat: Counters accumulate since statistics were last reset; one nightly report can make up much of them."#,
    );

    let mut s = ten_days(snapshot("postgres-demo.json"));
    events_read_in_full(&mut s, 1000.0);
    let mut q = events_statement(&s, "all-events", "SELECT count(*) FROM events", true, 10.0);
    s.workload.as_mut().unwrap().queries.retain(|q| q.id != "2885972271926804069");
    q.executions = 3.0;
    add(&mut s, q);
    pin(
        "scan-heavy",
        &s,
        "scan-heavy:public.events",
        r#"scan-heavy:public.events · Low · Low · OptionalImprovement
title: events is read in full about 100 times a day (30.0M rows a day)
summary: public.events has 300K rows, and queries have read all of it about 100 times a day in the 10 days since the server last restarted: 30.0M rows a day read without an index. A read from events (query all-events) reads it in full too, on purpose: it has no WHERE clause, so no index on a filter changes that. The rest come from statements outside the top of the workload.
impact: 30.0M rows scanned/day
metric: Full reads per day: 100
metric: Rows read without an index per day: 30000000
metric: Rows in table: 300000
fix: Find the statements that read it in full (they aren't among the heaviest this scan captured), explain them (Culprit can show a generic plan, which does not measure actual rows) to check their access paths. To measure each filter, capture an actual plan with representative values in a controlled environment: this runs the statement. Compare rows kept with rows read. If a filter keeps at most 10% of the rows read, consider an index on the columns it searches by; if it keeps more, read fewer rows or columns instead. Small lookup tables are fine to scan.
cost: public.events received no writes in the 10 days since the server last restarted, so any index you add would cost next to nothing to maintain.
verify: After adding an index for a selective filter, the full reads of events should grow far slower. Compare how much its scan counters grow between a scan taken once the change is live and one a day or more later (1.0K full reads and 1.7K index scans in this scan).
caveat: Counters accumulate since statistics were last reset; one nightly report can make up much of them."#,
    );
}

// Under 20 hours of counters; and statements that stopped running, told in the past.
#[test]
fn a_table_read_in_full_over_hours_and_in_the_past() {
    let mut s = ten_hours(snapshot("postgres-demo.json"));
    events_read_in_full(&mut s, 1000.0);
    let mut q = events_statement(&s, "scans-events", "SELECT kind FROM events WHERE note = $1", true, 10.0);
    q.last_seen = Some("2026-01-01T00:00:00Z".into());
    add(&mut s, q);
    pin(
        "scan-heavy",
        &s,
        "scan-heavy:public.events",
        r#"scan-heavy:public.events · Low · Medium · OptionalImprovement
title: events was read in full 1.0K times in the 10 hours since its statistics were last reset (300.0M rows)
summary: public.events has 300K rows, and queries have read all of it 1.0K times in the 10 hours since its statistics were last reset: 300.0M rows read without an index. 1 of the captured statements reads it in full (its plan scans it); explain it to check the access path, then measure rows kept in representative runs.
impact: 300.0M rows scanned
metric: Full reads: 1000
metric: Rows read without an index: 300000000
metric: Rows in table: 300000
fix: Explain the statements listed with this finding (Culprit can show a generic plan, which does not measure actual rows) to check their access paths. To measure each filter, capture an actual plan with representative values in a controlled environment: this runs the statement. Compare rows kept with rows read. If a filter keeps at most 10% of the rows read, consider an index on the columns it searches by; if it keeps more, read fewer rows or columns instead. Small lookup tables are fine to scan.
cost: public.events had no writes in the 10 h its counters cover, too short to say it gets none (a weekly job may not have run yet); each write it gets will also update any index you add.
verify: After adding an index for a selective filter, the full reads of events should grow far slower. Compare how much its scan counters grow between a scan taken once the change is live and one a day or more later (1.0K full reads and 1.7K index scans in this scan).
verify: Whether the fix adds an index or reads fewer rows or columns: pg_stat_statements keeps running totals, so a later scan's runs, share and averages for the query still hold every run from before the change. Scan when the change ships and again a day or more later, then compare the two in What changed, with Since at the ship time (`culprit changes <connection> --since <ship time>`): there its time per run should drop.
caveat: Counters accumulate since statistics were last reset; one nightly report can make up much of them."#,
    );
}

// MySQL counts the rows each statement examined: a statement that examines the whole table reads it in full.
#[test]
fn a_table_read_in_full_by_rows_examined() {
    let examine = |s: &mut Snapshot, id: &str, text: &str, rows_a_run: f64, plan: bool| {
        let mut q = s.queries().iter().find(|q| q.id == "17f97e6ec7a22e0a").expect("a read").clone();
        q.id = id.into();
        q.text = text.into();
        q.logical_reads = Some(rows_a_run * q.executions);
        match q.plan.as_mut() {
            Some(p) if plan => {
                let mut scan = p.root.clone();
                scan.children.clear();
                (scan.kind, scan.op, scan.object, scan.index) =
                    (NodeKind::Scan, "Table scan".into(), Some("shop.customers".into()), None);
                scan.predicate = None;
                p.root = scan;
            }
            _ => q.plan = None,
        }
        add(s, q);
    };
    let mut s = ten_days(snapshot("mysql-demo.json"));
    let customers = table(&mut s, "customers");
    customers.seq_scans = Some(1000.0);
    customers.seq_rows_read = Some(1000.0 * customers.rows.unwrap());
    examine(&mut s, "examines-1", "SELECT `id` FROM `customers` WHERE `name` = ?", 199_000.0, false);
    pin(
        "scan-heavy",
        &s,
        "scan-heavy:shop.customers",
        r#"scan-heavy:shop.customers · Low · Medium · OptionalImprovement
title: customers is read in full about 100 times a day (19.9M rows a day)
summary: shop.customers has 199K rows, and queries have read all of it about 100 times a day in the 10 days since the server last restarted: 19.9M rows a day read without an index. 1 of the captured statements reads it in full (it examines 199K rows a run, about the whole table); explain it to check the access path, then measure rows kept in representative runs.
impact: 1% of DB time
metric: Full reads per day: 100
metric: Rows read without an index per day: 19911600
metric: Rows in table: 199116
fix: Explain the statements listed with this finding to check their access paths. To measure each filter, capture an actual plan with representative values in a controlled environment: this runs the statement. Compare rows kept with rows read. If a filter keeps at most 10% of the rows read, consider an index on the columns it searches by; if it keeps more, read fewer rows or columns instead. Small lookup tables are fine to scan.
cost: shop.customers received no writes in the 10 days since the server last restarted, so any index you add would cost next to nothing to maintain.
verify: After adding an index for a selective filter, the full reads of customers should grow far slower. Compare how much its scan counters grow between a scan taken once the change is live and one a day or more later (1.0K full reads in this scan).
verify: Whether the fix adds an index or reads fewer rows or columns: performance_schema keeps running totals, so a later scan's runs, share and averages for the query still hold every run from before the change. Scan when the change ships and again a day or more later, then compare the two in What changed, with Since at the ship time (`culprit changes <connection> --since <ship time>`): there its time per run should drop.
caveat: Counters accumulate since statistics were last reset; one nightly report can make up much of them."#,
    );
    examine(&mut s, "examines-2", "SELECT `email` FROM `customers` WHERE `region` = ?", 150_000.0, false);
    pin(
        "scan-heavy",
        &s,
        "scan-heavy:shop.customers",
        r#"scan-heavy:shop.customers · Low · Medium · OptionalImprovement
title: customers is read in full about 100 times a day (19.9M rows a day)
summary: shop.customers has 199K rows, and queries have read all of it about 100 times a day in the 10 days since the server last restarted: 19.9M rows a day read without an index. 2 of the captured statements read it in full (each examines half its rows or more a run); explain them to check the access path, then measure rows kept in representative runs.
impact: 2% of DB time
metric: Full reads per day: 100
metric: Rows read without an index per day: 19911600
metric: Rows in table: 199116
fix: Explain the statements listed with this finding to check their access paths. To measure each filter, capture an actual plan with representative values in a controlled environment: this runs the statement. Compare rows kept with rows read. If a filter keeps at most 10% of the rows read, consider an index on the columns it searches by; if it keeps more, read fewer rows or columns instead. Small lookup tables are fine to scan.
cost: shop.customers received no writes in the 10 days since the server last restarted, so any index you add would cost next to nothing to maintain.
verify: After adding an index for a selective filter, the full reads of customers should grow far slower. Compare how much its scan counters grow between a scan taken once the change is live and one a day or more later (1.0K full reads in this scan).
verify: Whether the fix adds an index or reads fewer rows or columns: performance_schema keeps running totals, so a later scan's runs, share and averages for the queries still hold every run from before the change. Scan when the change ships and again a day or more later, then compare the two in What changed, with Since at the ship time (`culprit changes <connection> --since <ship time>`): there their time per run should drop.
caveat: Counters accumulate since statistics were last reset; one nightly report can make up much of them."#,
    );
    examine(&mut s, "scans-customers", "SELECT `id` FROM `customers` WHERE `phone` = ?", 199_000.0, true);
    pin(
        "scan-heavy",
        &s,
        "scan-heavy:shop.customers",
        r#"scan-heavy:shop.customers · Low · Medium · OptionalImprovement
title: customers is read in full about 100 times a day (19.9M rows a day)
summary: shop.customers has 199K rows, and queries have read all of it about 100 times a day in the 10 days since the server last restarted: 19.9M rows a day read without an index. 3 of the captured statements read it in full (by their plans, or by the rows they examine a run); explain them to check the access path, then measure rows kept in representative runs.
impact: 3% of DB time
metric: Full reads per day: 100
metric: Rows read without an index per day: 19911600
metric: Rows in table: 199116
fix: Explain the statements listed with this finding to check their access paths. To measure each filter, capture an actual plan with representative values in a controlled environment: this runs the statement. Compare rows kept with rows read. If a filter keeps at most 10% of the rows read, consider an index on the columns it searches by; if it keeps more, read fewer rows or columns instead. Small lookup tables are fine to scan.
cost: shop.customers received no writes in the 10 days since the server last restarted, so any index you add would cost next to nothing to maintain.
verify: After adding an index for a selective filter, the full reads of customers should grow far slower. Compare how much its scan counters grow between a scan taken once the change is live and one a day or more later (1.0K full reads in this scan).
verify: Whether the fix adds an index or reads fewer rows or columns: performance_schema keeps running totals, so a later scan's runs, share and averages for the queries still hold every run from before the change. Scan when the change ships and again a day or more later, then compare the two in What changed, with Since at the ship time (`culprit changes <connection> --since <ship time>`): there their time per run should drop.
caveat: Counters accumulate since statistics were last reset; one nightly report can make up much of them."#,
    );
}

// filtered-scan -----------------------------------------------------------------------------------------------------

// A statement that ran once is told in the past; the key is also a foreign key; the scan is of YSQL, which costs an
// index differently.
#[test]
fn a_scan_told_in_the_past_on_a_foreign_key() {
    let mut s = snapshot("postgres-demo.json");
    query(&mut s, "5308963031944094559").executions = 1.0;
    pin(
        "filtered-scan",
        &s,
        "filtered-scan:public.orders(customer_id)",
        r#"filtered-scan:public.orders(customer_id) · Medium · High · OptionalImprovement
title: orders was read in full to find rows by customer_id
summary: One query read public.orders and kept only those matching `(orders.customer_id = $1)`. The captured scan uses 2.0M rows as its size basis; this scan's catalog reports 2.0M rows in the table. Captured estimates can predate the current table size. No index starts with customer_id, so the database had no faster way in. It accounted for 24% of database time. customer_id is also the foreign key to customers, so deletes there scan this table too.
impact: 24% of DB time
metric: Share of DB time: 0.23975915432338366
metric: Captured scan size basis: 1999995
metric: Queries: 1
fix: Test an index on customer_id as a way for the query to find matching rows without a full scan.
verify: Obtain a newly compiled plan for the query and check whether orders uses the new index to read fewer rows. A scan can remain appropriate for some parameter values.
verify: pg_stat_statements keeps running totals, so a later scan's runs, share and averages for the query still hold every run from before the change. Scan when the change ships and again a day or more later, then compare the two in What changed, with Since at the ship time (`culprit changes <connection> --since <ship time>`): there compare its time per run under equivalent load. Use the scans' read-counter changes to compare reads per new run, accounting for resets; include index storage and write cost."#,
    );
}

// Two statements read the same table by the same key; the second ran once.
#[test]
fn a_scan_by_two_statements() {
    let mut s = snapshot("postgres-demo.json");
    let mut q = s.queries().iter().find(|q| q.id == "5308963031944094559").unwrap().clone();
    q.id = "second-read".into();
    q.text = "SELECT id FROM orders WHERE customer_id = $1".into();
    add(&mut s, q);
    pin(
        "filtered-scan",
        &s,
        "filtered-scan:public.orders(customer_id)",
        r#"filtered-scan:public.orders(customer_id) · Medium · High · OptionalImprovement
title: orders is read in full to find rows by customer_id
summary: 2 queries plan to read public.orders and keep only those matching `(orders.customer_id = $1)`. The captured scan uses 2.0M rows as its size basis; this scan's catalog reports 2.0M rows in the table. Captured estimates can predate the current table size. No index starts with customer_id, so the database has no faster way in. Together they account for 39% of database time. customer_id is also the foreign key to customers, so deletes there scan this table too.
impact: 39% of DB time
metric: Share of DB time: 0.38678343852074343
metric: Captured scan size basis: 1999995
metric: Queries: 2
fix: Test an index on customer_id as a way for these queries to find matching rows without a full scan.
verify: Obtain a newly compiled plan for one of the queries and check whether orders uses the new index to read fewer rows. A scan can remain appropriate for some parameter values.
verify: pg_stat_statements keeps running totals, so a later scan's runs, share and averages for the queries still hold every run from before the change. Scan when the change ships and again a day or more later, then compare the two in What changed, with Since at the ship time (`culprit changes <connection> --since <ship time>`): there compare their time per run under equivalent load. Use the scans' read-counter changes to compare reads per new run, accounting for resets; include index storage and write cost."#,
    );
    for id in ["5308963031944094559", "second-read"] {
        query(&mut s, id).executions = 1.0;
    }
    pin(
        "filtered-scan",
        &s,
        "filtered-scan:public.orders(customer_id)",
        r#"filtered-scan:public.orders(customer_id) · Medium · High · OptionalImprovement
title: orders was read in full to find rows by customer_id
summary: 2 queries read public.orders and kept only those matching `(orders.customer_id = $1)`. The captured scan uses 2.0M rows as its size basis; this scan's catalog reports 2.0M rows in the table. Captured estimates can predate the current table size. No index starts with customer_id, so the database had no faster way in. Together they accounted for 39% of database time. customer_id is also the foreign key to customers, so deletes there scan this table too.
impact: 39% of DB time
metric: Share of DB time: 0.38678343852074343
metric: Captured scan size basis: 1999995
metric: Queries: 2
fix: Test an index on customer_id as a way for these queries to find matching rows without a full scan.
verify: Obtain a newly compiled plan for one of the queries and check whether orders uses the new index to read fewer rows. A scan can remain appropriate for some parameter values.
verify: pg_stat_statements keeps running totals, so a later scan's runs, share and averages for the queries still hold every run from before the change. Scan when the change ships and again a day or more later, then compare the two in What changed, with Since at the ship time (`culprit changes <connection> --since <ship time>`): there compare their time per run under equivalent load. Use the scans' read-counter changes to compare reads per new run, accounting for resets; include index storage and write cost."#,
    );
}

// The only index that leads with the key is invalid.
#[test]
fn a_scan_past_an_invalid_index() {
    let mut s = ten_days(snapshot("postgres-demo.json"));
    let mut invalid = index_like(&s, "orders_status_idx", "orders_customer_idx", &["customer_id"]);
    invalid.is_disabled = true;
    s.indexes.push(invalid);
    pin(
        "filtered-scan",
        &s,
        "filtered-scan:public.orders(customer_id)",
        r#"filtered-scan:public.orders(customer_id) · Medium · High · OptionalImprovement
title: orders is read in full to find rows by customer_id
summary: One query plans to read public.orders and keep only those matching `(orders.customer_id = $1)`. The captured scan uses 2.0M rows as its size basis; this scan's catalog reports 2.0M rows in the table. Captured estimates can predate the current table size. Only orders_customer_idx, which is disabled, starts with customer_id, so the database has no faster way in. It accounts for 24% of database time. customer_id is also the foreign key to customers, so deletes there scan this table too.
impact: 24% of DB time
metric: Share of DB time: 0.23975915432338366
metric: Captured scan size basis: 1999995
metric: Queries: 1
fix: orders_customer_idx has these keys, but it is invalid (a build that didn't finish), so PostgreSQL never reads it. Rebuilding it makes it usable; if it was set aside because nothing read it, the queries below now would.
verify: Obtain a newly compiled plan for the query and check whether orders uses orders_customer_idx to read fewer rows. A scan can remain appropriate for some parameter values.
verify: pg_stat_statements keeps running totals, so a later scan's runs, share and averages for the query still hold every run from before the change. Scan when the change ships and again a day or more later, then compare the two in What changed, with Since at the ship time (`culprit changes <connection> --since <ship time>`): there compare its time per run under equivalent load. Use the scans' read-counter changes to compare reads per new run, accounting for resets; include index storage and write cost."#,
    );
    query(&mut s, "5308963031944094559").executions = 1.0;
    pin(
        "filtered-scan",
        &s,
        "filtered-scan:public.orders(customer_id)",
        r#"filtered-scan:public.orders(customer_id) · Medium · High · OptionalImprovement
title: orders was read in full to find rows by customer_id
summary: One query read public.orders and kept only those matching `(orders.customer_id = $1)`. The captured scan uses 2.0M rows as its size basis; this scan's catalog reports 2.0M rows in the table. Captured estimates can predate the current table size. Only orders_customer_idx, which is disabled, starts with customer_id, so the database had no faster way in. It accounted for 24% of database time. customer_id is also the foreign key to customers, so deletes there scan this table too.
impact: 24% of DB time
metric: Share of DB time: 0.23975915432338366
metric: Captured scan size basis: 1999995
metric: Queries: 1
fix: orders_customer_idx has these keys, but it is invalid (a build that didn't finish), so PostgreSQL never reads it. Rebuilding it makes it usable; if it was set aside because nothing read it, the queries below now would.
verify: Obtain a newly compiled plan for the query and check whether orders uses orders_customer_idx to read fewer rows. A scan can remain appropriate for some parameter values.
verify: pg_stat_statements keeps running totals, so a later scan's runs, share and averages for the query still hold every run from before the change. Scan when the change ships and again a day or more later, then compare the two in What changed, with Since at the ship time (`culprit changes <connection> --since <ship time>`): there compare its time per run under equivalent load. Use the scans' read-counter changes to compare reads per new run, accounting for resets; include index storage and write cost."#,
    );
}

// A table that is gone, with an index the workload shows created on the key; and the same index dropped from a table
// that is still there.
#[test]
fn a_scan_of_a_table_that_is_gone() {
    let plant = |gone: bool| {
        let mut s = snapshot("postgres-demo.json");
        let id = "-3722457391793582113";
        let mut create = s.queries().iter().find(|q| q.id == id).cloned().expect("a scan of order_items");
        (create.id, create.plan, create.executions) = ("create".into(), None, 1.0);
        create.text = "CREATE INDEX ix_items_product ON public.order_items (product_id)".into();
        create.first_seen = Some("2026-10-11T08:00:00Z".into());
        add(&mut s, create);
        if gone {
            let q = query(&mut s, id);
            q.executions = 1.0;
            scan_node(q).unwrap().table_rows = Some(1_000_000.0);
            s.tables.retain(|t| t.name != "order_items");
            s.indexes.retain(|i| i.table != "order_items");
        }
        s
    };
    pin(
        "filtered-scan",
        &plant(false),
        "filtered-scan:public.order_items(product_id)",
        r#"filtered-scan:public.order_items(product_id) · Low · High · OptionalImprovement
title: order_items is read in full to find rows by product_id
summary: One query plans to read public.order_items and keep only those matching `(order_items.product_id = $1)`. The captured scan uses 1.0M rows as its size basis; this scan's catalog reports 1.0M rows in the table. Captured estimates can predate the current table size. No index starts with product_id, so the database has no faster way in. It accounts for 4% of database time. product_id is also the foreign key to products, so deletes there scan this table too. An index on product_id did exist: pg_stat_statements recorded `CREATE INDEX ix_items_product ON public.order_items (product_id)` on Oct 11 at 08:00 UTC, and the catalog doesn't list ix_items_product now, so it was dropped or renamed since.
impact: 4% of DB time
metric: Share of DB time: 0.04140462131320083
metric: Captured scan size basis: 1000000
metric: Queries: 1
fix: Test an index on product_id as a way for the query to find matching rows without a full scan.
verify: Obtain a newly compiled plan for the query and check whether order_items uses the new index to read fewer rows. A scan can remain appropriate for some parameter values.
verify: pg_stat_statements keeps running totals, so a later scan's runs, share and averages for the query still hold every run from before the change. Scan when the change ships and again a day or more later, then compare the two in What changed, with Since at the ship time (`culprit changes <connection> --since <ship time>`): there compare its time per run under equivalent load. Use the scans' read-counter changes to compare reads per new run, accounting for resets; include index storage and write cost."#,
    );
    pin(
        "filtered-scan",
        &plant(true),
        "filtered-scan:public.order_items(product_id)",
        r#"filtered-scan:public.order_items(product_id) · Low · High · History
title: order_items was read in full to find rows by product_id
summary: One query read public.order_items and kept only those matching `(order_items.product_id = $1)`. The captured scan uses 1.0M rows as its size basis; the current table row count is unavailable. This is not a measured count of rows read on every execution. The plan it last ran with had no index starting with product_id to seek. It accounted for 4% of database time. product_id is also the foreign key to products, so deletes there scan this table too. public.order_items isn't in the database's catalog any more. An index on product_id did exist: pg_stat_statements recorded `CREATE INDEX ix_items_product ON public.order_items (product_id)` on Oct 11 at 08:00 UTC.
impact: 4% of DB time
metric: Share of DB time: 0.04140462131320083
metric: Captured scan size basis: 1000000
metric: Queries: 1
fix: Nothing to create: public.order_items isn't in the database's catalog any more, so it was dropped or renamed since these statements ran. If they now read a table under another name, a scan of it says whether that one needs an index.
verify: Nothing to check: public.order_items is gone, and this finding goes once no captured statement reads it."#,
    );
    let mut s = plant(true);
    let mut q = s.queries().iter().find(|q| q.id == "-3722457391793582113").unwrap().clone();
    q.id = "second-read".into();
    q.text = "SELECT sum(quantity) FROM order_items WHERE product_id = $1".into();
    add(&mut s, q);
    pin(
        "filtered-scan",
        &s,
        "filtered-scan:public.order_items(product_id)",
        r#"filtered-scan:public.order_items(product_id) · Medium · High · History
title: order_items was read in full to find rows by product_id
summary: 2 queries read public.order_items and kept only those matching `(order_items.product_id = $1)`. The captured scan uses 1.0M rows as its size basis; the current table row count is unavailable. This is not a measured count of rows read on every execution. The plans they last ran with had no index starting with product_id to seek. Together they accounted for 8% of database time. product_id is also the foreign key to products, so deletes there scan this table too. public.order_items isn't in the database's catalog any more. An index on product_id did exist: pg_stat_statements recorded `CREATE INDEX ix_items_product ON public.order_items (product_id)` on Oct 11 at 08:00 UTC.
impact: 8% of DB time
metric: Share of DB time: 0.07951687646822618
metric: Captured scan size basis: 1000000
metric: Queries: 2
fix: Nothing to create: public.order_items isn't in the database's catalog any more, so it was dropped or renamed since these statements ran. If they now read a table under another name, a scan of it says whether that one needs an index.
verify: Nothing to check: public.order_items is gone, and this finding goes once no captured statement reads it."#,
    );
}

// SQL Server's windowed Query Store: no running totals to compare between scans; and a table whose rows aren't known.
#[test]
fn a_scan_on_sql_server() {
    let mut s = snapshot("mssql-demo.json");
    s.missing_indexes.clear();
    pin(
        "filtered-scan",
        &s,
        "filtered-scan:shop.order_items(order_id)",
        r#"filtered-scan:shop.order_items(order_id) · Low · High · OptionalImprovement
title: order_items is read in full to find rows by order_id
summary: One query plans to read shop.order_items and keep only those matching `[culprit_demo].[shop].[order_items].[order_id]=[@P1]`. The captured scan uses 1.5M rows as its size basis; this scan's catalog reports 1.5M rows in the table. Captured estimates can predate the current table size. No index starts with order_id, so the database has no faster way in. It accounts for 17% of database time. order_id is also the foreign key to orders, so deletes there scan this table too.
impact: 17% of DB time
metric: Share of DB time: 0.16547628884843782
metric: Captured scan size basis: 1500000
metric: Queries: 1
fix: Test an index on order_id as a way for the query to find matching rows without a full scan.
verify: Obtain a newly compiled plan for the query and check whether order_items uses the new index to read fewer rows. A scan can remain appropriate for some parameter values.
verify: Re-scan after representative runs and compare reads and time per run with equivalent parameters and load. Include index storage and write cost; an old stored plan or a changing share of database time alone does not verify improvement."#,
    );
    // A table whose rows the catalog doesn't know.
    s.tables.retain(|t| t.name != "order_items");
    pin(
        "filtered-scan",
        &s,
        "filtered-scan:shop.orders(shipped_at)",
        r#"filtered-scan:shop.orders(shipped_at) · Low · High · OptionalImprovement
title: orders is read in full to find rows by shipped_at
summary: One query plans to read shop.orders and keep only those matching `[culprit_demo].[shop].[orders].[shipped_at]>=dateadd(day,[@P1],'2026-01-01 00:00:00.000') AND [culprit_demo].[shop].[orders].[shipped_at]<dateadd(day,[@P2],'2026-01-01 00:00:00.000')`. The captured scan uses 1.0M rows as its size basis; this scan's catalog reports 1.0M rows in the table. Captured estimates can predate the current table size. No index starts with shipped_at, so the database has no faster way in. It accounts for 12% of database time.
impact: 12% of DB time
metric: Share of DB time: 0.12221322978535604
metric: Captured scan size basis: 1000000
metric: Queries: 1
fix: Test an index on shipped_at as a way for the query to find matching rows without a full scan.
verify: Obtain a newly compiled plan for the query and check whether orders uses the new index to read fewer rows. A scan can remain appropriate for some parameter values.
verify: Re-scan after representative runs and compare reads and time per run with equivalent parameters and load. Include index storage and write cost; an old stored plan or a changing share of database time alone does not verify improvement."#,
    );
}

// Two days of statements that ran a day at a time: database time a day.
#[test]
fn a_scan_over_two_days() {
    let mut s = snapshot("postgres-demo.json");
    crate::last_hours(s.workload.as_mut().unwrap(), 48.0);
    pin(
        "filtered-scan",
        &s,
        "filtered-scan:public.orders(customer_id)",
        r#"filtered-scan:public.orders(customer_id) · Low · High · OptionalImprovement
title: orders is read in full to find rows by customer_id
summary: One query plans to read public.orders and keep only those matching `(orders.customer_id = $1)`. The captured scan uses 2.0M rows as its size basis; this scan's catalog reports 2.0M rows in the table. Captured estimates can predate the current table size. No index starts with customer_id, so the database has no faster way in. It accounts for 24% of database time. customer_id is also the foreign key to customers, so deletes there scan this table too.
impact: 24% of DB time
metric: Share of DB time: 0.23975915432338366
metric: DB time per day: 18839.242554000015
metric: Captured scan size basis: 1999995
metric: Queries: 1
fix: Test an index on customer_id as a way for the query to find matching rows without a full scan.
verify: Obtain a newly compiled plan for the query and check whether orders uses the new index to read fewer rows. A scan can remain appropriate for some parameter values.
verify: Re-scan after representative runs and compare reads and time per run with equivalent parameters and load. Include index storage and write cost; an old stored plan or a changing share of database time alone does not verify improvement."#,
    );
}

// YSQL: a new ordered index may start on one tablet.
#[test]
fn a_scan_on_yugabyte() {
    let mut s = snapshot("postgres-demo.json");
    s.server.engine = culprit_core::Engine::Yugabyte;
    pin(
        "filtered-scan",
        &s,
        "filtered-scan:public.orders(customer_id)",
        r#"filtered-scan:public.orders(customer_id) · Medium · High · OptionalImprovement
title: orders is read in full to find rows by customer_id
summary: One query plans to read public.orders and keep only those matching `(orders.customer_id = $1)`. The captured scan uses 2.0M rows as its size basis; this scan's catalog reports 2.0M rows in the table. Captured estimates can predate the current table size. No index starts with customer_id, so the database has no faster way in. It accounts for 24% of database time. customer_id is also the foreign key to customers, so deletes there scan this table too.
impact: 24% of DB time
metric: Share of DB time: 0.23975915432338366
metric: Captured scan size basis: 1999995
metric: Queries: 1
fix: Test an index on customer_id as a way for the query to find matching rows without a full scan.
cost: Standalone ordered YSQL indexes may start on one range tablet. Review tablet splitting, colocation and write hotspots before deploying the index.
verify: Obtain a newly compiled plan for the query and check whether orders uses the new index to read fewer rows. A scan can remain appropriate for some parameter values.
verify: pg_stat_statements keeps running totals, so a later scan's runs, share and averages for the query still hold every run from before the change. Scan when the change ships and again a day or more later, then compare the two in What changed, with Since at the ship time (`culprit changes <connection> --since <ship time>`): there compare its time per run under equivalent load. Use the scans' read-counter changes to compare reads per new run, accounting for resets; include index storage and write cost."#,
    );
}

// unindexed-fk ------------------------------------------------------------------------------------------------------

// A key whose parent deletes every day, declared ON DELETE CASCADE, whose parent's updates are mostly not HOT, and
// that the statements go through.
#[test]
fn a_foreign_key_whose_parent_deletes() {
    let mut s = ten_days(snapshot("postgres-demo.json"));
    let orders = table(&mut s, "orders");
    orders.deletes = Some(40_000.0);
    orders.updates = Some(1_000_000.0);
    orders.storage = Some(culprit_core::TableStorage { hot_updates: Some(100_000.0), ..Default::default() });
    for fk in &mut s.foreign_keys {
        fk.delete_rule = Some(if fk.table == "order_items" { "CASCADE" } else { "SET NULL" }.into());
    }
    pin(
        "unindexed-fk",
        &s,
        "unindexed-fk:public.order_items(order_id)",
        r#"unindexed-fk:public.order_items(order_id) · Medium · High · OptionalImprovement
title: order_items has no index on order_id, its foreign key to orders
summary: Deleting a row from public.orders, or changing its key, makes the database look for matching rows in public.order_items (1.0M rows) without an index, which means reading the whole table. Queries that load order_items rows for one orders row can't seek either. This foreign key is declared ON DELETE CASCADE: deleting a row from public.orders not only reads all of public.order_items without an index, it also locks and deletes every matching row in public.order_items in the same transaction. public.orders has also had 900K non-HOT updates (10% HOT): in PostgreSQL, non-HOT updates on a referenced parent table contend with FOR KEY SHARE locks from concurrent writes on public.order_items, and any key update scans public.order_items in full while holding row locks. Deletes from public.orders came to about 4.0K rows a day, each a read of the whole of public.order_items.
impact: 1.0M rows unindexed
metric: Rows in table: 1000000
metric: Foreign key: order_items_order_id_fkey
metric: On delete: ON DELETE CASCADE
fix: Index the foreign key column on public.order_items.
cost: public.order_items received no writes in the 10 days since the server last restarted, so this index would cost next to nothing to maintain.
verify: Re-scan once the index exists: this finding should be gone, and a delete from public.orders then finds the rows of public.order_items it checks through the index, not by scanning the table."#,
    );
    pin(
        "unindexed-fk",
        &s,
        "unindexed-fk:public.orders(customer_id)",
        r#"unindexed-fk:public.orders(customer_id) · Medium · Low · OptionalImprovement
title: orders has no index on customer_id, its foreign key to customers
summary: Deleting a row from public.customers, or changing its key, makes the database look for matching rows in public.orders (2.0M rows) without an index, which means reading the whole table. Queries that load orders rows for one customers row can't seek either. This foreign key is declared ON DELETE SET NULL: deleting a row from public.customers not only reads all of public.orders without an index, it also locks and updates every matching row in public.orders in the same transaction. The statements that go through it take 24% of database time. public.customers has had no deletes in the 10 days since the server last restarted and no updates, though: the database looks for matching rows only when a parent row is deleted or its key changes, so the index would mostly serve joins.
impact: 24% of DB time
metric: Rows in table: 1999995
metric: Foreign key: orders_customer_id_fkey
metric: On delete: ON DELETE SET NULL
fix: Index the foreign key column on public.orders.
cost: public.orders receives about 104K writes a day; each one will also update this index.
verify: Explain query 5308963031944094559 again: it reads public.orders through ix_orders_customer_id instead of reading all of it.
verify: Re-scan once the index exists: this finding should be gone, and a delete from public.customers then finds the rows of public.orders it checks through the index, not by scanning the table.
verify: pg_stat_statements keeps running totals, so a later scan's runs, share and averages for the query still hold every run from before the change. Scan when the change ships and again a day or more later, then compare the two in What changed, with Since at the ship time (`culprit changes <connection> --since <ship time>`): there its time per run should drop."#,
    );
}

// A key on two columns whose parent never deletes, of a lookup table, with a filtered index that can't serve it.
#[test]
fn a_foreign_key_whose_parent_never_deletes() {
    let mut s = ten_days(snapshot("postgres-demo.json"));
    let orders = table(&mut s, "orders");
    (orders.deletes, orders.updates, orders.rows) = (Some(0.0), Some(0.0), Some(4.0));
    let fk = s.foreign_keys.iter_mut().find(|fk| fk.name == "order_items_order_id_fkey").unwrap();
    fk.columns = names(&["order_id", "product_id"]);
    fk.ref_columns = names(&["id", "product_id"]);
    let mut filtered = index_like(&s, "orders_status_idx", "order_items_open_idx", &["order_id", "product_id"]);
    filtered.table = "order_items".into();
    filtered.filter = Some("(quantity > 0)".into());
    s.indexes.push(filtered);
    pin(
        "unindexed-fk",
        &s,
        "unindexed-fk:public.order_items(order_id,product_id)",
        r#"unindexed-fk:public.order_items(order_id,product_id) · Low · Low · OptionalImprovement
title: order_items has no index on order_id and product_id, its foreign key to orders
summary: Deleting a row from public.orders, or changing its key, makes the database look for matching rows in public.order_items (1.0M rows) without an index, which means reading the whole table. Queries that load order_items rows for one orders row can't seek either. The statements that go through it take 4% of database time. public.orders has had no deletes in the 10 days since the server last restarted and no updates, though: the database looks for matching rows only when a parent row is deleted or its key changes, so the index would mostly serve joins.
impact: 4% of DB time
metric: Rows in table: 1000000
metric: Foreign key: order_items_order_id_fkey
fix: Index the foreign key columns on public.order_items.
cost: public.order_items received no writes in the 10 days since the server last restarted, so this index would cost next to nothing to maintain.
verify: Explain query -3722457391793582113 again: it reads public.order_items through ix_order_items_order_id_product_id instead of reading all of it.
verify: Re-scan once the index exists: this finding should be gone, and a delete from public.orders then finds the rows of public.order_items it checks through the index, not by scanning the table.
verify: pg_stat_statements keeps running totals, so a later scan's runs, share and averages for the query still hold every run from before the change. Scan when the change ships and again a day or more later, then compare the two in What changed, with Since at the ship time (`culprit changes <connection> --since <ship time>`): there its time per run should drop.
caveat: public.orders has only 4 rows, so each of its keys matches about 250K rows of public.order_items: the index makes deleting a parent row cheap, but a query that reads one key's rows would still read most of the table.
caveat: order_items_open_idx leads with these columns but is filtered ((quantity > 0)), so it can't serve the key: a delete on public.orders has to find the rows the filter leaves out too."#,
    );
    table(&mut s, "orders").updates = Some(10.0);
    pin(
        "unindexed-fk",
        &s,
        "unindexed-fk:public.order_items(order_id,product_id)",
        r#"unindexed-fk:public.order_items(order_id,product_id) · Low · Low · OptionalImprovement
title: order_items has no index on order_id and product_id, its foreign key to orders
summary: Deleting a row from public.orders, or changing its key, makes the database look for matching rows in public.order_items (1.0M rows) without an index, which means reading the whole table. Queries that load order_items rows for one orders row can't seek either. The statements that go through it take 4% of database time. public.orders has had no deletes in the 10 days since the server last restarted, though: the database looks for matching rows only when a parent row is deleted or its key changes, so the index would mostly serve joins.
impact: 4% of DB time
metric: Rows in table: 1000000
metric: Foreign key: order_items_order_id_fkey
fix: Index the foreign key columns on public.order_items.
cost: public.order_items received no writes in the 10 days since the server last restarted, so this index would cost next to nothing to maintain.
verify: Explain query -3722457391793582113 again: it reads public.order_items through ix_order_items_order_id_product_id instead of reading all of it.
verify: Re-scan once the index exists: this finding should be gone, and a delete from public.orders then finds the rows of public.order_items it checks through the index, not by scanning the table.
verify: pg_stat_statements keeps running totals, so a later scan's runs, share and averages for the query still hold every run from before the change. Scan when the change ships and again a day or more later, then compare the two in What changed, with Since at the ship time (`culprit changes <connection> --since <ship time>`): there its time per run should drop.
caveat: public.orders has only 4 rows, so each of its keys matches about 250K rows of public.order_items: the index makes deleting a parent row cheap, but a query that reads one key's rows would still read most of the table.
caveat: order_items_open_idx leads with these columns but is filtered ((quantity > 0)), so it can't serve the key: a delete on public.orders has to find the rows the filter leaves out too."#,
    );
}

// SQLite checks a key only on connections that ask it to; the statements that go through it, found by its plans.
#[test]
fn a_foreign_key_on_sqlite() {
    let s = snapshot("sqlite-demo.json");
    pin(
        "unindexed-fk",
        &s,
        "unindexed-fk:main.order_items(product_id)",
        r#"unindexed-fk:main.order_items(product_id) · Low · High · OptionalImprovement
title: order_items has no index on product_id, its foreign key to products
summary: Deleting a row from main.products, or changing its key, makes the database look for matching rows in main.order_items (200K rows) without an index, which means reading the whole table. Queries that load order_items rows for one products row can't seek either.
impact: 200K rows unindexed
metric: Rows in table: 200000
metric: Foreign key: order_items_fk_0
fix: Index the foreign key column on main.order_items.
verify: Re-scan once the index exists: this finding should be gone, and a delete from main.products then finds the rows of main.order_items it checks through the index, not by scanning the table.
caveat: SQLite looks for matching child rows only on connections that enable PRAGMA foreign_keys; queries that join or filter on the key need the index either way."#,
    );
    pin(
        "unindexed-fk",
        &s,
        "unindexed-fk:main.orders(customer_id)",
        r#"unindexed-fk:main.orders(customer_id) · High · High · OptionalImprovement
title: orders has no index on customer_id, its foreign key to customers
summary: Deleting a row from main.customers, or changing its key, makes the database look for matching rows in main.orders (200K rows) without an index, which means reading the whole table. Queries that load orders rows for one customers row can't seek either. The statements that go through it take 99% of database time.
impact: 99% of DB time
metric: Rows in table: 200000
metric: Foreign key: orders_fk_0
fix: Index the foreign key column on main.orders.
verify: Explain the queries listed again: each reads main.orders through ix_orders_customer_id (`SEARCH … USING INDEX ix_orders_customer_id`) instead of reading all of it.
verify: Re-scan once the index exists: this finding should be gone, and a delete from main.customers then finds the rows of main.orders it checks through the index, not by scanning the table.
verify: Re-scan after the workload has run: the queries' time per run should drop.
caveat: SQLite looks for matching child rows only on connections that enable PRAGMA foreign_keys; queries that join or filter on the key need the index either way."#,
    );
}

// Statements that read the child by the key without an index, one and then two.
#[test]
fn a_foreign_key_the_statements_read_by() {
    let mut s = ten_days(snapshot("mssql-demo.json"));
    let mut q = s.queries().iter().find(|q| q.id == "9").unwrap().clone();
    q.id = "events-of-a-ticket".into();
    q.text = "SELECT [id], [kind] FROM [shop].[ticket_events] WHERE [ticket_id] = @P1".into();
    let node = scan_node(&mut q).expect("a scan");
    node.object = Some("[culprit_demo].[shop].[ticket_events]".into());
    node.predicate = Some("[shop].[ticket_events].[ticket_id]=[@P1]".into());
    q.total_time_ms = 20_000.0;
    add(&mut s, q.clone());
    pin(
        "unindexed-fk",
        &s,
        "unindexed-fk:shop.ticket_events(ticket_id)",
        r#"unindexed-fk:shop.ticket_events(ticket_id) · Medium · High · OptionalImprovement
title: ticket_events has no index on ticket_id, its foreign key to support_tickets
summary: Deleting a row from shop.support_tickets, or changing its key, makes the database look for matching rows in shop.ticket_events (800K rows) without an index, which means reading the whole table. Queries that load ticket_events rows for one support_tickets row can't seek either. The statements that go through it take 33% of database time.
impact: 33% of DB time
metric: Rows in table: 800000
metric: Foreign key: fk_ticket_events_ticket
fix: Index the foreign key column on shop.ticket_events.
cost: shop.ticket_events received no writes in the 10 days since the server last restarted, so this index would cost next to nothing to maintain.
verify: Explain query events-of-a-ticket again: it reads shop.ticket_events through IX_ticket_events_ticket_id instead of reading all of it.
verify: Re-scan once the index exists: this finding should be gone, and a delete from shop.support_tickets then finds the rows of shop.ticket_events it checks through the index, not by scanning the table.
verify: Re-scan after the workload has run: the query's time per run should drop."#,
    );
    q.id = "events-of-a-ticket-2".into();
    q.text = "SELECT [kind] FROM [shop].[ticket_events] WHERE [ticket_id] = @P1".into();
    add(&mut s, q);
    pin(
        "unindexed-fk",
        &s,
        "unindexed-fk:shop.ticket_events(ticket_id)",
        r#"unindexed-fk:shop.ticket_events(ticket_id) · Medium · High · OptionalImprovement
title: ticket_events has no index on ticket_id, its foreign key to support_tickets
summary: Deleting a row from shop.support_tickets, or changing its key, makes the database look for matching rows in shop.ticket_events (800K rows) without an index, which means reading the whole table. Queries that load ticket_events rows for one support_tickets row can't seek either. The statements that go through it take 50% of database time.
impact: 50% of DB time
metric: Rows in table: 800000
metric: Foreign key: fk_ticket_events_ticket
fix: Index the foreign key column on shop.ticket_events.
cost: shop.ticket_events received no writes in the 10 days since the server last restarted, so this index would cost next to nothing to maintain.
verify: Explain the queries listed again: each reads shop.ticket_events through IX_ticket_events_ticket_id instead of reading all of it.
verify: Re-scan once the index exists: this finding should be gone, and a delete from shop.support_tickets then finds the rows of shop.ticket_events it checks through the index, not by scanning the table.
verify: Re-scan after the workload has run: the queries' time per run should drop."#,
    );
    // On a source of running totals, two statements: a later scan's What changed shows their time a run.
    let mut s = ten_days(snapshot("postgres-demo.json"));
    let mut q = s.queries().iter().find(|q| q.id == "-3722457391793582113").unwrap().clone();
    q.id = "second-read".into();
    q.text = "SELECT sum(quantity) FROM order_items WHERE product_id = $1".into();
    add(&mut s, q);
    pin(
        "unindexed-fk",
        &s,
        "unindexed-fk:public.order_items(product_id)",
        r#"unindexed-fk:public.order_items(product_id) · Medium · Low · OptionalImprovement
title: order_items has no index on product_id, its foreign key to products
summary: Deleting a row from public.products, or changing its key, makes the database look for matching rows in public.order_items (1.0M rows) without an index, which means reading the whole table. Queries that load order_items rows for one products row can't seek either. The statements that go through it take 8% of database time. public.products has had no deletes in the 10 days since the server last restarted, though: the database looks for matching rows only when a parent row is deleted or its key changes, so the index would mostly serve joins.
impact: 8% of DB time
metric: Rows in table: 1000000
metric: Foreign key: order_items_product_id_fkey
fix: Index the foreign key column on public.order_items.
cost: public.order_items received no writes in the 10 days since the server last restarted, so this index would cost next to nothing to maintain.
verify: Explain the queries listed again: each reads public.order_items through ix_order_items_product_id instead of reading all of it.
verify: Re-scan once the index exists: this finding should be gone, and a delete from public.products then finds the rows of public.order_items it checks through the index, not by scanning the table.
verify: pg_stat_statements keeps running totals, so a later scan's runs, share and averages for the queries listed still hold every run from before the change. Scan when the change ships and again a day or more later, then compare the two in What changed, with Since at the ship time (`culprit changes <connection> --since <ship time>`): there their time per run should drop."#,
    );
}

/// Small tables, each a key with no index: the habit rather than the table.
fn small_keys(s: &mut Snapshot, n: usize) {
    let from = s.tables.iter().filter(|t| t.name.starts_with("child_")).count();
    for k in from..from + n {
        let name = format!("child_{k}");
        let mut t = s.tables[0].clone();
        (t.name, t.rows, t.partition_of) = (name.clone(), Some(100.0 * (k + 1) as f64), None);
        s.tables.push(t);
        s.foreign_keys.push(foreign_key(
            &format!("fk_{name}"),
            &s.tables[0].schema.clone(),
            &name,
            &["parent_id"],
            "orders",
            &["id"],
        ));
    }
}

#[test]
fn many_foreign_keys_without_an_index() {
    let mut s = snapshot("postgres-demo.json");
    small_keys(&mut s, 3);
    pin(
        "unindexed-fk",
        &s,
        "unindexed-fk:schema",
        r#"unindexed-fk:schema · Low · High · OptionalImprovement
title: None of the 6 foreign keys has an index that can serve it
summary: 3 of these keys are on tables under 10K rows (3 on larger tables have findings of their own); they are child_2(parent_id) (300 rows), child_1(parent_id) (200 rows) and child_0(parent_id) (100 rows). Deleting a parent row, or changing its key, checks every child table for matching rows, and without an index that check reads the child table in full: cheap while the tables are small, slower with every row and with every new table declared the same way. When this many keys lack an index, the code that declares tables usually creates foreign keys without one: fixing it there covers the keys still to come.
impact: 6 of 6 foreign keys unindexed
metric: Foreign keys without a usable index: 6
metric: Foreign keys: 6
fix: Give each foreign key an unfiltered index led by its columns, and make declaring a foreign key declare its index wherever the schema is defined (a migration helper, the ORM's table definitions), so new keys get one too. Run the statements one at a time (psql -f does): CREATE INDEX CONCURRENTLY can't run inside a transaction.
cost: Each index is on a small table and costs little to keep up; check the busiest tables' write rates first.
verify: Re-scan: this finding is gone; the foreign-key findings for single tables go once their own fixes are applied.
caveat: A key whose parent rows are never deleted or re-keyed (a lookup table, append-only history) doesn't need the index for the check; leave those out if their write cost matters."#,
    );
    // A fourth with an index that serves it.
    let mut served = s.clone();
    small_keys(&mut served, 1);
    let mut i = index_like(&served, "orders_status_idx", "child_3_parent_idx", &["parent_id"]);
    i.table = "child_3".into();
    served.indexes.push(i);
    pin(
        "unindexed-fk",
        &served,
        "unindexed-fk:schema",
        r#"unindexed-fk:schema · Low · High · OptionalImprovement
title: 6 of 7 foreign keys have no index that can serve them
summary: 3 of these keys are on tables under 10K rows (3 on larger tables have findings of their own); they are child_2(parent_id) (300 rows), child_1(parent_id) (200 rows) and child_0(parent_id) (100 rows). Deleting a parent row, or changing its key, checks every child table for matching rows, and without an index that check reads the child table in full: cheap while the tables are small, slower with every row and with every new table declared the same way. When this many keys lack an index, the code that declares tables usually creates foreign keys without one: fixing it there covers the keys still to come.
impact: 6 of 7 foreign keys unindexed
metric: Foreign keys without a usable index: 6
metric: Foreign keys: 7
fix: Give each foreign key an unfiltered index led by its columns, and make declaring a foreign key declare its index wherever the schema is defined (a migration helper, the ORM's table definitions), so new keys get one too. Run the statements one at a time (psql -f does): CREATE INDEX CONCURRENTLY can't run inside a transaction.
cost: Each index is on a small table and costs little to keep up; check the busiest tables' write rates first.
verify: Re-scan: this finding is gone; the foreign-key findings for single tables go once their own fixes are applied.
caveat: A key whose parent rows are never deleted or re-keyed (a lookup table, append-only history) doesn't need the index for the check; leave those out if their write cost matters."#,
    );
    small_keys(&mut s, 3);
    // One of them is led by a filtered index, and one is on a table whose size isn't known.
    let mut filtered = index_like(&s, "orders_status_idx", "child_0_live_idx", &["parent_id"]);
    filtered.table = "child_0".into();
    filtered.filter = Some("(deleted_at IS NULL)".into());
    s.indexes.push(filtered);
    table(&mut s, "child_5").rows = None;
    pin(
        "unindexed-fk",
        &s,
        "unindexed-fk:schema",
        r#"unindexed-fk:schema · Low · High · OptionalImprovement
title: None of the 9 foreign keys has an index that can serve it
summary: 5 of these keys are on tables under 10K rows (3 on larger tables have findings of their own and 1 is on a table whose size isn't known (a partitioned table, for one)); the largest are child_4(parent_id) (500 rows), child_3(parent_id) (400 rows), child_2(parent_id) (300 rows) and child_1(parent_id) (200 rows). Deleting a parent row, or changing its key, checks every child table for matching rows, and without an index that check reads the child table in full: cheap while the tables are small, slower with every row and with every new table declared the same way. child_0_live_idx is led by its key's columns but filtered, so it can't serve the check: a parent delete has to find the rows the filter leaves out too. When this many keys lack an index, the code that declares tables usually creates foreign keys without one: fixing it there covers the keys still to come.
impact: 9 of 9 foreign keys unindexed
metric: Foreign keys without a usable index: 9
metric: Foreign keys: 9
metric: Covered only by a filtered index: 1
fix: Give each foreign key an unfiltered index led by its columns, and make declaring a foreign key declare its index wherever the schema is defined (a migration helper, the ORM's table definitions), so new keys get one too. Run the statements one at a time (psql -f does): CREATE INDEX CONCURRENTLY can't run inside a transaction.
cost: Each index is on a small table and costs little to keep up; check the busiest tables' write rates first.
verify: Re-scan: this finding is gone; the foreign-key findings for single tables go once their own fixes are applied.
caveat: A key whose parent rows are never deleted or re-keyed (a lookup table, append-only history) doesn't need the index for the check; leave those out if their write cost matters."#,
    );
    let mut filtered = index_like(&s, "orders_status_idx", "child_1_live_idx", &["parent_id"]);
    filtered.table = "child_1".into();
    filtered.filter = Some("(deleted_at IS NULL)".into());
    s.indexes.push(filtered);
    table(&mut s, "child_4").rows = None;
    let mut big = s.tables[0].clone();
    big.name = "child_big".into();
    big.rows = Some(50_000.0);
    s.tables.push(big);
    s.foreign_keys.push(foreign_key("fk_child_big", "public", "child_big", &["parent_id"], "orders", &["id"]));
    pin(
        "unindexed-fk",
        &s,
        "unindexed-fk:schema",
        r#"unindexed-fk:schema · Low · High · OptionalImprovement
title: None of the 10 foreign keys has an index that can serve it
summary: 4 of these keys are on tables under 10K rows (4 on larger tables have findings of their own and 2 are on tables whose size isn't known (partitioned tables, for one)); they are child_3(parent_id) (400 rows), child_2(parent_id) (300 rows), child_1(parent_id) (200 rows) and child_0(parent_id) (100 rows). Deleting a parent row, or changing its key, checks every child table for matching rows, and without an index that check reads the child table in full: cheap while the tables are small, slower with every row and with every new table declared the same way. 2 of these keys are covered only by a filtered index, which can't serve the check: a parent delete has to find the rows the filter leaves out too. When this many keys lack an index, the code that declares tables usually creates foreign keys without one: fixing it there covers the keys still to come.
impact: 10 of 10 foreign keys unindexed
metric: Foreign keys without a usable index: 10
metric: Foreign keys: 10
metric: Covered only by a filtered index: 2
fix: Give each foreign key an unfiltered index led by its columns, and make declaring a foreign key declare its index wherever the schema is defined (a migration helper, the ORM's table definitions), so new keys get one too. Run the statements one at a time (psql -f does): CREATE INDEX CONCURRENTLY can't run inside a transaction.
cost: Each index is on a small table and costs little to keep up; check the busiest tables' write rates first.
verify: Re-scan: this finding is gone; the foreign-key findings for single tables go once their own fixes are applied.
caveat: A key whose parent rows are never deleted or re-keyed (a lookup table, append-only history) doesn't need the index for the check; leave those out if their write cost matters."#,
    );
}

#[test]
fn every_foreign_key_without_an_index_on_sqlite() {
    let mut s = snapshot("sqlite-demo.json");
    small_keys(&mut s, 4);
    s.foreign_keys.retain(|fk| fk.table.starts_with("child_") || fk.name == "orders_fk_0");
    pin(
        "unindexed-fk",
        &s,
        "unindexed-fk:schema",
        r#"unindexed-fk:schema · Low · High · OptionalImprovement
title: None of the 5 foreign keys has an index that can serve it
summary: 4 of these keys are on tables under 10K rows (1 on a larger table has a finding of its own); they are child_3(parent_id) (400 rows), child_2(parent_id) (300 rows), child_1(parent_id) (200 rows) and child_0(parent_id) (100 rows). Deleting a parent row, or changing its key, checks every child table for matching rows, and without an index that check reads the child table in full: cheap while the tables are small, slower with every row and with every new table declared the same way. When this many keys lack an index, the code that declares tables usually creates foreign keys without one: fixing it there covers the keys still to come.
impact: 5 of 5 foreign keys unindexed
metric: Foreign keys without a usable index: 5
metric: Foreign keys: 5
fix: Give each foreign key an unfiltered index led by its columns, and make declaring a foreign key declare its index wherever the schema is defined (a migration helper, the ORM's table definitions), so new keys get one too.
cost: Each index is on a small table and costs little to keep up; check the busiest tables' write rates first.
verify: Re-scan: this finding is gone; the foreign-key findings for single tables go once their own fixes are applied.
caveat: SQLite looks for matching child rows only on connections that enable PRAGMA foreign_keys; queries that join or filter on the keys need the indexes either way.
caveat: A key whose parent rows are never deleted or re-keyed (a lookup table, append-only history) doesn't need the index for the check; leave those out if their write cost matters."#,
    );
}

fn column(table: &str, name: &str, data_type: &str, default: Option<&str>) -> ColumnInfo {
    ColumnInfo {
        schema: "shop".into(),
        table: table.into(),
        name: name.into(),
        data_type: data_type.into(),
        nullable: false,
        default: default.map(str::to_string),
        generated: false,
        ordinal: 99,
    }
}

// A key whose column's type differs from its parent's, and UUIDs kept as text, on each engine that has a native type.
#[test]
fn a_key_of_another_type_and_uuids_as_text() {
    let mut s = snapshot("mssql-demo.json");
    let c = s.columns.iter_mut().find(|c| c.table == "ticket_events" && c.name == "ticket_id").expect("a column");
    c.data_type = "bigint".into();
    s.columns.push(column("ticket_events", "event_guid", "char(36)", Some("(newid())")));
    let mut i = index_like(&s, "ix_customers_email", "ix_ticket_events_guid", &["event_guid"]);
    i.table = "ticket_events".into();
    s.indexes.push(i);
    pin(
        "unindexed-fk",
        &s,
        "unindexed-fk:type-mismatch:shop.ticket_events(ticket_id)",
        r#"unindexed-fk:type-mismatch:shop.ticket_events(ticket_id) · Low · High · OptionalImprovement
title: ticket_events.ticket_id (bigint) has a different type from support_tickets.id (int) on foreign key fk_ticket_events_ticket
summary: Foreign key fk_ticket_events_ticket links shop.ticket_events.ticket_id (bigint) to shop.support_tickets.id (int). Joining or checking rows across different types can force an implicit conversion that bypasses an index on the converted side, and narrower or differently signed child columns fail once parent keys exceed their range.
impact: bigint ≠ int
metric: Child column: ticket_events.ticket_id (bigint)
metric: Parent column: support_tickets.id (int)
metric: Foreign key: fk_ticket_events_ticket
fix: Align the column types on shop.ticket_events.ticket_id and shop.support_tickets.id so joins and foreign-key checks compare identical types without conversion.
cost: Changing a column's type rewrites the table or its indexes and requires dropping and recreating the foreign key constraint during a maintenance window.
verify: Re-scan after aligning the types: ticket_events.ticket_id and support_tickets.id have the same type and this finding is gone."#,
    );
    pin(
        "unindexed-fk",
        &s,
        "unindexed-fk:uuid-text:shop.ticket_events(event_guid)",
        r#"unindexed-fk:uuid-text:shop.ticket_events(event_guid) · Low · High · OptionalImprovement
title: ticket_events.event_guid stores UUIDs as char(36) instead of uniqueidentifier (800K rows)
summary: Indexed column shop.ticket_events.event_guid (800K rows) is declared as `char(36)`. Storing 36-character hex-plus-hyphen UUID strings uses more than twice the key space of SQL Server's native 16-byte `uniqueidentifier` type, enlarging every B-tree index and foreign-key reference on it and reducing buffer-pool density.
impact: char(36) -> uniqueidentifier
metric: Rows in table: 800000
metric: Column: ticket_events.event_guid (char(36))
metric: Recommended type: uniqueidentifier
fix: Migrate shop.ticket_events.event_guid (and any referencing foreign-key columns) from `char(36)` to `uniqueidentifier` during a planned schema migration.
cost: Changing an indexed column's type rewrites the table and its indexes.
verify: Re-scan after migrating shop.ticket_events.event_guid to `uniqueidentifier`: the column type is `uniqueidentifier` and index size drops."#,
    );
    let natively = [
        (
            culprit_core::Engine::Postgres,
            r#"unindexed-fk:uuid-text:shop.ticket_events(event_guid) · Low · High · OptionalImprovement
title: ticket_events.event_guid stores UUIDs as char(36) instead of uuid (800K rows)
summary: Indexed column shop.ticket_events.event_guid (800K rows) is declared as `char(36)`. Storing 36-character hex-plus-hyphen UUID strings uses more than twice the key space of PostgreSQL's native 16-byte `uuid` type, enlarging every B-tree index and foreign-key reference on it and reducing buffer-pool density.
impact: char(36) -> uuid
metric: Rows in table: 800000
metric: Column: ticket_events.event_guid (char(36))
metric: Recommended type: uuid
fix: Migrate shop.ticket_events.event_guid (and any referencing foreign-key columns) from `char(36)` to `uuid` during a planned schema migration.
cost: Changing an indexed column's type rewrites the table and its indexes.
verify: Re-scan after migrating shop.ticket_events.event_guid to `uuid`: the column type is `uuid` and index size drops."#,
        ),
        (
            culprit_core::Engine::Yugabyte,
            r#"unindexed-fk:uuid-text:shop.ticket_events(event_guid) · Low · High · OptionalImprovement
title: ticket_events.event_guid stores UUIDs as char(36) instead of uuid (800K rows)
summary: Indexed column shop.ticket_events.event_guid (800K rows) is declared as `char(36)`. Storing 36-character hex-plus-hyphen UUID strings uses more than twice the key space of YSQL's native 16-byte `uuid` type, enlarging every B-tree index and foreign-key reference on it and reducing buffer-pool density.
impact: char(36) -> uuid
metric: Rows in table: 800000
metric: Column: ticket_events.event_guid (char(36))
metric: Recommended type: uuid
fix: Migrate shop.ticket_events.event_guid (and any referencing foreign-key columns) from `char(36)` to `uuid` during a planned schema migration.
cost: Changing an indexed column's type rewrites the table and its indexes.
verify: Re-scan after migrating shop.ticket_events.event_guid to `uuid`: the column type is `uuid` and index size drops."#,
        ),
        (
            culprit_core::Engine::Mysql,
            r#"unindexed-fk:uuid-text:shop.ticket_events(event_guid) · Low · High · OptionalImprovement
title: ticket_events.event_guid stores UUIDs as char(36) instead of binary(16) (800K rows)
summary: Indexed column shop.ticket_events.event_guid (800K rows) is declared as `char(36)`. Storing 36-character hex-plus-hyphen UUID strings uses more than twice the key space of MySQL's 16-byte `binary(16)` (with `UUID_TO_BIN` / `BIN_TO_UUID`), enlarging every B-tree index and foreign-key reference on it and reducing buffer-pool density.
impact: char(36) -> binary(16)
metric: Rows in table: 800000
metric: Column: ticket_events.event_guid (char(36))
metric: Recommended type: binary(16)
fix: Migrate shop.ticket_events.event_guid (and any referencing foreign-key columns) from `char(36)` to `binary(16)` during a planned schema migration.
cost: Changing an indexed column's type rewrites the table and its indexes.
verify: Re-scan after migrating shop.ticket_events.event_guid to `binary(16)`: the column type is `binary(16)` and index size drops."#,
        ),
    ];
    for (engine, expected) in natively {
        s.server.engine = engine;
        // YSQL's indexes say how DocDB shards them, or nothing is judged.
        for i in &mut s.indexes {
            i.docdb = (engine == culprit_core::Engine::Yugabyte)
                .then(|| culprit_core::signals::DocdbIndex { hash_columns: Vec::new(), tablets: Some(1) });
        }
        pin("unindexed-fk", &s, "unindexed-fk:uuid-text:shop.ticket_events(event_guid)", expected);
    }
}

// missing-index -----------------------------------------------------------------------------------------------------

/// SQL Server's optimizer asks for an index on support_tickets (`equality`, then `inequality`, including `include`) for
/// one query of the window, which compares is_deleted with `comparison`.
fn asked(equality: &[&str], inequality: &[&str], include: &[&str], comparison: &str) -> Snapshot {
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
    let mut q = w.queries[0].clone();
    q.id = "4242".into();
    q.query_hash = Some(hash.into());
    q.plan = None;
    q.text = format!(
        "(@par0 int,@par1 bit)select [id], [status] from [shop].[support_tickets] where (([customer_id] = @par0) and ({comparison}))"
    );
    w.queries.push(q);
    s
}

/// An index on support_tickets like its customer_id index, with `columns`, `filter` and `include`.
fn ticket_index(s: &mut Snapshot, name: &str, columns: &[&str], filter: Option<&str>, include: &[&str]) {
    let mut i = index_like(s, "ix_support_tickets_customer_id", name, columns);
    i.filter = filter.map(str::to_string);
    i.include = names(include);
    s.indexes.push(i);
}

// The demo's requests: one a query in the window made, one none did; then the same with Azure SQL's recommendation, and
// with a request asking to include more columns than an index should.
#[test]
fn requests_with_and_without_their_queries() {
    let mut s = ten_days(snapshot("mssql-demo.json"));
    pin(
        "missing-index",
        &s,
        "missing-index:shop.order_items(order_id)",
        r#"missing-index:shop.order_items(order_id) · Low · High · OptionalImprovement
title: order_items is searched by order_id without a supporting index
summary: SQL Server's optimizer asked for an index on shop.order_items (order_id) 300 times in the 10 days since the server last restarted. Without it, each of those lookups reads more of shop.order_items (1.5M rows) than it needs to. It estimates the index would make them about 99% cheaper. The query that asked for it accounts for 17% of database time.
impact: 17% of DB time
metric: Share of DB time: 0.16547628884843782
metric: Lookups that wanted it: 300
metric: Estimated improvement: 0.9947
metric: Rows in table: 1500000
fix: Create an index on order_id.
cost: shop.order_items received no writes in the 10 days since the server last restarted, so this index would cost next to nothing to maintain.
verify: Test the candidate index with representative parameters and load, then capture a newly compiled plan. Old stored suggestions need not disappear.
verify: Check whether the queries listed here use the new index and compare reads and execution time under equivalent parameters and load, including index storage and write cost.
caveat: Missing-index suggestions are per-query and ignore the cost of maintaining the index; review overlapping suggestions together."#,
    );
    pin(
        "missing-index",
        &s,
        "missing-index:shop.orders(shipped_at)",
        r#"missing-index:shop.orders(shipped_at) · Low · High · OptionalImprovement
title: orders is searched by shipped_at without a supporting index
summary: SQL Server's optimizer asked for an index on shop.orders (shipped_at) 150 times in the 10 days since the server last restarted. Without it, each of those lookups reads more of shop.orders (1.0M rows) than it needs to. It estimates the index would make them about 81% cheaper. The query that asked for it accounts for 12% of database time.
impact: 12% of DB time
metric: Share of DB time: 0.12221322978535604
metric: Lookups that wanted it: 150
metric: Estimated improvement: 0.81
metric: Rows in table: 1000000
fix: Create an index on shipped_at, including total.
cost: shop.orders received no writes in the 10 days since the server last restarted, so this index would cost next to nothing to maintain.
verify: Test the candidate index with representative parameters and load, then capture a newly compiled plan. Old stored suggestions need not disappear.
verify: Check whether the queries listed here use the new index and compare reads and execution time under equivalent parameters and load, including index storage and write cost.
caveat: Missing-index suggestions are per-query and ignore the cost of maintaining the index; review overlapping suggestions together."#,
    );
    let m = s.missing_indexes.iter_mut().find(|m| m.table == "order_items").unwrap();
    m.include = names(&["a1", "a2", "a3", "a4", "a5", "a6", "a7"]);
    s.tuning = Some(culprit_core::TuningAdvice {
        recommendations: vec![culprit_core::TuningRecommendation {
            name: "IR_1".into(),
            kind: "CreateIndex".into(),
            state: Some("Active".into()),
            score: Some(64.0),
            index: Some(culprit_core::RecommendedIndex {
                schema: "shop".into(),
                table: "order_items".into(),
                name: Some("nci_wi_order_items_1".into()),
                columns: names(&["order_id"]),
                include: Vec::new(),
            }),
            ..Default::default()
        }],
        ..Default::default()
    });
    pin(
        "missing-index",
        &s,
        "missing-index:shop.order_items(order_id)",
        r#"missing-index:shop.order_items(order_id) · Low · High · OptionalImprovement
title: order_items is searched by order_id without a supporting index
summary: SQL Server's optimizer asked for an index on shop.order_items (order_id) 300 times in the 10 days since the server last restarted. Without it, each of those lookups reads more of shop.order_items (1.5M rows) than it needs to. It estimates the index would make them about 99% cheaper. The query that asked for it accounts for 17% of database time. Azure SQL's automatic tuning recommends the same index, scoring it 64 out of 100.
impact: 17% of DB time
metric: Share of DB time: 0.16547628884843782
metric: Lookups that wanted it: 300
metric: Estimated improvement: 0.9947
metric: Rows in table: 1500000
fix: Create an index on order_id.
cost: shop.order_items received no writes in the 10 days since the server last restarted, so this index would cost next to nothing to maintain.
verify: Test the candidate index with representative parameters and load, then capture a newly compiled plan. Old stored suggestions need not disappear.
verify: Check whether the queries listed here use the new index and compare reads and execution time under equivalent parameters and load, including index storage and write cost.
caveat: SQL Server also suggested including 7 more columns. That makes a large, write-heavy index; the fix keeps the keys only. Add INCLUDE columns only for the columns the hot queries actually select.
caveat: Missing-index suggestions are per-query and ignore the cost of maintaining the index; review overlapping suggestions together."#,
    );
    s.tuning.as_mut().unwrap().recommendations[0].score = None;
    pin(
        "missing-index",
        &s,
        "missing-index:shop.order_items(order_id)",
        r#"missing-index:shop.order_items(order_id) · Low · High · OptionalImprovement
title: order_items is searched by order_id without a supporting index
summary: SQL Server's optimizer asked for an index on shop.order_items (order_id) 300 times in the 10 days since the server last restarted. Without it, each of those lookups reads more of shop.order_items (1.5M rows) than it needs to. It estimates the index would make them about 99% cheaper. The query that asked for it accounts for 17% of database time. Azure SQL's automatic tuning recommends the same index.
impact: 17% of DB time
metric: Share of DB time: 0.16547628884843782
metric: Lookups that wanted it: 300
metric: Estimated improvement: 0.9947
metric: Rows in table: 1500000
fix: Create an index on order_id.
cost: shop.order_items received no writes in the 10 days since the server last restarted, so this index would cost next to nothing to maintain.
verify: Test the candidate index with representative parameters and load, then capture a newly compiled plan. Old stored suggestions need not disappear.
verify: Check whether the queries listed here use the new index and compare reads and execution time under equivalent parameters and load, including index storage and write cost.
caveat: SQL Server also suggested including 7 more columns. That makes a large, write-heavy index; the fix keeps the keys only. Add INCLUDE columns only for the columns the hot queries actually select.
caveat: Missing-index suggestions are per-query and ignore the cost of maintaining the index; review overlapping suggestions together."#,
    );
}

// A request made by two queries, with keys to put in order and large columns to include.
#[test]
fn a_request_of_two_queries() {
    let mut s = asked(&["customer_id", "is_deleted"], &[], &["status", "body"], "[is_deleted] = 0");
    s.columns.push(column("support_tickets", "body", "nvarchar(max)", None));
    let mut q = s.queries().iter().find(|q| q.id == "4242").unwrap().clone();
    q.id = "4343".into();
    add(&mut s, q);
    pin(
        "missing-index",
        &s,
        "missing-index:shop.support_tickets(customer_id,is_deleted)",
        r#"missing-index:shop.support_tickets(customer_id,is_deleted) · Medium · High · OptionalImprovement
title: support_tickets is searched by customer_id and is_deleted without a supporting index
summary: SQL Server's optimizer asked for an index on shop.support_tickets (customer_id and is_deleted) 400 times in the 10 days since the server last restarted. Without it, each of those lookups reads more of shop.support_tickets (300K rows) than it needs to. It estimates the index would make them about 95% cheaper. The 2 queries that asked for it account for 70% of database time.
impact: 70% of DB time
metric: Share of DB time: 0.7046436248448801
metric: Lookups that wanted it: 400
metric: Estimated improvement: 0.95
metric: Rows in table: 300000
fix: Create an index on customer_id and is_deleted, including status. Equality columns come first; put the most selective one at the front if you know it.
cost: shop.support_tickets received no writes in the 10 days since the server last restarted, so this index would cost next to nothing to maintain.
verify: Test the candidate index with representative parameters and load, then capture a newly compiled plan. Old stored suggestions need not disappear.
verify: Check whether the queries listed here use the new index and compare reads and execution time under equivalent parameters and load, including index storage and write cost.
caveat: Left body out of INCLUDE: its type can hold large values, and including it would copy whatever it holds into the index. The rows the index finds go back to the table for it.
caveat: Missing-index suggestions are per-query and ignore the cost of maintaining the index; review overlapping suggestions together."#,
    );
    let t = table(&mut s, "support_tickets");
    t.lob_bytes = Some(0.0);
    s.columns.push(column("support_tickets", "notes", "ntext", None));
    s.missing_indexes.last_mut().unwrap().include.push("notes".into());
    pin(
        "missing-index",
        &s,
        "missing-index:shop.support_tickets(customer_id,is_deleted)",
        r#"missing-index:shop.support_tickets(customer_id,is_deleted) · Medium · High · OptionalImprovement
title: support_tickets is searched by customer_id and is_deleted without a supporting index
summary: SQL Server's optimizer asked for an index on shop.support_tickets (customer_id and is_deleted) 400 times in the 10 days since the server last restarted. Without it, each of those lookups reads more of shop.support_tickets (300K rows) than it needs to. It estimates the index would make them about 95% cheaper. The 2 queries that asked for it account for 70% of database time.
impact: 70% of DB time
metric: Share of DB time: 0.7046436248448801
metric: Lookups that wanted it: 400
metric: Estimated improvement: 0.95
metric: Rows in table: 300000
fix: Create an index on customer_id and is_deleted, including status. Equality columns come first; put the most selective one at the front if you know it.
cost: shop.support_tickets received no writes in the 10 days since the server last restarted, so this index would cost next to nothing to maintain.
verify: Test the candidate index with representative parameters and load, then capture a newly compiled plan. Old stored suggestions need not disappear.
verify: Check whether the queries listed here use the new index and compare reads and execution time under equivalent parameters and load, including index storage and write cost.
caveat: Left body and notes out of INCLUDE: their type can hold large values, though shop.support_tickets keeps none apart from its rows now, and including them would copy whatever they hold into the index. The rows the index finds go back to the table for them.
caveat: Missing-index suggestions are per-query and ignore the cost of maintaining the index; review overlapping suggestions together."#,
    );
    table(&mut s, "support_tickets").lob_bytes = Some(5.0e9);
    pin(
        "missing-index",
        &s,
        "missing-index:shop.support_tickets(customer_id,is_deleted)",
        r#"missing-index:shop.support_tickets(customer_id,is_deleted) · Medium · High · OptionalImprovement
title: support_tickets is searched by customer_id and is_deleted without a supporting index
summary: SQL Server's optimizer asked for an index on shop.support_tickets (customer_id and is_deleted) 400 times in the 10 days since the server last restarted. Without it, each of those lookups reads more of shop.support_tickets (300K rows) than it needs to. It estimates the index would make them about 95% cheaper. The 2 queries that asked for it account for 70% of database time.
impact: 70% of DB time
metric: Share of DB time: 0.7046436248448801
metric: Lookups that wanted it: 400
metric: Estimated improvement: 0.95
metric: Rows in table: 300000
fix: Create an index on customer_id and is_deleted, including status. Equality columns come first; put the most selective one at the front if you know it.
cost: shop.support_tickets received no writes in the 10 days since the server last restarted, so this index would cost next to nothing to maintain.
verify: Test the candidate index with representative parameters and load, then capture a newly compiled plan. Old stored suggestions need not disappear.
verify: Check whether the queries listed here use the new index and compare reads and execution time under equivalent parameters and load, including index storage and write cost.
caveat: Left body and notes out of INCLUDE: their type can hold large values, and shop.support_tickets keeps 4.7 GB of large values apart from its rows. Including them could copy that data into the index; the rows the index finds go back to the table for them, which costs less.
caveat: Missing-index suggestions are per-query and ignore the cost of maintaining the index; review overlapping suggestions together."#,
    );
}

// Requests the existing indexes already serve: widened, brought back, or already started.
#[test]
fn requests_an_existing_index_serves() {
    // A unique index with the keys: rebuilt to include the column, staying unique.
    let mut s = asked(&["customer_id"], &["created_at"], &["status"], "[is_deleted] = 0");
    ticket_index(&mut s, "ux_support_tickets_customer", &["customer_id", "created_at DESC"], None, &["priority"]);
    s.indexes.last_mut().unwrap().is_unique = true;
    pin(
        "missing-index",
        &s,
        "missing-index:shop.support_tickets(customer_id,created_at)",
        r#"missing-index:shop.support_tickets(customer_id,created_at) · Medium · High · OptionalImprovement
title: support_tickets is searched by customer_id and created_at without a supporting index
summary: SQL Server's optimizer asked for an index on shop.support_tickets (customer_id and created_at) 400 times in the 10 days since the server last restarted. Without it, each of those lookups reads more of shop.support_tickets (300K rows) than it needs to. It estimates the index would make them about 95% cheaper. The query that asked for it accounts for 54% of database time.
impact: 54% of DB time
metric: Share of DB time: 0.5439766525721529
metric: Lookups that wanted it: 400
metric: Estimated improvement: 0.95
metric: Rows in table: 300000
fix: ux_support_tickets_customer already has the right keys but makes every matching row go back to the table for status. Rebuild it with those columns included so it covers the query. The statement keeps its keys, their order, its filter and the columns it already includes; restate DATA_COMPRESSION, FILLFACTOR or its partition scheme in it if the index uses them. It stays unique. If ux_support_tickets_customer enforces a UNIQUE constraint rather than being a unique index, SQL Server refuses the rebuild (error 1907): create a separate index with these keys and included columns instead.
cost: The index grows from 4.3 MB; writes that touch the included columns maintain it.
verify: After testing the rebuild of ux_support_tickets_customer, capture a newly compiled plan under representative load. A stored suggestion can remain in old plan XML.
verify: Check whether the queries listed here use ux_support_tickets_customer and compare reads and execution time under equivalent parameters and load, including index storage and write cost.
caveat: Missing-index suggestions are per-query and ignore the cost of maintaining the index; review overlapping suggestions together."#,
    );
    // A primary key with the keys: a separate index, and why.
    let mut s = asked(&["customer_id"], &["created_at"], &["status"], "[is_deleted] = 0");
    ticket_index(&mut s, "pk_support_tickets_customer", &["customer_id", "created_at"], None, &[]);
    let pk = s.indexes.last_mut().unwrap();
    (pk.is_primary_key, pk.is_unique) = (true, true);
    pin(
        "missing-index",
        &s,
        "missing-index:shop.support_tickets(customer_id,created_at)",
        r#"missing-index:shop.support_tickets(customer_id,created_at) · Medium · High · OptionalImprovement
title: support_tickets is searched by customer_id and created_at without a supporting index
summary: SQL Server's optimizer asked for an index on shop.support_tickets (customer_id and created_at) 400 times in the 10 days since the server last restarted. Without it, each of those lookups reads more of shop.support_tickets (300K rows) than it needs to. It estimates the index would make them about 95% cheaper. The query that asked for it accounts for 54% of database time.
impact: 54% of DB time
metric: Share of DB time: 0.5439766525721529
metric: Lookups that wanted it: 400
metric: Estimated improvement: 0.95
metric: Rows in table: 300000
fix: pk_support_tickets_customer enforces the primary key, and SQL Server won't change a constraint's index to include columns. Create an index on customer_id and created_at, including status.
cost: shop.support_tickets received no writes in the 10 days since the server last restarted, so this index would cost next to nothing to maintain.
verify: Test the candidate index with representative parameters and load, then capture a newly compiled plan. Old stored suggestions need not disappear.
verify: Check whether the queries listed here use the new index and compare reads and execution time under equivalent parameters and load, including index storage and write cost.
caveat: Missing-index suggestions are per-query and ignore the cost of maintaining the index; review overlapping suggestions together."#,
    );
    // An index that already has the keys and the column.
    let mut s = asked(&["customer_id"], &[], &["status"], "[is_deleted] = 0");
    index(&mut s, "ix_support_tickets_customer_id").include = names(&["status"]);
    pin(
        "missing-index",
        &s,
        "missing-index:shop.support_tickets(customer_id)",
        r#"missing-index:shop.support_tickets(customer_id) · Medium · High · OptionalImprovement
title: support_tickets is searched by customer_id without a supporting index
summary: SQL Server's optimizer asked for an index on shop.support_tickets (customer_id) 400 times in the 10 days since the server last restarted. Without it, each of those lookups reads more of shop.support_tickets (300K rows) than it needs to. It estimates the index would make them about 95% cheaper. The query that asked for it accounts for 54% of database time.
impact: 54% of DB time
metric: Share of DB time: 0.5439766525721529
metric: Lookups that wanted it: 400
metric: Estimated improvement: 0.95
metric: Rows in table: 300000
fix: ix_support_tickets_customer_id already starts with these columns, so the remaining gap is small. Check whether the queries below use it; if not, look at their predicates for type mismatches or functions on the column.
verify: Check whether newly compiled plans use ix_support_tickets_customer_id; an old stored suggestion alone does not establish a current gap.
verify: Check whether the queries listed here use ix_support_tickets_customer_id and compare reads and execution time under equivalent parameters and load, including index storage and write cost.
caveat: Missing-index suggestions are per-query and ignore the cost of maintaining the index; review overlapping suggestions together."#,
    );
    // A disabled index with the keys.
    let mut s = asked(&["customer_id"], &[], &[], "[is_deleted] = 0");
    index(&mut s, "ix_support_tickets_customer_id").is_disabled = true;
    pin(
        "missing-index",
        &s,
        "missing-index:shop.support_tickets(customer_id)",
        r#"missing-index:shop.support_tickets(customer_id) · Medium · High · OptionalImprovement
title: support_tickets is searched by customer_id without a supporting index
summary: SQL Server's optimizer asked for an index on shop.support_tickets (customer_id) 400 times in the 10 days since the server last restarted. Without it, each of those lookups reads more of shop.support_tickets (300K rows) than it needs to. It estimates the index would make them about 95% cheaper. The query that asked for it accounts for 54% of database time.
impact: 54% of DB time
metric: Share of DB time: 0.5439766525721529
metric: Lookups that wanted it: 400
metric: Estimated improvement: 0.95
metric: Rows in table: 300000
fix: ix_support_tickets_customer_id has these keys, but it is disabled, so SQL Server neither maintains nor reads it. Rebuilding it brings it back as it was; if it was set aside because nothing read it, the queries below now would.
verify: After testing the rebuild of ix_support_tickets_customer_id, capture a newly compiled plan under representative load. A stored suggestion can remain in old plan XML.
verify: Check whether the queries listed here use ix_support_tickets_customer_id and compare reads and execution time under equivalent parameters and load, including index storage and write cost.
caveat: Missing-index suggestions are per-query and ignore the cost of maintaining the index; review overlapping suggestions together."#,
    );
    // An index one column short: widened with it.
    let mut s = asked(&["customer_id", "priority", "is_deleted"], &[], &["status"], "[is_deleted] = 0");
    ticket_index(&mut s, "ix_support_tickets_priority_customer", &["priority", "customer_id"], None, &[]);
    pin(
        "missing-index",
        &s,
        "missing-index:shop.support_tickets(customer_id,priority,is_deleted)",
        r#"missing-index:shop.support_tickets(customer_id,priority,is_deleted) · Medium · High · OptionalImprovement
title: support_tickets is searched by customer_id, priority and is_deleted without a supporting index
summary: SQL Server's optimizer asked for an index on shop.support_tickets (customer_id, priority and is_deleted) 400 times in the 10 days since the server last restarted. Without it, each of those lookups reads more of shop.support_tickets (300K rows) than it needs to. It estimates the index would make them about 95% cheaper. The query that asked for it accounts for 54% of database time.
impact: 54% of DB time
metric: Share of DB time: 0.5439766525721529
metric: Lookups that wanted it: 400
metric: Estimated improvement: 0.95
metric: Rows in table: 300000
fix: ix_support_tickets_priority_customer already finds these rows by priority and customer_id: all the request asks for but is_deleted. Widen it rather than add an index: with is_deleted and status included, it checks is_deleted on the rows it finds without going back to the table, and there is one index fewer to maintain. The statement keeps its keys, their order, its filter and the columns it already includes; restate DATA_COMPRESSION, FILLFACTOR or its partition scheme in it if the index uses them.
cost: The index grows from 4.3 MB; writes that touch the included columns maintain it.
verify: After testing the rebuild of ix_support_tickets_priority_customer, capture a newly compiled plan under representative load. A stored suggestion can remain in old plan XML.
verify: Check whether the queries listed here use ix_support_tickets_priority_customer and compare reads and execution time under equivalent parameters and load, including index storage and write cost.
caveat: Missing-index suggestions are per-query and ignore the cost of maintaining the index; review overlapping suggestions together."#,
    );
    // A filtered index that holds the rows the query asks for: widened, its filter kept.
    let mut s = asked(&["customer_id", "priority", "is_deleted"], &[], &["status"], "[is_deleted] = 0");
    ticket_index(&mut s, "ix_support_tickets_open", &["customer_id", "priority"], Some("([is_deleted]=(0))"), &[]);
    pin(
        "missing-index",
        &s,
        "missing-index:shop.support_tickets(customer_id,priority,is_deleted)",
        r#"missing-index:shop.support_tickets(customer_id,priority,is_deleted) · Medium · High · OptionalImprovement
title: support_tickets is searched by customer_id, priority and is_deleted without a supporting index
summary: SQL Server's optimizer asked for an index on shop.support_tickets (customer_id, priority and is_deleted) 400 times in the 10 days since the server last restarted. Without it, each of those lookups reads more of shop.support_tickets (300K rows) than it needs to. It estimates the index would make them about 95% cheaper. The query that asked for it accounts for 54% of database time.
impact: 54% of DB time
metric: Share of DB time: 0.5439766525721529
metric: Lookups that wanted it: 400
metric: Estimated improvement: 0.95
metric: Rows in table: 300000
fix: ix_support_tickets_open already finds these rows by customer_id and priority, in the rows where is_deleted = 0. Rebuild it with status included rather than add an index, so it covers the query below without going back to the table. The statement keeps its keys, their order, its filter and the columns it already includes; restate DATA_COMPRESSION, FILLFACTOR or its partition scheme in it if the index uses them.
cost: The index grows from 4.3 MB; writes that touch the included columns maintain it.
verify: After testing the rebuild of ix_support_tickets_open, capture a newly compiled plan under representative load. A stored suggestion can remain in old plan XML.
verify: Check whether the queries listed here use ix_support_tickets_open and compare reads and execution time under equivalent parameters and load, including index storage and write cost.
caveat: Missing-index suggestions are per-query and ignore the cost of maintaining the index; review overlapping suggestions together."#,
    );
    // A filtered index one column short of the rows it holds: widened with it.
    let mut s = asked(&["customer_id", "priority", "is_deleted", "kind"], &[], &["status"], "[is_deleted] = 0");
    ticket_index(&mut s, "ix_support_tickets_open", &["customer_id", "priority"], Some("([is_deleted]=(0))"), &[]);
    pin(
        "missing-index",
        &s,
        "missing-index:shop.support_tickets(customer_id,priority,is_deleted,kind)",
        r#"missing-index:shop.support_tickets(customer_id,priority,is_deleted,kind) · Medium · High · OptionalImprovement
title: support_tickets is searched by customer_id, priority, is_deleted and kind without a supporting index
summary: SQL Server's optimizer asked for an index on shop.support_tickets (customer_id, priority, is_deleted and kind) 400 times in the 10 days since the server last restarted. Without it, each of those lookups reads more of shop.support_tickets (300K rows) than it needs to. It estimates the index would make them about 95% cheaper. The query that asked for it accounts for 54% of database time.
impact: 54% of DB time
metric: Share of DB time: 0.5439766525721529
metric: Lookups that wanted it: 400
metric: Estimated improvement: 0.95
metric: Rows in table: 300000
fix: ix_support_tickets_open already finds these rows by customer_id and priority, in the rows where is_deleted = 0: all the request asks for but kind. Widen it rather than add an index: with kind and status included, it checks kind on the rows it finds without going back to the table, and there is one index fewer to maintain. The statement keeps its keys, their order, its filter and the columns it already includes; restate DATA_COMPRESSION, FILLFACTOR or its partition scheme in it if the index uses them.
cost: The index grows from 4.3 MB; writes that touch the included columns maintain it.
verify: After testing the rebuild of ix_support_tickets_open, capture a newly compiled plan under representative load. A stored suggestion can remain in old plan XML.
verify: Check whether the queries listed here use ix_support_tickets_open and compare reads and execution time under equivalent parameters and load, including index storage and write cost.
caveat: Missing-index suggestions are per-query and ignore the cost of maintaining the index; review overlapping suggestions together."#,
    );
}

// Requests an existing index serves only in part: a filtered one the queries don't ask for, and one with no key for
// the range.
#[test]
fn requests_an_existing_index_serves_in_part() {
    let mut s = asked(&["customer_id"], &["created_at"], &["status"], "[priority] = @par1");
    ticket_index(&mut s, "ix_support_tickets_open", &["customer_id", "created_at"], Some("([is_deleted]=(0))"), &[]);
    pin(
        "missing-index",
        &s,
        "missing-index:shop.support_tickets(customer_id,created_at)",
        r#"missing-index:shop.support_tickets(customer_id,created_at) · Medium · High · OptionalImprovement
title: support_tickets is searched by customer_id and created_at without a supporting index
summary: SQL Server's optimizer asked for an index on shop.support_tickets (customer_id and created_at) 400 times in the 10 days since the server last restarted. Without it, each of those lookups reads more of shop.support_tickets (300K rows) than it needs to. It estimates the index would make them about 95% cheaper. The query that asked for it accounts for 54% of database time.
impact: 54% of DB time
metric: Share of DB time: 0.5439766525721529
metric: Lookups that wanted it: 400
metric: Estimated improvement: 0.95
metric: Rows in table: 300000
fix: Create an index on customer_id and created_at, including status. ix_support_tickets_open has these keys but holds only the rows where is_deleted = 0, which the query below doesn't ask for.
cost: shop.support_tickets received no writes in the 10 days since the server last restarted, so this index would cost next to nothing to maintain.
verify: Test the candidate index with representative parameters and load, then capture a newly compiled plan. Old stored suggestions need not disappear.
verify: Check whether the queries listed here use the new index and compare reads and execution time under equivalent parameters and load, including index storage and write cost.
caveat: Missing-index suggestions are per-query and ignore the cost of maintaining the index; review overlapping suggestions together."#,
    );
    let mut q = s.queries().iter().find(|q| q.id == "4242").unwrap().clone();
    q.id = "4343".into();
    add(&mut s, q);
    pin(
        "missing-index",
        &s,
        "missing-index:shop.support_tickets(customer_id,created_at)",
        r#"missing-index:shop.support_tickets(customer_id,created_at) · Medium · High · OptionalImprovement
title: support_tickets is searched by customer_id and created_at without a supporting index
summary: SQL Server's optimizer asked for an index on shop.support_tickets (customer_id and created_at) 400 times in the 10 days since the server last restarted. Without it, each of those lookups reads more of shop.support_tickets (300K rows) than it needs to. It estimates the index would make them about 95% cheaper. The 2 queries that asked for it account for 70% of database time.
impact: 70% of DB time
metric: Share of DB time: 0.7046436248448801
metric: Lookups that wanted it: 400
metric: Estimated improvement: 0.95
metric: Rows in table: 300000
fix: Create an index on customer_id and created_at, including status. ix_support_tickets_open has these keys but holds only the rows where is_deleted = 0, which the queries below don't ask for.
cost: shop.support_tickets received no writes in the 10 days since the server last restarted, so this index would cost next to nothing to maintain.
verify: Test the candidate index with representative parameters and load, then capture a newly compiled plan. Old stored suggestions need not disappear.
verify: Check whether the queries listed here use the new index and compare reads and execution time under equivalent parameters and load, including index storage and write cost.
caveat: Missing-index suggestions are per-query and ignore the cost of maintaining the index; review overlapping suggestions together."#,
    );
    s.workload.as_mut().unwrap().queries.retain(|q| q.id != "4242" && q.id != "4343");
    pin(
        "missing-index",
        &s,
        "missing-index:shop.support_tickets(customer_id,created_at)",
        r#"missing-index:shop.support_tickets(customer_id,created_at) · Low · Low · OptionalImprovement
title: support_tickets is searched by customer_id and created_at without a supporting index
summary: SQL Server's optimizer asked for an index on shop.support_tickets (customer_id and created_at) 400 times in the 10 days since the server last restarted. Without it, each of those lookups reads more of shop.support_tickets (300K rows) than it needs to. It estimates the index would make them about 95% cheaper. None of the queries in this scan asked for it: the requests came from plans that ran before the scan's window, or that Query Store no longer keeps.
impact: 400 lookups
metric: Lookups that wanted it: 400
metric: Estimated improvement: 0.95
metric: Rows in table: 300000
fix: Create an index on customer_id and created_at, including status. ix_support_tickets_open has these keys but holds only the rows where is_deleted = 0, and Culprit can't show that these lookups ask for only those.
cost: shop.support_tickets received no writes in the 10 days since the server last restarted, so this index would cost next to nothing to maintain.
verify: Test the candidate index with representative parameters and load, then capture a newly compiled plan. Old stored suggestions need not disappear.
caveat: Missing-index suggestions are per-query and ignore the cost of maintaining the index; review overlapping suggestions together."#,
    );
    let mut s = asked(&["customer_id", "is_deleted"], &["opened_at"], &[], "[is_deleted] = 0");
    ticket_index(&mut s, "ix_support_tickets_open", &["customer_id"], Some("([is_deleted]=(0))"), &[]);
    pin(
        "missing-index",
        &s,
        "missing-index:shop.support_tickets(customer_id,is_deleted,opened_at)",
        r#"missing-index:shop.support_tickets(customer_id,is_deleted,opened_at) · Medium · High · OptionalImprovement
title: support_tickets is searched by customer_id, is_deleted and opened_at without a supporting index
summary: SQL Server's optimizer asked for an index on shop.support_tickets (customer_id, is_deleted and opened_at) 400 times in the 10 days since the server last restarted. Without it, each of those lookups reads more of shop.support_tickets (300K rows) than it needs to. It estimates the index would make them about 95% cheaper. The query that asked for it accounts for 54% of database time.
impact: 54% of DB time
metric: Share of DB time: 0.5439766525721529
metric: Lookups that wanted it: 400
metric: Estimated improvement: 0.95
metric: Rows in table: 300000
fix: Create an index on customer_id, is_deleted and opened_at. Equality columns come first; put the most selective one at the front if you know it. ix_support_tickets_open already finds these rows by customer_id, in the rows where is_deleted = 0, but has no key for opened_at: this index seeks the range as well.
cost: shop.support_tickets received no writes in the 10 days since the server last restarted, so this index would cost next to nothing to maintain.
verify: Test the candidate index with representative parameters and load, then capture a newly compiled plan. Old stored suggestions need not disappear.
verify: Check whether the queries listed here use the new index and compare reads and execution time under equivalent parameters and load, including index storage and write cost.
caveat: Missing-index suggestions are per-query and ignore the cost of maintaining the index; review overlapping suggestions together."#,
    );
    let mut s = asked(&["customer_id", "is_deleted"], &["opened_at"], &[], "[is_deleted] = 0");
    ticket_index(&mut s, "ix_support_tickets_customer_deleted", &["customer_id", "is_deleted"], None, &[]);
    pin(
        "missing-index",
        &s,
        "missing-index:shop.support_tickets(customer_id,is_deleted,opened_at)",
        r#"missing-index:shop.support_tickets(customer_id,is_deleted,opened_at) · Medium · High · OptionalImprovement
title: support_tickets is searched by customer_id, is_deleted and opened_at without a supporting index
summary: SQL Server's optimizer asked for an index on shop.support_tickets (customer_id, is_deleted and opened_at) 400 times in the 10 days since the server last restarted. Without it, each of those lookups reads more of shop.support_tickets (300K rows) than it needs to. It estimates the index would make them about 95% cheaper. The query that asked for it accounts for 54% of database time.
impact: 54% of DB time
metric: Share of DB time: 0.5439766525721529
metric: Lookups that wanted it: 400
metric: Estimated improvement: 0.95
metric: Rows in table: 300000
fix: Create an index on customer_id, is_deleted and opened_at. Equality columns come first; put the most selective one at the front if you know it. ix_support_tickets_customer_deleted already finds these rows by customer_id and is_deleted, but has no key for opened_at: this index seeks the range as well.
cost: shop.support_tickets received no writes in the 10 days since the server last restarted, so this index would cost next to nothing to maintain.
verify: Test the candidate index with representative parameters and load, then capture a newly compiled plan. Old stored suggestions need not disappear.
verify: Check whether the queries listed here use the new index and compare reads and execution time under equivalent parameters and load, including index storage and write cost.
caveat: Missing-index suggestions are per-query and ignore the cost of maintaining the index; review overlapping suggestions together."#,
    );
}

// A filtered index missed because the queries pass its column as a parameter.
#[test]
fn requests_a_filtered_index_serves_with_a_literal() {
    let mut s = asked(&["customer_id", "is_deleted"], &[], &[], "[is_deleted] = @par1");
    ticket_index(&mut s, "ix_support_tickets_open", &["customer_id", "created_at"], Some("([is_deleted]=(0))"), &[]);
    pin(
        "missing-index",
        &s,
        "missing-index:shop.support_tickets(customer_id,is_deleted)",
        r#"missing-index:shop.support_tickets(customer_id,is_deleted) · Low · Medium · OptionalImprovement
title: A query on support_tickets passes is_deleted as a parameter, so ix_support_tickets_open can't serve it
summary: SQL Server's optimizer asked for an index on shop.support_tickets (customer_id and is_deleted) 400 times in the 10 days since the server last restarted, but ix_support_tickets_open (customer_id and created_at) already serves these lookups where is_deleted = 0. It's a filtered index, and SQL Server uses one only for a query whose filter matches it as written: the query below compares is_deleted to a parameter, which could hold any value, so the optimizer can't use the index and asks for a new one instead.
impact: 400 lookups
metric: Lookups that wanted it: 400
metric: Estimated improvement: 0.95
fix: Write the filter as the index does, is_deleted = 0, instead of a parameter in the query below: ix_support_tickets_open then serves it, and no new index is needed.
verify: Change the query, let the workload run, then re-scan: this request disappears, and the query's plan seeks ix_support_tickets_open.
caveat: If the query also runs with other values for is_deleted, those runs can't use ix_support_tickets_open: give them a statement of their own, with their own literal."#,
    );
    let mut s = asked(&["customer_id", "is_deleted"], &[], &["status", "subject"], "[is_deleted] = @par1");
    ticket_index(&mut s, "ix_support_tickets_open", &["customer_id", "created_at"], Some("([is_deleted]=(0))"), &[]);
    let mut q = s.queries().iter().find(|q| q.id == "4242").unwrap().clone();
    q.id = "4343".into();
    add(&mut s, q);
    pin(
        "missing-index",
        &s,
        "missing-index:shop.support_tickets(customer_id,is_deleted)",
        r#"missing-index:shop.support_tickets(customer_id,is_deleted) · Low · Medium · OptionalImprovement
title: 2 queries on support_tickets pass is_deleted as a parameter, so ix_support_tickets_open can't serve them
summary: SQL Server's optimizer asked for an index on shop.support_tickets (customer_id and is_deleted) 400 times in the 10 days since the server last restarted, but ix_support_tickets_open (customer_id and created_at) already serves these lookups where is_deleted = 0. It's a filtered index, and SQL Server uses one only for a query whose filter matches it as written: the queries below compare is_deleted to a parameter, which could hold any value, so the optimizer can't use the index and asks for a new one instead.
impact: 400 lookups
metric: Lookups that wanted it: 400
metric: Estimated improvement: 0.95
fix: Write the filter as the index does, is_deleted = 0, instead of a parameter in the queries below: ix_support_tickets_open then serves them. If reading status and subject from the table for each row still costs too much after that, include them in ix_support_tickets_open (below: the rebuild keeps its filter).
cost: shop.support_tickets received no writes in the 10 days since the server last restarted, so this index would cost next to nothing to maintain.
verify: Change the query, let the workload run, then re-scan: the query's plan seeks ix_support_tickets_open, and if the optimizer still asks for an index, it's the one above.
caveat: If the queries also run with other values for is_deleted, those runs can't use ix_support_tickets_open: give them a statement of their own, with their own literal."#,
    );
    let mut s = asked(&["customer_id", "priority", "is_deleted"], &["due_at"], &[], "[is_deleted] = @par1");
    ticket_index(&mut s, "ix_support_tickets_open", &["priority", "customer_id"], Some("([is_deleted]=(0))"), &[]);
    pin(
        "missing-index",
        &s,
        "missing-index:shop.support_tickets(customer_id,priority,is_deleted,due_at)",
        r#"missing-index:shop.support_tickets(customer_id,priority,is_deleted,due_at) · Low · Low · OptionalImprovement
title: A query on support_tickets passes is_deleted as a parameter, so ix_support_tickets_open can't serve it
summary: SQL Server's optimizer asked for an index on shop.support_tickets (customer_id, priority, is_deleted and due_at) 400 times in the 10 days since the server last restarted, but ix_support_tickets_open (priority and customer_id) already finds these rows where is_deleted = 0 by priority and customer_id, though not by due_at. It's a filtered index, and SQL Server uses one only for a query whose filter matches it as written: the query below compares is_deleted to a parameter, which could hold any value, so the optimizer can't use the index and asks for a new one instead.
impact: 400 lookups
metric: Lookups that wanted it: 400
metric: Estimated improvement: 0.95
fix: Write the filter as the index does, is_deleted = 0, instead of a parameter in the query below: ix_support_tickets_open then finds its rows by priority and customer_id. If the range on due_at still costs too much after that, create the index below: the request without is_deleted, filtered the same way, so it holds only the rows where is_deleted = 0. It starts with ix_support_tickets_open's keys, so once it's in use ix_support_tickets_open can go.
cost: shop.support_tickets received no writes in the 10 days since the server last restarted, so this index would cost next to nothing to maintain.
verify: Change the query, let the workload run, then re-scan: the query's plan seeks ix_support_tickets_open, and if the optimizer still asks for an index, it's the one above.
caveat: If the query also runs with other values for is_deleted, those runs can't use ix_support_tickets_open: give them a statement of their own, with their own literal."#,
    );
    let mut s = asked(&["customer_id", "priority", "is_deleted"], &["due_at"], &[], "[is_deleted] = @par1");
    ticket_index(
        &mut s,
        "ix_support_tickets_open",
        &["priority", "customer_id", "kind"],
        Some("([is_deleted]=(0))"),
        &[],
    );
    pin(
        "missing-index",
        &s,
        "missing-index:shop.support_tickets(customer_id,priority,is_deleted,due_at)",
        r#"missing-index:shop.support_tickets(customer_id,priority,is_deleted,due_at) · Low · Low · OptionalImprovement
title: A query on support_tickets passes is_deleted as a parameter, so ix_support_tickets_open can't serve it
summary: SQL Server's optimizer asked for an index on shop.support_tickets (customer_id, priority, is_deleted and due_at) 400 times in the 10 days since the server last restarted, but ix_support_tickets_open (priority, customer_id and kind) already finds these rows where is_deleted = 0 by priority and customer_id, though not by due_at. It's a filtered index, and SQL Server uses one only for a query whose filter matches it as written: the query below compares is_deleted to a parameter, which could hold any value, so the optimizer can't use the index and asks for a new one instead.
impact: 400 lookups
metric: Lookups that wanted it: 400
metric: Estimated improvement: 0.95
fix: Write the filter as the index does, is_deleted = 0, instead of a parameter in the query below: ix_support_tickets_open then finds its rows by priority and customer_id. If the range on due_at still costs too much after that, create the index below: the request without is_deleted, filtered the same way, so it holds only the rows where is_deleted = 0.
cost: shop.support_tickets received no writes in the 10 days since the server last restarted, so this index would cost next to nothing to maintain.
verify: Change the query, let the workload run, then re-scan: the query's plan seeks ix_support_tickets_open, and if the optimizer still asks for an index, it's the one above.
caveat: If the query also runs with other values for is_deleted, those runs can't use ix_support_tickets_open: give them a statement of their own, with their own literal."#,
    );
}

// Requests on a table too small for an index to pay.
#[test]
fn requests_on_a_small_table() {
    let small = |rows: f64, index: bool| {
        let mut s = asked(&["customer_id", "is_deleted"], &["opened_at"], &[], "[is_deleted] = 0");
        if index {
            ticket_index(&mut s, "ix_support_tickets_open", &["customer_id"], Some("([is_deleted]=(0))"), &[]);
            ticket_index(&mut s, "ix_support_tickets_by_customer", &["customer_id", "is_deleted"], None, &[]);
        }
        table(&mut s, "support_tickets").rows = Some(rows);
        query(&mut s, "4242").total_time_ms = 27.0;
        s
    };
    pin(
        "missing-index",
        &small(160.0, false),
        "missing-index:shop.support_tickets(customer_id,is_deleted,opened_at)",
        r#"missing-index:shop.support_tickets(customer_id,is_deleted,opened_at) · Low · Medium · Information
title: SQL Server asks for an index on support_tickets, which is too small for one to pay off
summary: SQL Server's optimizer asked for an index on shop.support_tickets (customer_id, is_deleted and opened_at) 400 times in the 10 days since the server last restarted. Without it, each of those lookups reads more of shop.support_tickets (160 rows) than it needs to. It estimates the index would make them about 95% cheaper. The query that asked for it accounts for <1% of database time.
impact: <1% of DB time
metric: Share of DB time: 0.0006626321856110306
metric: Lookups that wanted it: 400
metric: Estimated improvement: 0.95
metric: Rows in table: 160
fix: Nothing to create while shop.support_tickets stays this small: it holds 160 rows, and the query that asked for it took 27 ms in the last 5 hours, so reading it whole costs little, and an index on customer_id, is_deleted and opened_at would add to every one of its writes. Create one only if the table grows past 10K rows or these lookups come to take real time.
verify: Nothing to check while it stays this small: once shop.support_tickets holds 10K rows or more, a scan says whether the index is worth creating.
caveat: Missing-index suggestions are per-query and ignore the cost of maintaining the index; review overlapping suggestions together."#,
    );
    pin(
        "missing-index",
        &small(1_200.0, true),
        "missing-index:shop.support_tickets(customer_id,is_deleted,opened_at)",
        r#"missing-index:shop.support_tickets(customer_id,is_deleted,opened_at) · Low · Medium · Information
title: SQL Server asks for an index on support_tickets, which is too small for one to pay off
summary: SQL Server's optimizer asked for an index on shop.support_tickets (customer_id, is_deleted and opened_at) 400 times in the 10 days since the server last restarted. Without it, each of those lookups reads more of shop.support_tickets (1.2K rows) than it needs to. It estimates the index would make them about 95% cheaper. The query that asked for it accounts for <1% of database time.
impact: <1% of DB time
metric: Share of DB time: 0.0006626321856110306
metric: Lookups that wanted it: 400
metric: Estimated improvement: 0.95
metric: Rows in table: 1200
fix: Nothing to create while shop.support_tickets stays this small: it holds 1.2K rows, and the query that asked for it took 27 ms in the last 5 hours, so reading it whole costs little, and an index on customer_id, is_deleted and opened_at would add to every one of its writes. ix_support_tickets_by_customer already leads with customer_id and is_deleted. Create one only if the table grows past 10K rows or these lookups come to take real time.
verify: Nothing to check while it stays this small: once shop.support_tickets holds 10K rows or more, a scan says whether the index is worth creating.
caveat: Missing-index suggestions are per-query and ignore the cost of maintaining the index; review overlapping suggestions together."#,
    );
    let mut s = small(1_200.0, true);
    s.indexes.retain(|i| i.name != "ix_support_tickets_by_customer");
    pin(
        "missing-index",
        &s,
        "missing-index:shop.support_tickets(customer_id,is_deleted,opened_at)",
        r#"missing-index:shop.support_tickets(customer_id,is_deleted,opened_at) · Low · Medium · Information
title: SQL Server asks for an index on support_tickets, which is too small for one to pay off
summary: SQL Server's optimizer asked for an index on shop.support_tickets (customer_id, is_deleted and opened_at) 400 times in the 10 days since the server last restarted. Without it, each of those lookups reads more of shop.support_tickets (1.2K rows) than it needs to. It estimates the index would make them about 95% cheaper. The query that asked for it accounts for <1% of database time.
impact: <1% of DB time
metric: Share of DB time: 0.0006626321856110306
metric: Lookups that wanted it: 400
metric: Estimated improvement: 0.95
metric: Rows in table: 1200
fix: Nothing to create while shop.support_tickets stays this small: it holds 1.2K rows, and the query that asked for it took 27 ms in the last 5 hours, so reading it whole costs little, and an index on customer_id, is_deleted and opened_at would add to every one of its writes. ix_support_tickets_open already leads with customer_id, for the rows where is_deleted = 0. Create one only if the table grows past 10K rows or these lookups come to take real time.
verify: Nothing to check while it stays this small: once shop.support_tickets holds 10K rows or more, a scan says whether the index is worth creating.
caveat: Missing-index suggestions are per-query and ignore the cost of maintaining the index; review overlapping suggestions together."#,
    );
    // The statement that asked stopped running long ago, and so did the requests.
    let mut s = small(160.0, false);
    query(&mut s, "4242").last_seen = Some("2026-08-01T09:00:00Z".into());
    s.missing_indexes.last_mut().unwrap().last_used = Some("2026-08-01T09:00:00Z".into());
    index(&mut s, "pk_support_tickets").writes = Some(50_000.0);
    pin(
        "missing-index",
        &s,
        "missing-index:shop.support_tickets(customer_id,is_deleted,opened_at)",
        r#"missing-index:shop.support_tickets(customer_id,is_deleted,opened_at) · Low · Medium · Information
title: SQL Server asked for an index on support_tickets, which is too small for one to pay off
summary: SQL Server's optimizer asked for an index on shop.support_tickets (customer_id, is_deleted and opened_at) 400 times in the 10 days since the server last restarted. Without it, each of those lookups read more of shop.support_tickets (160 rows) than it needed to. It estimated the index would make them about 95% cheaper. The query that asked for it accounted for <1% of database time.
impact: <1% of DB time
metric: Share of DB time: 0.0006626321856110306
metric: Lookups that wanted it: 400
metric: Estimated improvement: 0.95
metric: Rows in table: 160
fix: Nothing to create while shop.support_tickets stays this small: it holds 160 rows, and the query that asked for it took 27 ms in the last 5 hours, so reading it whole costs little, and an index on customer_id, is_deleted and opened_at would add to every one of its writes (about 5.0K writes a day). Reconsider it only if the statements return and the optimizer asks again, and the table grows past 10K rows or those lookups come to take real time.
verify: Nothing to check while it stays this small: once shop.support_tickets holds 10K rows or more, a scan says whether the index is worth creating.
caveat: SQL Server's optimizer last asked for this index on Aug 1, 71.1 days before this scan's data ends: the queries that wanted it have most likely changed or stopped running. Its requests are kept until the server restarts.
caveat: Missing-index suggestions are per-query and ignore the cost of maintaining the index; review overlapping suggestions together."#,
    );
    let mut s = small(160.0, false);
    s.missing_indexes.last_mut().unwrap().last_used = Some("2026-08-01T09:00:00Z".into());
    s.workload.as_mut().unwrap().queries.retain(|q| q.id != "4242");
    pin(
        "missing-index",
        &s,
        "missing-index:shop.support_tickets(customer_id,is_deleted,opened_at)",
        r#"missing-index:shop.support_tickets(customer_id,is_deleted,opened_at) · Low · Medium · Information
title: SQL Server asks for an index on support_tickets, which is too small for one to pay off
summary: SQL Server's optimizer asked for an index on shop.support_tickets (customer_id, is_deleted and opened_at) 400 times in the 10 days since the server last restarted. Without it, each of those lookups read more of shop.support_tickets (160 rows) than it needed to. It estimated the index would make them about 95% cheaper. None of the queries in this scan asked for it: the requests came from plans that ran before the scan's window, or that Query Store no longer keeps.
impact: 400 lookups
metric: Lookups that wanted it: 400
metric: Estimated improvement: 0.95
metric: Rows in table: 160
fix: Nothing to create while shop.support_tickets stays this small: it holds 160 rows, so reading it whole costs little, and an index on customer_id, is_deleted and opened_at would add to every one of its writes. Create one only if the table grows past 10K rows or these lookups come to take real time.
verify: Nothing to check while it stays this small: once shop.support_tickets holds 10K rows or more, a scan says whether the index is worth creating.
caveat: SQL Server's optimizer last asked for this index on Aug 1, 71.1 days before this scan's data ends: the queries that wanted it have most likely changed or stopped running. Its requests are kept until the server restarts.
caveat: Missing-index suggestions are per-query and ignore the cost of maintaining the index; review overlapping suggestions together."#,
    );
}

// Requests last made before the window: long before (history), and hours before.
#[test]
fn requests_made_before_the_window() {
    let mut s = snapshot("mssql-demo.json");
    s.missing_indexes.iter_mut().find(|m| m.table == "order_items").unwrap().last_used =
        Some("2026-08-01T09:00:00Z".into());
    pin(
        "missing-index",
        &s,
        "missing-index:shop.order_items(order_id)",
        r#"missing-index:shop.order_items(order_id) · Low · High · History
title: SQL Server previously requested an index on order_items (order_id)
summary: SQL Server last requested this index on Aug 1 at 09:00 UTC, before the last 5 hours. The 300 requests are cumulative in the minute since the server last restarted. Associated statements may still run, but their activity does not establish that this historical request is still needed in the selected period.
impact: Historical optimizer request
metric: Lookups that wanted it: 300
metric: Estimated improvement: 0.9947
metric: Rows in table: 1500000
query 9: Associated with a request made before the selected window; current query activity does not date that request.
fix: Keep this candidate as context. Check representative plans and reads in the period that matters before choosing an index. If the optimizer requests it again, compare the measured benefit with index storage and write maintenance.
verify: A later scan must show a new request inside its workload window and enough measured work before this becomes an index recommendation.
caveat: SQL Server's optimizer last asked for this index on Aug 1, 71.1 days before this scan's data ends: the queries that wanted it have most likely changed or stopped running. Its requests are kept until the server restarts."#,
    );
    s.missing_indexes.iter_mut().find(|m| m.table == "order_items").unwrap().last_used =
        Some("2026-10-11T06:00:00Z".into());
    pin(
        "missing-index",
        &s,
        "missing-index:shop.order_items(order_id)",
        r#"missing-index:shop.order_items(order_id) · Low · High · Information
title: SQL Server previously requested an index on order_items (order_id)
summary: SQL Server last requested this index on Oct 11 at 06:00 UTC, before the last 5 hours. The 300 requests are cumulative in the minute since the server last restarted. Associated statements may still run, but their activity does not establish that this historical request is still needed in the selected period.
impact: Historical optimizer request
metric: Lookups that wanted it: 300
metric: Estimated improvement: 0.9947
metric: Rows in table: 1500000
query 9: Associated with a request made before the selected window; current query activity does not date that request.
fix: Keep this candidate as context. Check representative plans and reads in the period that matters before choosing an index. If the optimizer requests it again, compare the measured benefit with index storage and write maintenance.
verify: A later scan must show a new request inside its workload window and enough measured work before this becomes an index recommendation.
caveat: None of the last 5 hours' statements asked for it: SQL Server's optimizer last did on Oct 11 at 06:00 UTC, 6 h 14 min before this scan's data ends, too short a time to say the statements that wanted it stopped."#,
    );
    let mut q = s.workload.as_mut().unwrap().queries.iter().find(|q| q.id == "9").unwrap().clone();
    s.workload.as_mut().unwrap().queries.retain(|q| q.id != "9");
    pin(
        "missing-index",
        &s,
        "missing-index:shop.order_items(order_id)",
        r#"missing-index:shop.order_items(order_id) · Low · Low · Information
title: SQL Server previously requested an index on order_items (order_id)
summary: SQL Server last requested this index on Oct 11 at 06:00 UTC, before the last 5 hours. The 300 requests are cumulative in the minute since the server last restarted. Associated statements may still run, but their activity does not establish that this historical request is still needed in the selected period.
impact: Historical optimizer request
metric: Lookups that wanted it: 300
metric: Estimated improvement: 0.9947
metric: Rows in table: 1500000
fix: Keep this candidate as context. Check representative plans and reads in the period that matters before choosing an index. If the optimizer requests it again, compare the measured benefit with index storage and write maintenance.
verify: A later scan must show a new request inside its workload window and enough measured work before this becomes an index recommendation."#,
    );
    q.last_seen = Some("2026-08-01T09:00:00Z".into());
    s.missing_indexes.iter_mut().find(|m| m.table == "order_items").unwrap().last_used =
        Some("2026-08-01T09:00:00Z".into());
    add(&mut s, q);
    pin(
        "missing-index",
        &s,
        "missing-index:shop.order_items(order_id)",
        r#"missing-index:shop.order_items(order_id) · Low · High · History
title: order_items was searched by order_id without a supporting index
summary: SQL Server's optimizer asked for an index on shop.order_items (order_id) 300 times in the minute since the server last restarted. Without it, each of those lookups read more of shop.order_items (1.5M rows) than it needed to. It estimated the index would make them about 99% cheaper. The query that asked for it accounted for 14% of database time.
impact: 14% of DB time
metric: Share of DB time: 0.14198168631293098
metric: Lookups that wanted it: 300
metric: Estimated improvement: 0.9947
metric: Rows in table: 1500000
fix: Nothing to create unless the statements that wanted it come back: they have most likely changed or stopped running. If they come back, the optimizer asks for it again, and a scan then shows them and the index to create.
verify: Nothing to check unless they come back: if the optimizer asks for this index again within a scan's window, this finding names the queries behind it and gives the index to create.
caveat: SQL Server's optimizer last asked for this index on Aug 1, 71.1 days before this scan's data ends: the queries that wanted it have most likely changed or stopped running. Its requests are kept until the server restarts."#,
    );
    // Over ten days of counters, with the writes an index would add to.
    let mut s = ten_days(s);
    index(&mut s, "pk_order_items").writes = Some(50_000.0);
    pin(
        "missing-index",
        &s,
        "missing-index:shop.order_items(order_id)",
        r#"missing-index:shop.order_items(order_id) · Low · High · History
title: order_items was searched by order_id without a supporting index
summary: SQL Server's optimizer asked for an index on shop.order_items (order_id) 300 times in the 10 days since the server last restarted. Without it, each of those lookups read more of shop.order_items (1.5M rows) than it needed to. It estimated the index would make them about 99% cheaper. The query that asked for it accounted for 14% of database time.
impact: 14% of DB time
metric: Share of DB time: 0.14198168631293098
metric: Lookups that wanted it: 300
metric: Estimated improvement: 0.9947
metric: Rows in table: 1500000
fix: Nothing to create unless the statements that wanted it come back: they have most likely changed or stopped running, and an index on order_id would add to every write on shop.order_items (about 5.0K writes a day). If they come back, the optimizer asks for it again, and a scan then shows them and the index to create.
verify: Nothing to check unless they come back: if the optimizer asks for this index again within a scan's window, this finding names the queries behind it and gives the index to create.
caveat: SQL Server's optimizer last asked for this index on Aug 1, 71.1 days before this scan's data ends: the queries that wanted it have most likely changed or stopped running. Its requests are kept until the server restarts."#,
    );
    let mut s = snapshot("mssql-demo.json");
    s.missing_indexes.iter_mut().find(|m| m.table == "order_items").unwrap().last_used =
        Some("2026-08-01T09:00:00Z".into());
    s.workload = None;
    pin(
        "missing-index",
        &s,
        "missing-index:shop.order_items(order_id)",
        r#"missing-index:shop.order_items(order_id) · Low · Low · History
title: SQL Server previously requested an index on order_items (order_id)
summary: SQL Server last requested this index on Aug 1 at 09:00 UTC, before the last week. The 300 requests are cumulative in the minute since the server last restarted. Associated statements may still run, but their activity does not establish that this historical request is still needed in the selected period.
impact: Historical optimizer request
metric: Lookups that wanted it: 300
metric: Estimated improvement: 0.9947
metric: Rows in table: 1500000
fix: Keep this candidate as context. Check representative plans and reads in the period that matters before choosing an index. If the optimizer requests it again, compare the measured benefit with index storage and write maintenance.
verify: A later scan must show a new request inside its workload window and enough measured work before this becomes an index recommendation.
caveat: SQL Server's optimizer last asked for this index on Aug 1, 71.1 days before this scan's data ends: the queries that wanted it have most likely changed or stopped running. Its requests are kept until the server restarts."#,
    );
}

// More shapes ------------------------------------------------------------------------------------------------------

/// A copy of query `id` on `s`, as `new_id`.
fn another(s: &mut Snapshot, id: &str, new_id: &str) {
    let mut q = s.queries().iter().find(|q| q.id == id).expect("a query").clone();
    q.id = new_id.into();
    add(s, q);
}

// One large column left out of the INCLUDE list of a table that keeps large values apart from its rows.
#[test]
fn a_request_including_one_large_column() {
    let mut s = asked(&["customer_id", "is_deleted"], &[], &["status", "body"], "[is_deleted] = 0");
    s.columns.push(column("support_tickets", "body", "nvarchar(max)", None));
    table(&mut s, "support_tickets").lob_bytes = Some(5.0e9);
    pin(
        "missing-index",
        &s,
        "missing-index:shop.support_tickets(customer_id,is_deleted)",
        r#"missing-index:shop.support_tickets(customer_id,is_deleted) · Medium · High · OptionalImprovement
title: support_tickets is searched by customer_id and is_deleted without a supporting index
summary: SQL Server's optimizer asked for an index on shop.support_tickets (customer_id and is_deleted) 400 times in the 10 days since the server last restarted. Without it, each of those lookups reads more of shop.support_tickets (300K rows) than it needs to. It estimates the index would make them about 95% cheaper. The query that asked for it accounts for 54% of database time.
impact: 54% of DB time
metric: Share of DB time: 0.5439766525721529
metric: Lookups that wanted it: 400
metric: Estimated improvement: 0.95
metric: Rows in table: 300000
fix: Create an index on customer_id and is_deleted, including status. Equality columns come first; put the most selective one at the front if you know it.
cost: shop.support_tickets received no writes in the 10 days since the server last restarted, so this index would cost next to nothing to maintain.
verify: Test the candidate index with representative parameters and load, then capture a newly compiled plan. Old stored suggestions need not disappear.
verify: Check whether the queries listed here use the new index and compare reads and execution time under equivalent parameters and load, including index storage and write cost.
caveat: Left body out of INCLUDE: its type can hold large values, and shop.support_tickets keeps 4.7 GB of large values apart from its rows. Including it could copy that data into the index; the rows the index finds go back to the table for it, which costs less.
caveat: Missing-index suggestions are per-query and ignore the cost of maintaining the index; review overlapping suggestions together."#,
    );
}

// A filtered index that leads with the request's columns, which its query doesn't fit.
#[test]
fn a_request_led_by_a_filtered_index_it_doesnt_fit() {
    let mut s = asked(&["customer_id", "priority", "kind"], &[], &["status"], "[priority] = @par1");
    ticket_index(&mut s, "ix_support_tickets_open", &["customer_id", "priority"], Some("([is_deleted]=(0))"), &[]);
    pin(
        "missing-index",
        &s,
        "missing-index:shop.support_tickets(customer_id,priority,kind)",
        r#"missing-index:shop.support_tickets(customer_id,priority,kind) · Medium · High · OptionalImprovement
title: support_tickets is searched by customer_id, priority and kind without a supporting index
summary: SQL Server's optimizer asked for an index on shop.support_tickets (customer_id, priority and kind) 400 times in the 10 days since the server last restarted. Without it, each of those lookups reads more of shop.support_tickets (300K rows) than it needs to. It estimates the index would make them about 95% cheaper. The query that asked for it accounts for 54% of database time.
impact: 54% of DB time
metric: Share of DB time: 0.5439766525721529
metric: Lookups that wanted it: 400
metric: Estimated improvement: 0.95
metric: Rows in table: 300000
fix: Create an index on customer_id, priority and kind, including status. Equality columns come first; put the most selective one at the front if you know it. ix_support_tickets_open leads with customer_id and priority but holds only the rows where is_deleted = 0, which the query below doesn't ask for.
cost: shop.support_tickets received no writes in the 10 days since the server last restarted, so this index would cost next to nothing to maintain.
verify: Test the candidate index with representative parameters and load, then capture a newly compiled plan. Old stored suggestions need not disappear.
verify: Check whether the queries listed here use the new index and compare reads and execution time under equivalent parameters and load, including index storage and write cost.
caveat: Missing-index suggestions are per-query and ignore the cost of maintaining the index; review overlapping suggestions together."#,
    );
}

// The literal, then one column to include, for one query; the literal alone, and the literal then an index, for two.
#[test]
fn literal_filters_for_one_query_and_for_two() {
    let mut s = asked(&["customer_id", "is_deleted"], &[], &["status"], "[is_deleted] = @par1");
    ticket_index(&mut s, "ix_support_tickets_open", &["customer_id", "created_at"], Some("([is_deleted]=(0))"), &[]);
    pin(
        "missing-index",
        &s,
        "missing-index:shop.support_tickets(customer_id,is_deleted)",
        r#"missing-index:shop.support_tickets(customer_id,is_deleted) · Low · Medium · OptionalImprovement
title: A query on support_tickets passes is_deleted as a parameter, so ix_support_tickets_open can't serve it
summary: SQL Server's optimizer asked for an index on shop.support_tickets (customer_id and is_deleted) 400 times in the 10 days since the server last restarted, but ix_support_tickets_open (customer_id and created_at) already serves these lookups where is_deleted = 0. It's a filtered index, and SQL Server uses one only for a query whose filter matches it as written: the query below compares is_deleted to a parameter, which could hold any value, so the optimizer can't use the index and asks for a new one instead.
impact: 400 lookups
metric: Lookups that wanted it: 400
metric: Estimated improvement: 0.95
fix: Write the filter as the index does, is_deleted = 0, instead of a parameter in the query below: ix_support_tickets_open then serves it. If reading status from the table for each row still costs too much after that, include it in ix_support_tickets_open (below: the rebuild keeps its filter).
cost: shop.support_tickets received no writes in the 10 days since the server last restarted, so this index would cost next to nothing to maintain.
verify: Change the query, let the workload run, then re-scan: the query's plan seeks ix_support_tickets_open, and if the optimizer still asks for an index, it's the one above.
caveat: If the query also runs with other values for is_deleted, those runs can't use ix_support_tickets_open: give them a statement of their own, with their own literal."#,
    );
    let mut s = asked(&["customer_id", "is_deleted"], &[], &[], "[is_deleted] = @par1");
    ticket_index(&mut s, "ix_support_tickets_open", &["customer_id", "created_at"], Some("([is_deleted]=(0))"), &[]);
    another(&mut s, "4242", "4343");
    pin(
        "missing-index",
        &s,
        "missing-index:shop.support_tickets(customer_id,is_deleted)",
        r#"missing-index:shop.support_tickets(customer_id,is_deleted) · Low · Medium · OptionalImprovement
title: 2 queries on support_tickets pass is_deleted as a parameter, so ix_support_tickets_open can't serve them
summary: SQL Server's optimizer asked for an index on shop.support_tickets (customer_id and is_deleted) 400 times in the 10 days since the server last restarted, but ix_support_tickets_open (customer_id and created_at) already serves these lookups where is_deleted = 0. It's a filtered index, and SQL Server uses one only for a query whose filter matches it as written: the queries below compare is_deleted to a parameter, which could hold any value, so the optimizer can't use the index and asks for a new one instead.
impact: 400 lookups
metric: Lookups that wanted it: 400
metric: Estimated improvement: 0.95
fix: Write the filter as the index does, is_deleted = 0, instead of a parameter in the queries below: ix_support_tickets_open then serves them, and no new index is needed.
verify: Change the query, let the workload run, then re-scan: this request disappears, and the query's plan seeks ix_support_tickets_open.
caveat: If the queries also run with other values for is_deleted, those runs can't use ix_support_tickets_open: give them a statement of their own, with their own literal."#,
    );
    let mut s = asked(&["customer_id", "priority", "is_deleted"], &["due_at"], &[], "[is_deleted] = @par1");
    ticket_index(&mut s, "ix_support_tickets_open", &["priority", "customer_id"], Some("([is_deleted]=(0))"), &[]);
    another(&mut s, "4242", "4343");
    pin(
        "missing-index",
        &s,
        "missing-index:shop.support_tickets(customer_id,priority,is_deleted,due_at)",
        r#"missing-index:shop.support_tickets(customer_id,priority,is_deleted,due_at) · Low · Low · OptionalImprovement
title: 2 queries on support_tickets pass is_deleted as a parameter, so ix_support_tickets_open can't serve them
summary: SQL Server's optimizer asked for an index on shop.support_tickets (customer_id, priority, is_deleted and due_at) 400 times in the 10 days since the server last restarted, but ix_support_tickets_open (priority and customer_id) already finds these rows where is_deleted = 0 by priority and customer_id, though not by due_at. It's a filtered index, and SQL Server uses one only for a query whose filter matches it as written: the queries below compare is_deleted to a parameter, which could hold any value, so the optimizer can't use the index and asks for a new one instead.
impact: 400 lookups
metric: Lookups that wanted it: 400
metric: Estimated improvement: 0.95
fix: Write the filter as the index does, is_deleted = 0, instead of a parameter in the queries below: ix_support_tickets_open then finds their rows by priority and customer_id. If the range on due_at still costs too much after that, create the index below: the request without is_deleted, filtered the same way, so it holds only the rows where is_deleted = 0. It starts with ix_support_tickets_open's keys, so once it's in use ix_support_tickets_open can go.
cost: shop.support_tickets received no writes in the 10 days since the server last restarted, so this index would cost next to nothing to maintain.
verify: Change the query, let the workload run, then re-scan: the query's plan seeks ix_support_tickets_open, and if the optimizer still asks for an index, it's the one above.
caveat: If the queries also run with other values for is_deleted, those runs can't use ix_support_tickets_open: give them a statement of their own, with their own literal."#,
    );
}

// A filtered index widened to cover two queries.
#[test]
fn a_filtered_index_widened_for_two_queries() {
    let mut s = asked(&["customer_id", "priority", "is_deleted"], &[], &["status"], "[is_deleted] = 0");
    ticket_index(&mut s, "ix_support_tickets_open", &["customer_id", "priority"], Some("([is_deleted]=(0))"), &[]);
    another(&mut s, "4242", "4343");
    pin(
        "missing-index",
        &s,
        "missing-index:shop.support_tickets(customer_id,priority,is_deleted)",
        r#"missing-index:shop.support_tickets(customer_id,priority,is_deleted) · Medium · High · OptionalImprovement
title: support_tickets is searched by customer_id, priority and is_deleted without a supporting index
summary: SQL Server's optimizer asked for an index on shop.support_tickets (customer_id, priority and is_deleted) 400 times in the 10 days since the server last restarted. Without it, each of those lookups reads more of shop.support_tickets (300K rows) than it needs to. It estimates the index would make them about 95% cheaper. The 2 queries that asked for it account for 70% of database time.
impact: 70% of DB time
metric: Share of DB time: 0.7046436248448801
metric: Lookups that wanted it: 400
metric: Estimated improvement: 0.95
metric: Rows in table: 300000
fix: ix_support_tickets_open already finds these rows by customer_id and priority, in the rows where is_deleted = 0. Rebuild it with status included rather than add an index, so it covers the queries below without going back to the table. The statement keeps its keys, their order, its filter and the columns it already includes; restate DATA_COMPRESSION, FILLFACTOR or its partition scheme in it if the index uses them.
cost: The index grows from 4.3 MB; writes that touch the included columns maintain it.
verify: After testing the rebuild of ix_support_tickets_open, capture a newly compiled plan under representative load. A stored suggestion can remain in old plan XML.
verify: Check whether the queries listed here use ix_support_tickets_open and compare reads and execution time under equivalent parameters and load, including index storage and write cost.
caveat: Missing-index suggestions are per-query and ignore the cost of maintaining the index; review overlapping suggestions together."#,
    );
}

// An hour of Query Store, before which the request was last made; and a table whose rows aren't known.
#[test]
fn requests_beside_an_hour_of_queries_and_of_a_table_of_unknown_size() {
    let mut s = snapshot("mssql-demo.json");
    crate::last_hours(s.workload.as_mut().unwrap(), 1.0);
    s.missing_indexes.iter_mut().find(|m| m.table == "order_items").unwrap().last_used =
        Some("2026-10-11T06:00:00Z".into());
    pin(
        "missing-index",
        &s,
        "missing-index:shop.order_items(order_id)",
        r#"missing-index:shop.order_items(order_id) · Low · High · Information
title: SQL Server previously requested an index on order_items (order_id)
summary: SQL Server last requested this index on Oct 11 at 06:00 UTC, before the last 60 minutes. The 300 requests are cumulative in the minute since the server last restarted. Associated statements may still run, but their activity does not establish that this historical request is still needed in the selected period.
impact: Historical optimizer request
metric: Lookups that wanted it: 300
metric: Estimated improvement: 0.9947
metric: Rows in table: 1500000
query 9: Associated with a request made before the selected window; current query activity does not date that request.
fix: Keep this candidate as context. Check representative plans and reads in the period that matters before choosing an index. If the optimizer requests it again, compare the measured benefit with index storage and write maintenance.
verify: A later scan must show a new request inside its workload window and enough measured work before this becomes an index recommendation.
caveat: None of the last 60 minutes' statements asked for it: SQL Server's optimizer last did on Oct 11 at 06:00 UTC, 6 h 14 min before this scan's data ends, too short a time to say the statements that wanted it stopped."#,
    );
    let mut s = snapshot("mssql-demo.json");
    s.tables.retain(|t| t.name != "orders");
    pin(
        "missing-index",
        &s,
        "missing-index:shop.orders(shipped_at)",
        r#"missing-index:shop.orders(shipped_at) · Low · Medium · OptionalImprovement
title: orders is searched by shipped_at without a supporting index
summary: SQL Server's optimizer asked for an index on shop.orders (shipped_at) 150 times in the minute since the server last restarted. Without it, each of those lookups reads more of shop.orders than it needs to. It estimates the index would make them about 81% cheaper. The query that asked for it accounts for 12% of database time.
impact: 12% of DB time
metric: Share of DB time: 0.12221322978535604
metric: Lookups that wanted it: 150
metric: Estimated improvement: 0.81
fix: Create an index on shipped_at, including total.
verify: Test the candidate index with representative parameters and load, then capture a newly compiled plan. Old stored suggestions need not disappear.
verify: Check whether the queries listed here use the new index and compare reads and execution time under equivalent parameters and load, including index storage and write cost.
caveat: Missing-index suggestions are per-query and ignore the cost of maintaining the index; review overlapping suggestions together."#,
    );
}

// MySQL: a table read in full by statements outside the workload (no generic plan to show).
#[test]
fn a_table_read_in_full_on_mysql_by_statements_outside_the_workload() {
    let mut s = ten_days(snapshot("mysql-demo.json"));
    let customers = table(&mut s, "customers");
    customers.seq_scans = Some(1000.0);
    customers.seq_rows_read = Some(1000.0 * customers.rows.unwrap());
    pin(
        "scan-heavy",
        &s,
        "scan-heavy:shop.customers",
        r#"scan-heavy:shop.customers · Low · Low · OptionalImprovement
title: customers is read in full about 100 times a day (19.9M rows a day)
summary: shop.customers has 199K rows, and queries have read all of it about 100 times a day in the 10 days since the server last restarted: 19.9M rows a day read without an index. None of the captured statements names it, so the scans come from statements outside the top of the workload.
impact: 19.9M rows scanned/day
metric: Full reads per day: 100
metric: Rows read without an index per day: 19911600
metric: Rows in table: 199116
fix: Find the statements that read it in full (they aren't among the heaviest this scan captured), explain them to check their access paths. To measure each filter, capture an actual plan with representative values in a controlled environment: this runs the statement. Compare rows kept with rows read. If a filter keeps at most 10% of the rows read, consider an index on the columns it searches by; if it keeps more, read fewer rows or columns instead. Small lookup tables are fine to scan.
cost: shop.customers received no writes in the 10 days since the server last restarted, so any index you add would cost next to nothing to maintain.
verify: After adding an index for a selective filter, the full reads of customers should grow far slower. Compare how much its scan counters grow between a scan taken once the change is live and one a day or more later (1.0K full reads in this scan).
caveat: Counters accumulate since statistics were last reset; one nightly report can make up much of them."#,
    );
}

// One statement whose plan reads it in full and one without a plan; statements that stopped over ten days of counters;
// and one statement on a windowed source.
#[test]
fn a_table_read_in_full_in_more_ways() {
    let mut s = ten_days(snapshot("postgres-demo.json"));
    events_read_in_full(&mut s, 1000.0);
    let q = events_statement(&s, "scans-events", "SELECT kind FROM events WHERE note = $1", true, 10.0);
    add(&mut s, q);
    let q = events_statement(&s, "maybe-events", "SELECT id FROM events WHERE created_at > $1", false, 10.0);
    add(&mut s, q);
    pin(
        "scan-heavy",
        &s,
        "scan-heavy:public.events",
        r#"scan-heavy:public.events · Low · Medium · OptionalImprovement
title: events is read in full about 100 times a day (30.0M rows a day)
summary: public.events has 300K rows, and queries have read all of it about 100 times a day in the 10 days since the server last restarted: 30.0M rows a day read without an index. 1 of the captured statements reads it in full (its plan scans it), and 1 more without a captured plan may; explain them to check the access path, then measure rows kept in representative runs.
impact: 30.0M rows scanned/day
metric: Full reads per day: 100
metric: Rows read without an index per day: 30000000
metric: Rows in table: 300000
fix: Explain the statements listed with this finding (Culprit can show a generic plan, which does not measure actual rows) to check their access paths. To measure each filter, capture an actual plan with representative values in a controlled environment: this runs the statement. Compare rows kept with rows read. If a filter keeps at most 10% of the rows read, consider an index on the columns it searches by; if it keeps more, read fewer rows or columns instead. Small lookup tables are fine to scan.
cost: public.events received no writes in the 10 days since the server last restarted, so any index you add would cost next to nothing to maintain.
verify: After adding an index for a selective filter, the full reads of events should grow far slower. Compare how much its scan counters grow between a scan taken once the change is live and one a day or more later (1.0K full reads and 1.7K index scans in this scan).
verify: Whether the fix adds an index or reads fewer rows or columns: pg_stat_statements keeps running totals, so a later scan's runs, share and averages for the queries still hold every run from before the change. Scan when the change ships and again a day or more later, then compare the two in What changed, with Since at the ship time (`culprit changes <connection> --since <ship time>`): there their time per run should drop.
caveat: Counters accumulate since statistics were last reset; one nightly report can make up much of them."#,
    );

    let mut s = ten_days(snapshot("postgres-demo.json"));
    events_read_in_full(&mut s, 1000.0);
    let mut q = events_statement(&s, "scans-events", "SELECT kind FROM events WHERE note = $1", true, 10.0);
    q.last_seen = Some("2026-01-01T00:00:00Z".into());
    add(&mut s, q);
    pin(
        "scan-heavy",
        &s,
        "scan-heavy:public.events",
        r#"scan-heavy:public.events · Low · Medium · OptionalImprovement
title: events was read in full about 100 times a day (30.0M rows a day)
summary: public.events has 300K rows, and queries have read all of it about 100 times a day in the 10 days since the server last restarted: 30.0M rows a day read without an index. 1 of the captured statements reads it in full (its plan scans it); explain it to check the access path, then measure rows kept in representative runs.
impact: 30.0M rows scanned/day
metric: Full reads per day: 100
metric: Rows read without an index per day: 30000000
metric: Rows in table: 300000
fix: Explain the statements listed with this finding (Culprit can show a generic plan, which does not measure actual rows) to check their access paths. To measure each filter, capture an actual plan with representative values in a controlled environment: this runs the statement. Compare rows kept with rows read. If a filter keeps at most 10% of the rows read, consider an index on the columns it searches by; if it keeps more, read fewer rows or columns instead. Small lookup tables are fine to scan.
cost: public.events received no writes in the 10 days since the server last restarted, so any index you add would cost next to nothing to maintain.
verify: After adding an index for a selective filter, the full reads of events should grow far slower. Compare how much its scan counters grow between a scan taken once the change is live and one a day or more later (1.0K full reads and 1.7K index scans in this scan).
verify: Whether the fix adds an index or reads fewer rows or columns: pg_stat_statements keeps running totals, so a later scan's runs, share and averages for the query still hold every run from before the change. Scan when the change ships and again a day or more later, then compare the two in What changed, with Since at the ship time (`culprit changes <connection> --since <ship time>`): there its time per run should drop.
caveat: Counters accumulate since statistics were last reset; one nightly report can make up much of them."#,
    );

    let mut s = ten_days(snapshot("postgres-demo.json"));
    events_read_in_full(&mut s, 1000.0);
    let q = events_statement(&s, "scans-events", "SELECT kind FROM events WHERE note = $1", true, 10.0);
    add(&mut s, q);
    s.workload.as_mut().unwrap().windowed = true;
    pin(
        "scan-heavy",
        &s,
        "scan-heavy:public.events",
        r#"scan-heavy:public.events · Low · Medium · OptionalImprovement
title: events is read in full about 100 times a day (30.0M rows a day)
summary: public.events has 300K rows, and queries have read all of it about 100 times a day in the 10 days since the server last restarted: 30.0M rows a day read without an index. 1 of the captured statements reads it in full (its plan scans it); explain it to check the access path, then measure rows kept in representative runs.
impact: 30.0M rows scanned/day
metric: Full reads per day: 100
metric: Rows read without an index per day: 30000000
metric: Rows in table: 300000
fix: Explain the statements listed with this finding (Culprit can show a generic plan, which does not measure actual rows) to check their access paths. To measure each filter, capture an actual plan with representative values in a controlled environment: this runs the statement. Compare rows kept with rows read. If a filter keeps at most 10% of the rows read, consider an index on the columns it searches by; if it keeps more, read fewer rows or columns instead. Small lookup tables are fine to scan.
cost: public.events received no writes in the 10 days since the server last restarted, so any index you add would cost next to nothing to maintain.
verify: After adding an index for a selective filter, the full reads of events should grow far slower. Compare how much its scan counters grow between a scan taken once the change is live and one a day or more later (1.0K full reads and 1.7K index scans in this scan).
verify: Whether the fix adds an index or reads fewer rows or columns: Re-scan after the workload has run: its time per run should drop.
caveat: Counters accumulate since statistics were last reset; one nightly report can make up much of them."#,
    );
}

// A window of unknown length, before whose end the request was last made: "the recorded workload window's".
#[test]
fn a_request_beside_a_window_of_unknown_length() {
    let mut s = snapshot("mssql-demo.json");
    s.workload.as_mut().unwrap().window_hours = 0.0;
    s.missing_indexes.iter_mut().find(|m| m.table == "order_items").unwrap().last_used =
        Some("2026-10-11T06:00:00Z".into());
    pin(
        "missing-index",
        &s,
        "missing-index:shop.order_items(order_id)",
        r#"missing-index:shop.order_items(order_id) · Low · High · Information
title: SQL Server previously requested an index on order_items (order_id)
summary: SQL Server last requested this index on Oct 11 at 06:00 UTC, before the recorded workload window. The 300 requests are cumulative in the minute since the server last restarted. Associated statements may still run, but their activity does not establish that this historical request is still needed in the selected period.
impact: Historical optimizer request
metric: Lookups that wanted it: 300
metric: Estimated improvement: 0.9947
metric: Rows in table: 1500000
query 9: Associated with a request made before the selected window; current query activity does not date that request.
fix: Keep this candidate as context. Check representative plans and reads in the period that matters before choosing an index. If the optimizer requests it again, compare the measured benefit with index storage and write maintenance.
verify: A later scan must show a new request inside its workload window and enough measured work before this becomes an index recommendation.
caveat: None of the recorded workload window's statements asked for it: SQL Server's optimizer last did on Oct 11 at 06:00 UTC, 6 h 14 min before this scan's data ends, too short a time to say the statements that wanted it stopped."#,
    );
}

// An index on the key the workload shows created, at a time it didn't record; on a table still there, and gone.
#[test]
fn a_scan_past_a_dropped_index_of_unknown_date() {
    let plant = |gone: bool| {
        let mut s = snapshot("postgres-demo.json");
        let id = "-3722457391793582113";
        let mut create = s.queries().iter().find(|q| q.id == id).cloned().expect("a scan of order_items");
        (create.id, create.plan, create.executions, create.first_seen) = ("create".into(), None, 1.0, None);
        create.text = "CREATE INDEX ix_items_product ON public.order_items (product_id)".into();
        add(&mut s, create);
        if gone {
            let q = query(&mut s, id);
            q.executions = 1.0;
            scan_node(q).unwrap().table_rows = Some(1_000_000.0);
            s.tables.retain(|t| t.name != "order_items");
            s.indexes.retain(|i| i.table != "order_items");
        }
        s
    };
    pin(
        "filtered-scan",
        &plant(false),
        "filtered-scan:public.order_items(product_id)",
        r#"filtered-scan:public.order_items(product_id) · Low · High · OptionalImprovement
title: order_items is read in full to find rows by product_id
summary: One query plans to read public.order_items and keep only those matching `(order_items.product_id = $1)`. The captured scan uses 1.0M rows as its size basis; this scan's catalog reports 1.0M rows in the table. Captured estimates can predate the current table size. No index starts with product_id, so the database has no faster way in. It accounts for 4% of database time. product_id is also the foreign key to products, so deletes there scan this table too. An index on product_id did exist: pg_stat_statements recorded `CREATE INDEX ix_items_product ON public.order_items (product_id)`, and the catalog doesn't list ix_items_product now, so it was dropped or renamed since.
impact: 4% of DB time
metric: Share of DB time: 0.04140462131320083
metric: Captured scan size basis: 1000000
metric: Queries: 1
fix: Test an index on product_id as a way for the query to find matching rows without a full scan.
verify: Obtain a newly compiled plan for the query and check whether order_items uses the new index to read fewer rows. A scan can remain appropriate for some parameter values.
verify: pg_stat_statements keeps running totals, so a later scan's runs, share and averages for the query still hold every run from before the change. Scan when the change ships and again a day or more later, then compare the two in What changed, with Since at the ship time (`culprit changes <connection> --since <ship time>`): there compare its time per run under equivalent load. Use the scans' read-counter changes to compare reads per new run, accounting for resets; include index storage and write cost."#,
    );
    pin(
        "filtered-scan",
        &plant(true),
        "filtered-scan:public.order_items(product_id)",
        r#"filtered-scan:public.order_items(product_id) · Low · High · History
title: order_items was read in full to find rows by product_id
summary: One query read public.order_items and kept only those matching `(order_items.product_id = $1)`. The captured scan uses 1.0M rows as its size basis; the current table row count is unavailable. This is not a measured count of rows read on every execution. The plan it last ran with had no index starting with product_id to seek. It accounted for 4% of database time. product_id is also the foreign key to products, so deletes there scan this table too. public.order_items isn't in the database's catalog any more. An index on product_id did exist: pg_stat_statements recorded `CREATE INDEX ix_items_product ON public.order_items (product_id)`.
impact: 4% of DB time
metric: Share of DB time: 0.04140462131320083
metric: Captured scan size basis: 1000000
metric: Queries: 1
fix: Nothing to create: public.order_items isn't in the database's catalog any more, so it was dropped or renamed since these statements ran. If they now read a table under another name, a scan of it says whether that one needs an index.
verify: Nothing to check: public.order_items is gone, and this finding goes once no captured statement reads it."#,
    );
}

// Two statements read past an invalid index that leads with the key.
#[test]
fn two_scans_past_an_invalid_index() {
    let mut s = ten_days(snapshot("postgres-demo.json"));
    let mut invalid = index_like(&s, "orders_status_idx", "orders_customer_idx", &["customer_id"]);
    invalid.is_disabled = true;
    s.indexes.push(invalid);
    let mut q = s.queries().iter().find(|q| q.id == "5308963031944094559").unwrap().clone();
    q.id = "second-read".into();
    q.text = "SELECT id FROM orders WHERE customer_id = $1".into();
    add(&mut s, q);
    pin(
        "filtered-scan",
        &s,
        "filtered-scan:public.orders(customer_id)",
        r#"filtered-scan:public.orders(customer_id) · Medium · High · OptionalImprovement
title: orders is read in full to find rows by customer_id
summary: 2 queries plan to read public.orders and keep only those matching `(orders.customer_id = $1)`. The captured scan uses 2.0M rows as its size basis; this scan's catalog reports 2.0M rows in the table. Captured estimates can predate the current table size. Only orders_customer_idx, which is disabled, starts with customer_id, so the database has no faster way in. Together they account for 39% of database time. customer_id is also the foreign key to customers, so deletes there scan this table too.
impact: 39% of DB time
metric: Share of DB time: 0.38678343852074343
metric: Captured scan size basis: 1999995
metric: Queries: 2
fix: orders_customer_idx has these keys, but it is invalid (a build that didn't finish), so PostgreSQL never reads it. Rebuilding it makes it usable; if it was set aside because nothing read it, the queries below now would.
verify: Obtain a newly compiled plan for one of the queries and check whether orders uses orders_customer_idx to read fewer rows. A scan can remain appropriate for some parameter values.
verify: pg_stat_statements keeps running totals, so a later scan's runs, share and averages for the queries still hold every run from before the change. Scan when the change ships and again a day or more later, then compare the two in What changed, with Since at the ship time (`culprit changes <connection> --since <ship time>`): there compare their time per run under equivalent load. Use the scans' read-counter changes to compare reads per new run, accounting for resets; include index storage and write cost."#,
    );
}
