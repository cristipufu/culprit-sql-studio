//! What each maintenance finding says, word for word, in every shape it takes: its messages come from
//! locales/en.json, and moving them there must not change a word of English.

use super::*;
use crate::checks::pressure::words::{pin, said};
use crate::waits::snapshot_with;
use culprit_core::operations::FreezeAge;
use culprit_core::{CleanupHolder, OpenTransaction, Setting, StatisticsInfo, TableStorage};

fn set(s: &mut Snapshot, name: &str, value: &str) {
    s.settings.retain(|x| x.name != name);
    s.settings.push(Setting { name: name.into(), value: value.into(), default_value: None, scope: "server".into() });
}

fn table<'a>(s: &'a mut Snapshot, name: &str) -> &'a mut culprit_core::TableStat {
    s.tables.iter_mut().find(|t| t.name == name).expect("a demo table")
}

fn options(t: &mut culprit_core::TableStat, options: &[&str]) {
    t.storage = Some(TableStorage {
        options: options.iter().map(|o| o.to_string()).collect(),
        ..t.storage.clone().unwrap_or_default()
    });
}

fn holder(s: &mut Snapshot, t: OpenTransaction) {
    crate::retention_fixture::install(s, t);
}

fn session(behind: f64) -> OpenTransaction {
    OpenTransaction {
        session: "4242".into(),
        seconds: 7200.0,
        activity: culprit_core::TransactionActivity::IdleInTransaction,
        state: Some("idle in transaction".into()),
        holder: CleanupHolder::Session,
        xid_age: Some(behind),
        ..OpenTransaction::default()
    }
}

#[test]
fn every_maintenance_check_describes_itself() {
    let said: Vec<String> = CHECKS
        .iter()
        .map(|c| {
            let info = c.info();
            format!("{}: {}: {}", info.id, info.title, info.description)
        })
        .collect();
    pin(vec![("infos", said.join("\n"))], INFOS);
}

#[test]
fn multixact_age_words() {
    let ages = |ages: &[(f64, Option<f64>)]| {
        let mut s = snapshot_with(Engine::Postgres);
        s.operations.multixacts = ages
            .iter()
            .enumerate()
            .map(|(i, (age, max))| FreezeAge { subject: format!("table public.t{i}"), age: *age, freeze_max_age: *max })
            .collect();
        said(&multixact_age::MultixactAgeCheck, &s)
    };
    pin(
        vec![
            ("high", ages(&[(1_300_000_000.0, Some(400_000_000.0))])),
            ("medium", ages(&[(600_000_000.0, None), (900_000_000.0, None)])),
        ],
        MULTIXACT_AGE,
    );
}

#[test]
fn xid_age_words() {
    let aged = |age: f64, freeze_max: Option<&str>, t: Option<OpenTransaction>| {
        let mut s = snapshot_with(Engine::Postgres);
        set(&mut s, "datfrozenxid_age", &format!("{age}"));
        if let Some(m) = freeze_max {
            set(&mut s, "autovacuum_freeze_max_age", m);
        }
        if let Some(t) = t {
            holder(&mut s, t);
        }
        s
    };
    let mut no_tables = aged(600_000_000.0, None, None);
    no_tables.tables.iter_mut().for_each(|t| t.xid_age = None);
    let prepared = OpenTransaction {
        session: "tx-17".into(),
        holder: CleanupHolder::PreparedTransaction,
        xid_age: Some(2_000.0),
        ..OpenTransaction::default()
    };
    let slot = OpenTransaction {
        session: "standby_1".into(),
        holder: CleanupHolder::ReplicationSlot,
        xid_age: Some(90_000_000.0),
        ..OpenTransaction::default()
    };
    pin(
        vec![
            ("behind", said(&XidAgeCheck, &aged(1_300_000_000.0, Some("200000000"), Some(session(5_000_000.0))))),
            ("no-tables", said(&XidAgeCheck, &no_tables)),
            ("prepared", said(&XidAgeCheck, &aged(1_600_000_000.0, Some("1500000000"), Some(prepared)))),
            ("slot", said(&XidAgeCheck, &aged(1_300_000_000.0, Some("1500000000"), Some(slot)))),
        ],
        XID_AGE,
    );
}

#[test]
fn dead_rows_words() {
    let orders = |last_vacuum: Option<&str>, storage: &[&str]| {
        let mut s = snapshot_with(Engine::Postgres);
        let t = table(&mut s, "orders");
        t.dead_rows = Some(1_000_000.0);
        t.last_vacuum = last_vacuum.map(str::to_string);
        if !storage.is_empty() {
            options(t, storage);
        }
        s
    };
    let mut held = orders(Some("2026-10-09T11:54:21Z"), &[]);
    holder(&mut held, session(5_000_000.0));
    let mut scale = snapshot_with(Engine::Postgres);
    let t = table(&mut scale, "events");
    (t.rows, t.dead_rows) = (Some(6_000_000.0), Some(200_000.0));
    let t = table(&mut scale, "customers");
    (t.rows, t.dead_rows, t.updates, t.deletes) = (Some(5_500_000.0), Some(0.0), Some(300_000.0), Some(0.0));
    let mut disabled = snapshot_with(Engine::Postgres);
    options(table(&mut disabled, "products"), &["autovacuum_enabled=false"]);
    options(table(&mut disabled, "customers"), &["toast.autovacuum_enabled=off"]);
    pin(
        vec![
            ("behind", said(&DeadRowsCheck, &orders(None, &[]))),
            ("held", said(&DeadRowsCheck, &held)),
            ("recent", said(&DeadRowsCheck, &orders(Some("2026-10-11T11:24:21Z"), &[]))),
            ("off", said(&DeadRowsCheck, &orders(Some("2026-10-12T00:00:00Z"), &["autovacuum_enabled=false"]))),
            ("low-factor", said(&DeadRowsCheck, &orders(None, &["autovacuum_vacuum_scale_factor=0.01"]))),
            (
                "low-factor-off",
                said(&DeadRowsCheck, &orders(None, &["autovacuum_vacuum_scale_factor=0.01", "autovacuum_enabled=off"])),
            ),
            ("scale-factor", said(&DeadRowsCheck, &scale)),
            ("disabled", said(&DeadRowsCheck, &disabled)),
        ],
        DEAD_ROWS,
    );
}

fn stat(table: &str, name: &str, at: Option<&str>, rows: f64, modifications: f64) -> StatisticsInfo {
    StatisticsInfo {
        schema: "shop".into(),
        table: table.into(),
        name: name.into(),
        last_updated: at.map(str::to_string),
        rows: Some(rows),
        rows_sampled: Some(rows),
        modifications: Some(modifications),
        auto_created: true,
    }
}

#[test]
fn stale_stats_words() {
    let mssql = |auto: bool, stats: Vec<StatisticsInfo>| {
        let mut s = snapshot_with(Engine::Mssql);
        s.statistics = stats;
        if !auto {
            set(&mut s, "is_auto_update_stats_on", "0");
        }
        said(&StaleStatsCheck, &s)
    };
    let products = |name: &str, at: Option<&str>, mods: f64| stat("products", name, at, 20_000.0, mods);
    let mut pg = snapshot_with(Engine::Postgres);
    let t = table(&mut pg, "orders");
    (t.modifications_since_analyze, t.last_analyze) = (Some(1_500_000.0), Some("2026-10-01T00:00:00Z".into()));
    let t = table(&mut pg, "customers");
    (t.modifications_since_analyze, t.last_analyze) = (Some(100_000.0), None);
    pin(
        vec![
            ("auto", mssql(true, vec![products("st_products", Some("2026-10-01T00:00:00Z"), 15_000.0)])),
            ("auto-undated", mssql(true, vec![products("st_products", None, 15_000.0)])),
            (
                "auto-two",
                mssql(
                    true,
                    vec![
                        products("st_a", Some("2026-10-13T09:00:00Z"), 8_000.0),
                        products("st_b", Some("2026-09-08T02:00:00Z"), 26_000.0),
                    ],
                ),
            ),
            (
                "auto-three",
                mssql(
                    true,
                    vec![
                        products("st_a", Some("2026-10-13T09:00:00Z"), 26_000.0),
                        products("st_b", None, 8_000.0),
                        products("st_c", Some("2026-09-08T02:00:00Z"), 6_000.0),
                    ],
                ),
            ),
            (
                "manual-hot",
                mssql(
                    false,
                    vec![stat("customers", "st_customers", Some("2026-10-01T00:00:00Z"), 200_000.0, 150_000.0)],
                ),
            ),
            (
                "manual",
                mssql(
                    false,
                    vec![stat("ticket_events", "st_events", Some("2026-10-01T00:00:00Z"), 800_000.0, 300_000.0)],
                ),
            ),
            ("postgres", said(&StaleStatsCheck, &pg)),
        ],
        STALE_STATS,
    );
}

// What each scenario said before its words moved to locales/en.json, which they must still say.
const INFOS: &[(&str, &str)] = &[(
    "infos",
    r#"stale-stats: Stale statistics: Tables whose optimizer statistics are far behind their data, which leads to bad row estimates and plans.
heap-table: Large tables without a clustered index: Heaps grow forwarded records and can't be read in key order; most OLTP tables should be clustered.
dead-rows: Dead rows and lagging autovacuum: Tables carrying many dead tuples, which bloat storage and slow scans.
xid-age: Transaction ID wraparound risk: Transaction ID age approaching the point where PostgreSQL stops accepting writes to protect data.
multixact-age: MultiXact wraparound risk: The independent PostgreSQL MultiXact age limit, which can reject row-locking writes even when ordinary transaction ID age is healthy."#,
)];
const MULTIXACT_AGE: &[(&str, &str)] = &[
    (
        "high",
        r#"multixact-age:demo · High · High · OptionalImprovement · score 45.00
title: MultiXact IDs are 1.3B old in table public.t0
summary: MultiXact IDs track shared row locks and have their own wraparound limit, independent of ordinary transaction IDs. If vacuum cannot advance them, PostgreSQL eventually rejects writes that require a new MultiXact. A healthy XID-age result does not rule this out. This observation is the current age, not a forecast of time remaining.
impact: 1.3B IDs old
metric: MultiXact age
metric: Oldest observed subject: table public.t0
fix: Inspect aggressive vacuum progress and transactions holding row locks, then vacuum the oldest affected tables under an operational plan. MultiXact cleanup differs from XID cleanup: replication slots do not directly retain MultiXacts, so removing slots is not a fix for this age.
cost: Aggressive vacuum adds I/O and must finish to advance the oldest age; inspect locks and capacity before scheduling it.
verify: Re-scan and confirm mxid_age(datminmxid) and the oldest tables' mxid_age(relminmxid) fall. Verify row-locking writes proceed and later vacuum cycles keep age controlled."#,
    ),
    (
        "medium",
        r#"multixact-age:demo · Medium · High · OptionalImprovement · score 22.00
title: MultiXact IDs are 900.0M old in table public.t1
summary: MultiXact IDs track shared row locks and have their own wraparound limit, independent of ordinary transaction IDs. If vacuum cannot advance them, PostgreSQL eventually rejects writes that require a new MultiXact. A healthy XID-age result does not rule this out. This observation is the current age, not a forecast of time remaining.
impact: 900.0M IDs old
metric: MultiXact age
metric: Oldest observed subject: table public.t1
fix: Inspect aggressive vacuum progress and transactions holding row locks, then vacuum the oldest affected tables under an operational plan. MultiXact cleanup differs from XID cleanup: replication slots do not directly retain MultiXacts, so removing slots is not a fix for this age.
cost: Aggressive vacuum adds I/O and must finish to advance the oldest age; inspect locks and capacity before scheduling it.
verify: Re-scan and confirm mxid_age(datminmxid) and the oldest tables' mxid_age(relminmxid) fall. Verify row-locking writes proceed and later vacuum cycles keep age controlled."#,
    ),
];
const XID_AGE: &[(&str, &str)] = &[
    (
        "behind",
        r#"xid-age:demo · High · High · OptionalImprovement · score 70.00
title: Transaction IDs are 65% of the way to wraparound
summary: The oldest unfrozen transaction ID in this database is 1.3B transactions old. Autovacuum starts freezing a table at autovacuum_freeze_max_age (200.0M), and this is 6.5 times that: freezing isn't keeping up. Cleanup retainers were observed in this scan (session 4242, 5.0M transactions behind). Inspect their native horizons alongside VACUUM progress. At about 2 billion PostgreSQL stops accepting writes until a full vacuum completes. 700.0M transactions remain.
impact: 700.0M left
metric: Transaction ID age
metric: Remaining before wraparound
metric: Oldest table: public.customers
metric: Oldest cleanup holder: session 4242, 5.0M transactions behind
fix: Freeze the oldest tables first, and look for what holds vacuum back: long-running or idle-in-transaction sessions, abandoned replication slots, or prepared transactions.
sql: VACUUM (FREEZE, VERBOSE) public.customers;
cost: A freeze vacuum reads the whole table; it doesn't block reads or writes."#,
    ),
    (
        "no-tables",
        r#"xid-age:demo · Medium · High · OptionalImprovement · score 35.00
title: Transaction IDs are 30% of the way to wraparound
summary: The oldest unfrozen transaction ID in this database is 600.0M transactions old. At about 2 billion PostgreSQL stops accepting writes until a full vacuum completes. 1.4B transactions remain.
impact: 1.4B left
metric: Transaction ID age
metric: Remaining before wraparound
fix: Freeze the oldest tables first, and look for what holds vacuum back: long-running or idle-in-transaction sessions, abandoned replication slots, or prepared transactions.
sql: VACUUM (FREEZE, VERBOSE);
cost: A freeze vacuum reads the whole table; it doesn't block reads or writes."#,
    ),
    (
        "prepared",
        r#"xid-age:demo · High · High · OptionalImprovement · score 95.00
title: Transaction IDs are 80% of the way to wraparound
summary: The oldest unfrozen transaction ID in this database is 1.6B transactions old. Autovacuum starts freezing a table at autovacuum_freeze_max_age (1.5B), and this is 1.1 times that: freezing isn't keeping up. Cleanup retainers were observed in this scan (prepared transaction 'tx-17', 2.0K transactions behind). Inspect their native horizons alongside VACUUM progress. At about 2 billion PostgreSQL stops accepting writes until a full vacuum completes. 400.0M transactions remain.
impact: 400.0M left
metric: Transaction ID age
metric: Remaining before wraparound
metric: Oldest table: public.customers
metric: Oldest cleanup holder: prepared transaction 'tx-17', 2.0K transactions behind
fix: Freeze the oldest tables first, and look for what holds vacuum back: long-running or idle-in-transaction sessions, abandoned replication slots, or prepared transactions.
sql: VACUUM (FREEZE, VERBOSE) public.customers;
cost: A freeze vacuum reads the whole table; it doesn't block reads or writes."#,
    ),
    (
        "slot",
        r#"xid-age:demo · High · High · OptionalImprovement · score 70.00
title: Transaction IDs are 65% of the way to wraparound
summary: The oldest unfrozen transaction ID in this database is 1.3B transactions old. Cleanup retainers were observed in this scan (replication slot 'standby_1', 90.0M transactions behind). Inspect their native horizons alongside VACUUM progress. At about 2 billion PostgreSQL stops accepting writes until a full vacuum completes. 700.0M transactions remain.
impact: 700.0M left
metric: Transaction ID age
metric: Remaining before wraparound
metric: Oldest table: public.customers
metric: Oldest cleanup holder: replication slot 'standby_1', 90.0M transactions behind
fix: Freeze the oldest tables first, and look for what holds vacuum back: long-running or idle-in-transaction sessions, abandoned replication slots, or prepared transactions.
sql: VACUUM (FREEZE, VERBOSE) public.customers;
cost: A freeze vacuum reads the whole table; it doesn't block reads or writes."#,
    ),
];
const DEAD_ROWS: &[(&str, &str)] = &[
    (
        "behind",
        r#"dead-rows:public.orders · Low · High · OptionalImprovement · score 11.33
title: 33% of orders is dead rows
summary: public.orders holds 1.0M dead rows next to 2.0M live ones. Every sequential scan still reads them, indexes still point at them, and the table stays larger than it needs to be. Autovacuum should visit it at about 400K dead rows; it has 2.5 times that, so it is behind. It has never been vacuumed since statistics were reset.
impact: 1.0M dead rows
metric: Dead rows
metric: Live rows
metric: Dead share
metric: Autovacuum trigger
fix: Vacuum it now, and make autovacuum visit this table sooner. If dead rows come back quickly, look for long-running transactions in Live: they stop vacuum from cleaning up.
sql: VACUUM (ANALYZE) public.orders;
ALTER TABLE public.orders SET (autovacuum_vacuum_scale_factor = 0.02);
cost: VACUUM doesn't block reads or writes; it does add I/O while it runs.
verify: Re-scan after VACUUM (ANALYZE): the dead-row count should fall below the table's autovacuum trigger, the last analyze time should be newer, and changes since analyze should fall. Check again after representative writes to confirm autovacuum keeps the dead-row count near its trigger rather than letting the backlog return."#,
    ),
    (
        "held",
        r#"dead-rows:public.orders · Low · High · OptionalImprovement · score 11.33
title: 33% of orders is dead rows
summary: public.orders holds 1.0M dead rows next to 2.0M live ones. Every sequential scan still reads them, indexes still point at them, and the table stays larger than it needs to be. Cleanup retainers were observed in this scan (session 4242, 5.0M transactions behind). They can prevent removal of some dead rows; this count does not establish how many they retain. Check the retainers and vacuum progress before changing maintenance. It was last vacuumed 2.0 days ago.
impact: 1.0M dead rows
metric: Dead rows
metric: Live rows
metric: Dead share
metric: Autovacuum trigger
fix: Inspect the observed cleanup holder and vacuum progress. Finish a transaction only after checking its owner and purpose; a replication consumer or prepared transaction needs its own recovery procedure. Then measure whether vacuum removes the backlog under representative writes.
verify: After safely resolving the cleanup hold and allowing VACUUM to complete, re-scan: the dead-row count should fall below the table's autovacuum trigger, the last analyze time should be newer, and changes since analyze should fall. Check again after representative writes to confirm autovacuum keeps the dead-row count near its trigger rather than letting the backlog return."#,
    ),
    (
        "recent",
        r#"dead-rows:public.orders · Low · High · OptionalImprovement · score 11.33
title: 33% of orders is dead rows
summary: public.orders holds 1.0M dead rows next to 2.0M live ones. Every sequential scan still reads them, indexes still point at them, and the table stays larger than it needs to be. Vacuum ran within the last hour. These rows may have become dead afterwards, or an older snapshot may prevent removal; the scan does not establish which. Compare write activity and the next vacuum before treating this as failed cleanup. It was last vacuumed 30 min ago.
impact: 1.0M dead rows
metric: Dead rows
metric: Live rows
metric: Dead share
metric: Autovacuum trigger
fix: Compare representative writes and the next autovacuum with the dead-row estimate. Inspect vacuum progress and any cleanup holders if the backlog persists. Change per-table maintenance only after establishing that its current settings cannot keep up.
verify: Compare the next completed vacuum under representative writes: the dead-row count should fall below the table's autovacuum trigger, the last analyze time should be newer, and changes since analyze should fall. Check again after representative writes to confirm autovacuum keeps the dead-row count near its trigger rather than letting the backlog return."#,
    ),
    (
        "off",
        r#"dead-rows:public.orders · High · High · OptionalImprovement · score 48.33
title: 33% of orders is dead rows
summary: public.orders holds 1.0M dead rows next to 2.0M live ones. Every sequential scan still reads them, indexes still point at them, and the table stays larger than it needs to be. Autovacuum is off for it, so nothing cleans them up. It was last vacuumed 2026-10-12.
impact: 1.0M dead rows
metric: Dead rows
metric: Live rows
metric: Dead share
fix: Vacuum it now, and make autovacuum visit this table sooner. If dead rows come back quickly, look for long-running transactions in Live: they stop vacuum from cleaning up.
sql: VACUUM (ANALYZE) public.orders;
ALTER TABLE public.orders SET (autovacuum_vacuum_scale_factor = 0.02, autovacuum_enabled = true);
cost: VACUUM doesn't block reads or writes; it does add I/O while it runs.
verify: Re-scan after VACUUM (ANALYZE): the dead-row count should fall below the table's autovacuum trigger, the last analyze time should be newer, and changes since analyze should fall. Check again after representative writes to confirm autovacuum keeps the dead-row count near its trigger rather than letting the backlog return."#,
    ),
    (
        "low-factor",
        r#"dead-rows:public.orders · Low · High · OptionalImprovement · score 11.33
title: 33% of orders is dead rows
summary: public.orders holds 1.0M dead rows next to 2.0M live ones. Every sequential scan still reads them, indexes still point at them, and the table stays larger than it needs to be. Autovacuum should visit it at about 20K dead rows; it has 49.9 times that, so it is behind. It has never been vacuumed since statistics were reset.
impact: 1.0M dead rows
metric: Dead rows
metric: Live rows
metric: Dead share
metric: Autovacuum trigger
fix: Vacuum it now, and make autovacuum visit this table sooner. If dead rows come back quickly, look for long-running transactions in Live: they stop vacuum from cleaning up.
sql: VACUUM (ANALYZE) public.orders;
cost: VACUUM doesn't block reads or writes; it does add I/O while it runs.
verify: Re-scan after VACUUM (ANALYZE): the dead-row count should fall below the table's autovacuum trigger, the last analyze time should be newer, and changes since analyze should fall. Check again after representative writes to confirm autovacuum keeps the dead-row count near its trigger rather than letting the backlog return."#,
    ),
    (
        "low-factor-off",
        r#"dead-rows:public.orders · High · High · OptionalImprovement · score 48.33
title: 33% of orders is dead rows
summary: public.orders holds 1.0M dead rows next to 2.0M live ones. Every sequential scan still reads them, indexes still point at them, and the table stays larger than it needs to be. Autovacuum is off for it, so nothing cleans them up. It has never been vacuumed since statistics were reset.
impact: 1.0M dead rows
metric: Dead rows
metric: Live rows
metric: Dead share
fix: Vacuum it now, and make autovacuum visit this table sooner. If dead rows come back quickly, look for long-running transactions in Live: they stop vacuum from cleaning up.
sql: VACUUM (ANALYZE) public.orders;
ALTER TABLE public.orders SET (autovacuum_enabled = true);
cost: VACUUM doesn't block reads or writes; it does add I/O while it runs.
verify: Re-scan after VACUUM (ANALYZE): the dead-row count should fall below the table's autovacuum trigger, the last analyze time should be newer, and changes since analyze should fall. Check again after representative writes to confirm autovacuum keeps the dead-row count near its trigger rather than letting the backlog return."#,
    ),
    (
        "scale-factor",
        r#"dead-rows:scale-factor:public.customers · Low · High · OptionalImprovement · score 8.00
title: customers (5.5M rows) waits for 1.1M dead rows before autovacuum starts
summary: public.customers has 5.5M live rows (and 0 dead rows today) with `autovacuum_vacuum_scale_factor` at 20%. At that scale factor, autovacuum does not start until about 1.1M dead rows accumulate, causing infrequent, long-running vacuum passes and larger table/index bloat between runs.
impact: trigger 1.1M rows
metric: Live rows
metric: Autovacuum trigger
fix: Set a lower per-table autovacuum and autoanalyze scale factor on public.customers so vacuum and statistics updates run in smaller, more frequent passes.
sql: ALTER TABLE public.customers SET (autovacuum_vacuum_scale_factor = 0.01, autovacuum_analyze_scale_factor = 0.005);
cost: Setting per-table storage parameters is a fast metadata change and does not rewrite the table.
verify: Re-scan after applying the storage options: public.customers's autovacuum trigger drops to about 1% of its live rows.
--
dead-rows:scale-factor:public.events · Low · High · OptionalImprovement · score 8.00
title: events (6.0M rows) waits for 1.2M dead rows before autovacuum starts
summary: public.events has 6.0M live rows (and 200K dead rows today) with `autovacuum_vacuum_scale_factor` at 20%. At that scale factor, autovacuum does not start until about 1.2M dead rows accumulate, causing infrequent, long-running vacuum passes and larger table/index bloat between runs.
impact: trigger 1.2M rows
metric: Live rows
metric: Dead rows
metric: Autovacuum trigger
fix: Set a lower per-table autovacuum and autoanalyze scale factor on public.events so vacuum and statistics updates run in smaller, more frequent passes.
sql: ALTER TABLE public.events SET (autovacuum_vacuum_scale_factor = 0.01, autovacuum_analyze_scale_factor = 0.005);
cost: Setting per-table storage parameters is a fast metadata change and does not rewrite the table.
verify: Re-scan after applying the storage options: public.events's autovacuum trigger drops to about 1% of its live rows."#,
    ),
    (
        "disabled",
        r#"dead-rows:autovacuum-disabled:public.customers · High · High · NeedsAttention · score 45.00
title: Autovacuum is disabled on customers (toast.autovacuum_enabled = false)
summary: public.customers has `toast.autovacuum_enabled=off` set in its table storage parameters (`reloptions`). While `toast.autovacuum_enabled` is disabled, PostgreSQL never automatically vacuums dead tuples or updates planner statistics on its TOAST table, allowing bloat to accumulate until transaction ID wraparound forces an emergency anti-wraparound vacuum.
impact: toast.autovacuum_enabled = false
metric: Live rows
fix: Re-enable autovacuum on public.customers by resetting `toast.autovacuum_enabled`.
sql: ALTER TABLE public.customers RESET (toast.autovacuum_enabled);
cost: Re-enabling autovacuum schedules a background vacuum pass if dead rows exceed the table's threshold.
verify: Re-scan after resetting `toast.autovacuum_enabled`: `toast.autovacuum_enabled=off` should no longer appear on public.customers.
--
dead-rows:autovacuum-disabled:public.products · Medium · High · NeedsAttention · score 22.00
title: Autovacuum is disabled on products (autovacuum_enabled = false)
summary: public.products has `autovacuum_enabled=false` set in its table storage parameters (`reloptions`). While `autovacuum_enabled` is disabled, PostgreSQL never automatically vacuums dead tuples or updates planner statistics on products, allowing bloat to accumulate until transaction ID wraparound forces an emergency anti-wraparound vacuum.
impact: autovacuum_enabled = false
metric: Live rows
metric: Dead rows
fix: Re-enable autovacuum on public.products by resetting `autovacuum_enabled`.
sql: ALTER TABLE public.products RESET (autovacuum_enabled);
cost: Re-enabling autovacuum schedules a background vacuum pass if dead rows exceed the table's threshold.
verify: Re-scan after resetting `autovacuum_enabled`: `autovacuum_enabled=false` should no longer appear on public.products."#,
    ),
];
const STALE_STATS: &[(&str, &str)] = &[
    (
        "auto",
        r#"stale-stats:shop.products · Low · Medium · Information · score 10.25
title: Statistics on products have 15K recorded changes
summary: 15K changes to shop.products (20K rows) since its statistics were last updated (2026-10-01). Automatic statistics updates are enabled or were not reported disabled. A saved plan's statistics list describes its captured compilation; it does not establish which version later compilations or current executions use. Catalog modification counts alone do not demonstrate a slow plan or justify an immediate refresh.
impact: 15K changes
metric: Changes since update
metric: Rows in table
metric: Statistics: st_products
fix: Inspect current statistics metadata and a newly compiled plan under representative load if this query is slow. Refresh only when that evidence supports it; the historical plan reference is not a request to update statistics now.
verify: Compare the current catalog update time and modification counter with a newly compiled plan's estimates and measured rows. An older saved XML reference may remain after maintenance, and does not prove that the change failed.
caveat: Automatic updates depend on the engine's thresholds and settings, including asynchronous updates and per-statistic NORECOMPUTE; this scan does not establish when every statistic will refresh."#,
    ),
    (
        "auto-undated",
        r#"stale-stats:shop.products · Low · Medium · Information · score 10.25
title: Statistics on products have 15K recorded changes
summary: 15K changes to shop.products (20K rows) since its statistics were last updated. Automatic statistics updates are enabled or were not reported disabled. A saved plan's statistics list describes its captured compilation; it does not establish which version later compilations or current executions use. Catalog modification counts alone do not demonstrate a slow plan or justify an immediate refresh.
impact: 15K changes
metric: Changes since update
metric: Rows in table
metric: Statistics: st_products
fix: Inspect current statistics metadata and a newly compiled plan under representative load if this query is slow. Refresh only when that evidence supports it; the historical plan reference is not a request to update statistics now.
verify: Compare the current catalog update time and modification counter with a newly compiled plan's estimates and measured rows. An older saved XML reference may remain after maintenance, and does not prove that the change failed.
caveat: Automatic updates depend on the engine's thresholds and settings, including asynchronous updates and per-statistic NORECOMPUTE; this scan does not establish when every statistic will refresh."#,
    ),
    (
        "auto-two",
        r#"stale-stats:shop.products · Low · Medium · Information · score 11.90
title: Statistics on products have 26K recorded changes
summary: 26K changes to shop.products (20K rows) since st_b was last updated (2026-09-08), and 8.0K since st_a was (2026-10-13). Automatic statistics updates are enabled or were not reported disabled. A saved plan's statistics list describes its captured compilation; it does not establish which version later compilations or current executions use. Catalog modification counts alone do not demonstrate a slow plan or justify an immediate refresh.
impact: 26K changes
metric: Changes since update
metric: Rows in table
metric: Statistics: st_a and st_b
fix: Inspect current statistics metadata and a newly compiled plan under representative load if this query is slow. Refresh only when that evidence supports it; the historical plan reference is not a request to update statistics now.
verify: Compare the current catalog update time and modification counter with a newly compiled plan's estimates and measured rows. An older saved XML reference may remain after maintenance, and does not prove that the change failed.
caveat: Automatic updates depend on the engine's thresholds and settings, including asynchronous updates and per-statistic NORECOMPUTE; this scan does not establish when every statistic will refresh."#,
    ),
    (
        "auto-three",
        r#"stale-stats:shop.products · Low · Medium · Information · score 11.90
title: Statistics on products have 26K recorded changes
summary: 26K changes to shop.products (20K rows) since st_a was last updated (2026-10-13); the 2 other statistics listed below are past SQL Server's own threshold too. Automatic statistics updates are enabled or were not reported disabled. A saved plan's statistics list describes its captured compilation; it does not establish which version later compilations or current executions use. Catalog modification counts alone do not demonstrate a slow plan or justify an immediate refresh.
impact: 26K changes
metric: Changes since update
metric: Rows in table
metric: Statistics: st_a, st_b and st_c
fix: Inspect current statistics metadata and a newly compiled plan under representative load if this query is slow. Refresh only when that evidence supports it; the historical plan reference is not a request to update statistics now.
verify: Compare the current catalog update time and modification counter with a newly compiled plan's estimates and measured rows. An older saved XML reference may remain after maintenance, and does not prove that the change failed.
caveat: Automatic updates depend on the engine's thresholds and settings, including asynchronous updates and per-statistic NORECOMPUTE; this scan does not establish when every statistic will refresh."#,
    ),
    (
        "manual-hot",
        r#"stale-stats:shop.customers · Medium · Medium · OptionalImprovement · score 24.25
title: Statistics on customers are stale: 150K changes since they were updated (75% of the table's rows)
summary: 150K changes to shop.customers (200K rows) since its statistics were last updated (2026-10-01). The optimizer uses these statistics for row estimates. Check current estimates against actual rows before choosing maintenance; modification counts alone do not measure query harm. Heavy queries in this workload read this table.
impact: 150K changes
metric: Changes since update
metric: Rows in table
metric: Statistics: st_customers
fix: Update the statistics now; if this table changes fast, schedule the update in a maintenance job.
sql: UPDATE STATISTICS [shop].[customers];
cost: Reads a sample of the table; run it outside peak hours on very large tables.
verify: After maintenance and representative workload, compare the changes-since-update counter on customers and fresh plan estimates with actual rows; check measured query time as well."#,
    ),
    (
        "manual",
        r#"stale-stats:shop.ticket_events · Low · Medium · OptionalImprovement · score 9.12
title: Statistics on ticket_events are stale: 300K changes since they were updated (38% of the table's rows)
summary: 300K changes to shop.ticket_events (800K rows) since its statistics were last updated (2026-10-01). The optimizer uses these statistics for row estimates. Check current estimates against actual rows before choosing maintenance; modification counts alone do not measure query harm.
impact: 300K changes
metric: Changes since update
metric: Rows in table
metric: Statistics: st_events
fix: Update the statistics now; if this table changes fast, schedule the update in a maintenance job.
sql: UPDATE STATISTICS [shop].[ticket_events];
cost: Reads a sample of the table; run it outside peak hours on very large tables.
verify: After maintenance and representative workload, compare the changes-since-update counter on ticket_events and fresh plan estimates with actual rows; check measured query time as well."#,
    ),
    (
        "postgres",
        r#"stale-stats:public.orders · Medium · Medium · OptionalImprovement · score 24.25
title: Statistics on orders are stale: 1.5M changes since they were updated (75% of the table's rows)
summary: 1.5M changes to public.orders (2.0M rows) since its statistics were last updated (2026-10-01). The optimizer uses these statistics for row estimates. Check current estimates against actual rows before choosing maintenance; modification counts alone do not measure query harm. Heavy queries in this workload read this table.
impact: 1.5M changes
metric: Changes since update
metric: Rows in table
fix: Analyze the table now. If autovacuum falls behind on it regularly, lower its analyze threshold for this table only.
sql: ANALYZE public.orders;
ALTER TABLE public.orders SET (autovacuum_analyze_scale_factor = 0.02);
cost: ANALYZE reads a sample of the table and does not block reads or writes.
verify: After maintenance and representative workload, compare the changes-since-update counter on orders and fresh plan estimates with actual rows; check measured query time as well.
--
stale-stats:public.customers · Low · Medium · OptionalImprovement · score 9.50
title: Statistics on customers are stale: 100K changes since they were updated (50% of the table's rows)
summary: 100K changes to public.customers (200K rows) since its statistics were last updated. The optimizer uses these statistics for row estimates. Check current estimates against actual rows before choosing maintenance; modification counts alone do not measure query harm.
impact: 100K changes
metric: Changes since update
metric: Rows in table
fix: Analyze the table now. If autovacuum falls behind on it regularly, lower its analyze threshold for this table only.
sql: ANALYZE public.customers;
ALTER TABLE public.customers SET (autovacuum_analyze_scale_factor = 0.02);
cost: ANALYZE reads a sample of the table and does not block reads or writes.
verify: After maintenance and representative workload, compare the changes-since-update counter on customers and fresh plan estimates with actual rows; check measured query time as well."#,
    ),
];
