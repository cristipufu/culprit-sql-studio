//! What the deadlocks check says, word for word, for each kind of title, count, victim and source: its messages come
//! from locales/en.json.

use crate::pins::{fixture, info, pin, pin_text, run};
use culprit_core::deadlock::classify;
use culprit_core::{Deadlock, DeadlockProcess, DeadlockReport, DeadlockResource, Engine, LockHolder, Snapshot};

fn process(id: &str, victim: bool, statement: &str) -> DeadlockProcess {
    DeadlockProcess {
        id: id.into(),
        session: Some(id.into()),
        victim,
        statement: Some(statement.into()),
        application: Some("orders-api".into()),
        database: Some("culprit_demo".into()),
        ..Default::default()
    }
}

fn resource(object: Option<&str>, index: &str, holder: &str, waiter: &str) -> DeadlockResource {
    DeadlockResource {
        id: format!("{object:?}{index}{holder}"),
        kind: "keylock".into(),
        object: object.map(str::to_string),
        index: Some(index.into()),
        holders: vec![LockHolder { process: holder.into(), mode: Some("X".into()) }],
        waiters: vec![LockHolder { process: waiter.into(), mode: Some("X".into()) }],
    }
}

/// A read through an index of customers, and an update of the same rows: SQL Server's key-lookup deadlock.
fn key_lookup(at: Option<&str>) -> Deadlock {
    let mut d = Deadlock::new(
        "system_health",
        at.map(str::to_string),
        vec![
            process("61", true, "SELECT name FROM shop.customers WHERE email = @p0"),
            process("64", false, "UPDATE shop.customers SET email = @p0 WHERE id = @p1"),
        ],
        vec![
            DeadlockResource {
                waiters: vec![LockHolder { process: "61".into(), mode: Some("S".into()) }],
                ..resource(Some("shop.customers"), "ix_customers_email", "64", "61")
            },
            DeadlockResource {
                waiters: vec![LockHolder { process: "64".into(), mode: Some("X".into()) }],
                ..resource(Some("shop.customers"), "PK_customers", "61", "64")
            },
        ],
    );
    classify(Engine::Mssql, &mut d);
    d
}

/// `processes` transactions, each holding a row of its table and waiting for the next one's.
fn cycle(engine: Engine, at: Option<&str>, tables: &[Option<&str>], processes: usize) -> Deadlock {
    let ids: Vec<String> = (0..processes).map(|i| format!("{}", 70 + i)).collect();
    let mut d = Deadlock::new(
        "system_health",
        at.map(str::to_string),
        ids.iter()
            .enumerate()
            .map(|(i, id)| process(id, i == 0, &format!("UPDATE t{i} SET v = v + 1 WHERE id = {i}")))
            .collect(),
        ids.iter()
            .enumerate()
            .map(|(i, id)| resource(tables[i % tables.len()], &format!("PK_{i}"), id, &ids[(i + 1) % ids.len()]))
            .collect(),
    );
    classify(engine, &mut d);
    d
}

fn with(mut s: Snapshot, deadlocks: Vec<Deadlock>, since_hours: f64, note: Option<&str>) -> Snapshot {
    s.deadlocks = Some(DeadlockReport {
        deadlocks,
        source: "system_health event files".into(),
        since_hours,
        note: note.map(str::to_string),
        ..Default::default()
    });
    s
}

#[test]
fn deadlocks_say_how_often_and_who_was_rolled_back() {
    pin_text(
        "deadlocks_info",
        &info("deadlocks"),
        r#"title: Deadlocks
description: Deadlocks in the last day, grouped by pattern (key lookup, opposite order, read-then-update, lock escalation, parallelism) with the fix that removes them. SQL Server reads system_health (Azure SQL Database its deadlock telemetry), MySQL the latest InnoDB report, PostgreSQL only keeps a counter."#,
    );
    // Six dated in the last day and one whose time is unknown; the victim named by session and application.
    let mut list: Vec<Deadlock> = (0..6).map(|i| key_lookup(Some(&format!("2026-10-11T1{i}:00:00Z")))).collect();
    list.push(key_lookup(None));
    let s = with(fixture("mssql-demo.json"), list, 24.0, None);
    pin(
        "deadlocks_key_lookup_day",
        &run("deadlocks", &s),
        r#"id: deadlocks:key-lookup:shop.customers · Medium · High · score 28 · share None
title: Two queries deadlocked 6 times in the last day, and 1 more at an unknown time (key-lookup deadlock on shop.customers)
summary: Key-lookup deadlock on shop.customers: a read through ix_customers_email and a write to the same rows took their locks in opposite order; session 61's transaction was rolled back. The read finds rows through ix_customers_email and then looks each one up in the table's main index; the write changes the main index first and then ix_customers_email. Each ends up holding the lock the other needs next. It recurs whenever the two statements touch the same rows at the same time.
impact: 7 deadlocks
metric: Deadlocks: 7 Count
metric: Pattern: key-lookup deadlock
metric: Rolled back: session 61 (orders-api)
fix: Make the read skip the lookup: Add the columns the read needs to ix_customers_email (as INCLUDE columns) so it never touches the main index the write locks. The query's plan in Queries shows the Key Lookup and its output columns. Or let reads stop taking shared locks: With READ_COMMITTED_SNAPSHOT on, reads use row versions instead of shared locks, so a read can't be part of this cycle. It costs some tempdb space for the versions.
sql: ALTER DATABASE [culprit_demo] SET READ_COMMITTED_SNAPSHOT ON WITH ROLLBACK IMMEDIATE;
verify: Re-scan tomorrow: no new key-lookup deadlocks on shop.customers.
verify: Live › Deadlocks shows the graph of each one, with both statements."#,
    );

    // Once in three days, across two tables; the victim known only by its login.
    let mut d = cycle(Engine::Mssql, Some("2026-10-10T09:30:00Z"), &[Some("shop.orders"), Some("shop.customers")], 2);
    for p in &mut d.processes {
        (p.session, p.application, p.login) = (None, None, Some("app_user".into()));
    }
    let s = with(fixture("mssql-demo.json"), vec![d], 72.0, None);
    pin(
        "deadlocks_once_in_three_days",
        &run("deadlocks", &s),
        r#"id: deadlocks:opposite-order:shop.customers+shop.orders · Low · High · score 9 · share None
title: Two queries deadlocked once in the last 3 days (opposite access order on shop.customers and shop.orders)
summary: Two transactions locked shop.customers and shop.orders in opposite order and waited on each other; session 70's transaction was rolled back. Session 70 held shop.orders and wanted shop.customers; session 71 held shop.customers and wanted shop.orders. With the same order in both, the second would simply wait instead of deadlocking.
impact: 1 deadlock
metric: Deadlocks: 1 Count
metric: Pattern: opposite access order
metric: Rolled back: a session of login app_user
fix: Touch tables and rows in the same order everywhere: Make every code path that changes shop.customers and shop.orders do it in one order (for example, always the parent row first, and rows sorted by key). Then the second transaction waits instead of deadlocking.
verify: Re-scan tomorrow: no new opposite access order deadlocks on shop.customers and shop.orders.
verify: Live › Deadlocks shows the graph of each one, with both statements."#,
    );

    // Three transactions over three tables, four over rows of no named table; victims by session alone, or unnamed.
    let three = cycle(
        Engine::Mssql,
        Some("2026-10-11T10:00:00Z"),
        &[Some("shop.orders"), Some("shop.customers"), Some("shop.products")],
        3,
    );
    let mut four = cycle(Engine::Mssql, Some("2026-10-11T11:00:00Z"), &[None], 4);
    for p in &mut four.processes {
        p.application = None;
    }
    let mut unnamed = cycle(Engine::Mssql, Some("2026-10-11T11:30:00Z"), &[Some("shop.payments")], 2);
    for p in &mut unnamed.processes {
        (p.session, p.application) = (None, None);
    }
    // Two of the same pattern whose statements differ: two transactions, not two queries.
    let tables = [Some("shop.invoices"), Some("shop.payments")];
    let first = cycle(Engine::Mssql, Some("2026-10-11T08:00:00Z"), &tables, 2);
    let mut second = cycle(Engine::Mssql, Some("2026-10-11T09:00:00Z"), &tables, 2);
    for p in &mut second.processes {
        p.statement = p.statement.as_ref().map(|s| format!("{s} AND v > 0"));
    }
    let s = with(fixture("mssql-demo.json"), vec![three, four, unnamed, first, second], 24.0, None);
    pin(
        "deadlocks_many_transactions",
        &run("deadlocks", &s),
        r#"id: deadlocks:key-lookup:shop.payments · Low · High · score 9 · share None
title: Two queries deadlocked once in the last day (key-lookup deadlock on shop.payments)
summary: Key-lookup deadlock on shop.payments: a read through a secondary index and a write to the same rows took their locks in opposite order; session 70's transaction was rolled back. The read finds rows through a secondary index and then looks each one up in the table's main index; the write changes the main index first and then the secondary index. Each ends up holding the lock the other needs next. It recurs whenever the two statements touch the same rows at the same time.
impact: 1 deadlock
metric: Deadlocks: 1 Count
metric: Pattern: key-lookup deadlock
fix: Make the read skip the lookup: Add the columns the read needs to the index it seeks (as INCLUDE columns) so it never touches the main index the write locks. The query's plan in Queries shows the Key Lookup and its output columns. Or let reads stop taking shared locks: With READ_COMMITTED_SNAPSHOT on, reads use row versions instead of shared locks, so a read can't be part of this cycle. It costs some tempdb space for the versions.
sql: ALTER DATABASE [culprit_demo] SET READ_COMMITTED_SNAPSHOT ON WITH ROLLBACK IMMEDIATE;
verify: Re-scan tomorrow: no new key-lookup deadlocks on shop.payments.
verify: Live › Deadlocks shows the graph of each one, with both statements.

id: deadlocks:opposite-order:unknown · Low · High · score 9 · share None
title: 4 transactions deadlocked once in the last day (opposite access order on the same rows)
summary: Two transactions locked the same data in opposite order and waited on each other; session 70's transaction was rolled back. Each transaction locked one thing and then wanted what the other had already locked. With the same order in both, the second would simply wait instead of deadlocking.
impact: 1 deadlock
metric: Deadlocks: 1 Count
metric: Pattern: opposite access order
metric: Rolled back: session 70
fix: Touch tables and rows in the same order everywhere: Make every code path that changes the same data do it in one order (for example, always the parent row first, and rows sorted by key). Then the second transaction waits instead of deadlocking.
verify: Re-scan tomorrow: no new opposite access order deadlocks on the same rows.
verify: Live › Deadlocks shows the graph of each one, with both statements.

id: deadlocks:opposite-order:shop.customers+shop.orders+shop.products · Low · High · score 9 · share None
title: Three transactions deadlocked once in the last day (opposite access order on shop.customers, shop.orders and shop.products)
summary: Two transactions locked shop.customers, shop.orders and shop.products in opposite order and waited on each other; session 70's transaction was rolled back. Session 70 held shop.orders and wanted shop.products; session 71 held shop.customers and wanted shop.orders; session 72 held shop.products and wanted shop.customers. With the same order in both, the second would simply wait instead of deadlocking.
impact: 1 deadlock
metric: Deadlocks: 1 Count
metric: Pattern: opposite access order
metric: Rolled back: session 70 (orders-api)
fix: Touch tables and rows in the same order everywhere: Make every code path that changes shop.customers, shop.orders and shop.products do it in one order (for example, always the parent row first, and rows sorted by key). Then the second transaction waits instead of deadlocking.
verify: Re-scan tomorrow: no new opposite access order deadlocks on shop.customers, shop.orders and shop.products.
verify: Live › Deadlocks shows the graph of each one, with both statements.

id: deadlocks:opposite-order:shop.invoices+shop.payments · Low · High · score 10 · share None
title: Two transactions deadlocked 2 times in the last day (opposite access order on shop.invoices and shop.payments)
summary: Two transactions locked shop.invoices and shop.payments in opposite order and waited on each other; session 70's transaction was rolled back. Session 70 held shop.invoices and wanted shop.payments; session 71 held shop.payments and wanted shop.invoices. With the same order in both, the second would simply wait instead of deadlocking.
impact: 2 deadlocks
metric: Deadlocks: 2 Count
metric: Pattern: opposite access order
metric: Rolled back: session 70 (orders-api)
fix: Touch tables and rows in the same order everywhere: Make every code path that changes shop.invoices and shop.payments do it in one order (for example, always the parent row first, and rows sorted by key). Then the second transaction waits instead of deadlocking.
verify: Re-scan tomorrow: no new opposite access order deadlocks on shop.invoices and shop.payments.
verify: Live › Deadlocks shows the graph of each one, with both statements."#,
    );

    // Only one whose time is unknown.
    let s = with(fixture("mssql-demo.json"), vec![key_lookup(None)], 24.0, None);
    pin(
        "deadlocks_undated",
        &run("deadlocks", &s),
        r#"id: deadlocks:key-lookup:shop.customers · Low · Medium · score 8 · share None
title: Two queries deadlocked at an unknown time (key-lookup deadlock on shop.customers): the server's latest deadlock report doesn't say when
summary: Key-lookup deadlock on shop.customers: a read through ix_customers_email and a write to the same rows took their locks in opposite order; session 61's transaction was rolled back. The read finds rows through ix_customers_email and then looks each one up in the table's main index; the write changes the main index first and then ix_customers_email. Each ends up holding the lock the other needs next. It recurs whenever the two statements touch the same rows at the same time.
impact: 1 deadlock
metric: Deadlocks: 1 Count
metric: Pattern: key-lookup deadlock
metric: Rolled back: session 61 (orders-api)
fix: Make the read skip the lookup: Add the columns the read needs to ix_customers_email (as INCLUDE columns) so it never touches the main index the write locks. The query's plan in Queries shows the Key Lookup and its output columns. Or let reads stop taking shared locks: With READ_COMMITTED_SNAPSHOT on, reads use row versions instead of shared locks, so a read can't be part of this cycle. It costs some tempdb space for the versions.
sql: ALTER DATABASE [culprit_demo] SET READ_COMMITTED_SNAPSHOT ON WITH ROLLBACK IMMEDIATE;
verify: Re-scan tomorrow: no new key-lookup deadlocks on shop.customers.
verify: Live › Deadlocks shows the graph of each one, with both statements.
caveat: When it happened is unknown, so it may be from long ago: InnoDB keeps its most recent deadlock until the next one, however old. Check the error log (innodb_print_all_deadlocks) before acting on it."#,
    );
}

#[test]
fn innodb_says_its_latest_deadlock_is_a_floor() {
    let mysql = fixture("mysql-demo.json");
    let table = format!("{}.orders", mysql.server.database);
    let mut d = cycle(Engine::Mysql, Some("2026-10-11T08:00:00Z"), &[Some(table.as_str())], 2);
    for p in &mut d.processes {
        p.database = None;
    }
    let mut undated = d.clone();
    undated.at = None;
    undated.objects = vec![format!("{}.customers", mysql.server.database)];
    let s = with(
        mysql,
        vec![d, undated],
        24.0,
        Some("InnoDB only keeps the most recent deadlock: turn on innodb_print_all_deadlocks to log every one."),
    );
    pin(
        "deadlocks_innodb_latest",
        &run("deadlocks", &s),
        r#"id: deadlocks:key-lookup:shop.customers · Low · Medium · score 8 · share None
title: Two queries deadlocked at an unknown time (key-lookup deadlock on shop.customers): the server's latest deadlock report doesn't say when
summary: Key-lookup deadlock on shop.orders: a read through a secondary index and a write to the same rows took their locks in opposite order; session 70's transaction was rolled back. The read finds rows through a secondary index and then looks each one up in the table's main index; the write changes the main index first and then the secondary index. Each ends up holding the lock the other needs next. It recurs whenever the two statements touch the same rows at the same time.
impact: at least 1 deadlock
metric: Deadlocks (at least): 1 Count
metric: Pattern: key-lookup deadlock
metric: Rolled back: session 70 (orders-api)
fix: Change rows through the primary key: Look up the primary key values first, then update or lock by primary key, so both statements lock the main index in the same order.
verify: Re-scan tomorrow: no new key-lookup deadlocks on shop.customers.
verify: Live › Deadlocks shows the graph of each one, with both statements.
caveat: When it happened is unknown, so it may be from long ago: InnoDB keeps its most recent deadlock until the next one, however old. Check the error log (innodb_print_all_deadlocks) before acting on it.
caveat: InnoDB only keeps the most recent deadlock: turn on innodb_print_all_deadlocks to log every one.

id: deadlocks:key-lookup:shop.orders · Low · High · score 9 · share None
title: Two queries deadlocked at least once, most recently on Oct 11 at 08:00 UTC (key-lookup deadlock on shop.orders)
summary: Key-lookup deadlock on shop.orders: a read through a secondary index and a write to the same rows took their locks in opposite order; session 70's transaction was rolled back. The read finds rows through a secondary index and then looks each one up in the table's main index; the write changes the main index first and then the secondary index. Each ends up holding the lock the other needs next. It recurs whenever the two statements touch the same rows at the same time.
impact: at least 1 deadlock
metric: Deadlocks (at least): 1 Count
metric: Pattern: key-lookup deadlock
metric: Rolled back: session 70 (orders-api)
fix: Change rows through the primary key: Look up the primary key values first, then update or lock by primary key, so both statements lock the main index in the same order.
verify: Re-scan tomorrow: no new key-lookup deadlocks on shop.orders.
verify: Live › Deadlocks shows the graph of each one, with both statements.
caveat: InnoDB only keeps the most recent deadlock: turn on innodb_print_all_deadlocks to log every one."#,
    );
}

#[test]
fn postgres_counts_deadlocks_it_does_not_keep() {
    let mut s = fixture("postgres-demo.json");
    let note = "PostgreSQL counts deadlocks but keeps no graph of them: the server log has each one.";
    s.deadlocks = Some(DeadlockReport {
        counted: Some(400.0),
        counted_since: Some("2026-10-01T00:00:00Z".into()),
        since_hours: 24.0,
        note: Some(note.into()),
        ..Default::default()
    });
    pin(
        "deadlocks_counter_since_reset",
        &run("deadlocks", &s),
        r#"id: deadlocks:counter · Medium · Medium · score 22 · share None
title: PostgreSQL resolved 400 deadlocks since Oct 1
summary: PostgreSQL counts deadlocks but keeps no graph of them: the server log has each one.
impact: 400 deadlocks
fix: Find "deadlock detected" in the server log and paste those lines into Live › Deadlocks: Culprit names the pattern. Most are fixed by changing rows in the same order everywhere and keeping transactions short.
verify: Re-scan later: the counter stops growing."#,
    );
    s.deadlocks = Some(DeadlockReport { counted: Some(1.0), since_hours: 24.0, ..Default::default() });
    pin(
        "deadlocks_counter_one_no_reset",
        &run("deadlocks", &s),
        r#"id: deadlocks:counter · Low · Medium · score 8 · share None
title: PostgreSQL resolved 1 deadlock since its statistics began (no reset on record, so it doesn't say when)
summary: 
impact: 1 deadlock
fix: Find "deadlock detected" in the server log and paste those lines into Live › Deadlocks: Culprit names the pattern. Most are fixed by changing rows in the same order everywhere and keeping transactions short.
verify: Re-scan later: the counter stops growing."#,
    );
}
