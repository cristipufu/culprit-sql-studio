//! What the SQL editor's linter says, word for word, for every statement it flags on each engine. Its words come from
//! the language catalog (locales/en.json); these tests hold the English to what the linter said when it wrote it in
//! code.

use culprit_checks::lint_sql;
use culprit_core::{ColumnInfo, Engine, Finding, Snapshot};

fn snapshot(name: &str) -> Snapshot {
    let text =
        std::fs::read_to_string(format!("{}/../../fixtures/{name}", env!("CARGO_MANIFEST_DIR"))).expect("fixture");
    serde_json::from_str(&text).expect("snapshot")
}

fn column(table: &str, name: &str, data_type: &str, schema: &str) -> ColumnInfo {
    ColumnInfo {
        schema: schema.into(),
        table: table.into(),
        name: name.into(),
        data_type: data_type.into(),
        nullable: true,
        default: None,
        generated: false,
        ordinal: 99,
    }
}

/// Everything a finding says, one line a part.
fn words(findings: &[Finding]) -> String {
    let mut out = Vec::new();
    for f in findings {
        out.push(format!("== {} · {:?}", f.id, f.severity));
        out.push(format!("title: {}", f.title));
        out.push(format!("summary: {}", f.summary));
        out.push(format!("impact: {}", f.impact.label));
        out.extend(f.metrics.iter().map(|m| format!("metric: {}", m.label)));
        if let Some(fix) = &f.fix {
            out.push(format!("fix: {}", fix.summary));
            if let Some(sql) = &fix.sql {
                out.push(format!("sql: {sql}"));
            }
            if let Some(cost) = &fix.cost {
                out.push(format!("cost: {cost}"));
            }
        }
        out.extend(f.verify.iter().map(|v| format!("verify: {v}")));
        out.extend(f.caveats.iter().map(|c| format!("caveat: {c}")));
    }
    out.join("\n")
}

#[track_caller]
fn same(actual: &str, expected: &str) {
    assert!(actual == expected, "said:\n{actual}\n\nwhere the English was:\n{expected}");
}

#[test]
fn postgres_migrations_say_what_they_block_in_english() {
    let mut s = snapshot("postgres-demo.json");
    s.columns.push(column("orders", "customer_ref", "bigint", "public"));
    let sql = "CREATE INDEX idx_orders_created ON orders (created_at);\n\
               DROP INDEX customers_pkey;\n\
               TRUNCATE TABLE customers;\n\
               VACUUM FULL orders;\n\
               CLUSTER orders USING orders_pkey;\n\
               REINDEX TABLE orders;\n\
               REINDEX INDEX orders_pkey;\n\
               ALTER TABLE orders ADD COLUMN required_code text NOT NULL;\n\
               ALTER TABLE orders ADD CONSTRAINT uq_code UNIQUE (required_code);\n\
               ALTER TABLE orders ADD PRIMARY KEY (id);\n\
               ALTER TABLE orders ADD CONSTRAINT chk_positive CHECK (total > 0);\n\
               ALTER TABLE orders ADD COLUMN token uuid DEFAULT gen_random_uuid();\n\
               ALTER TABLE orders ALTER COLUMN total TYPE numeric(14, 4);\n\
               ALTER TABLE orders ALTER COLUMN status SET NOT NULL;\n\
               ALTER TABLE orders ADD CONSTRAINT fk_customer FOREIGN KEY (customer_ref) REFERENCES customers (id);\n\
               DROP TABLE customers;\n\
               ALTER TABLE orders DROP COLUMN note;\n\
               ALTER TABLE orders RENAME COLUMN status TO state;\n\
               ALTER TABLE orders RENAME TO orders_old;";
    same(
        &words(&lint_sql(&s, sql)),
        r#"== sql-lint:blocking-index:public.orders · High
title: CREATE INDEX on orders (2.0M rows, 380 MB) without CONCURRENTLY blocks writes until the build finishes
summary: The statement `CREATE INDEX idx_orders_created ON orders (created_at)` builds an index on public.orders (2.0M rows, 380 MB) without `CONCURRENTLY`. Standard `CREATE INDEX` in PostgreSQL acquires a `SHARE` lock on the table for the entire index build, blocking all `INSERT`, `UPDATE` and `DELETE` statements.
impact: 2.0M rows, 380 MB
metric: Rows in snapshot
metric: Table size in snapshot
fix: Use `CREATE INDEX CONCURRENTLY` (outside a transaction block) so writes continue while the index builds.
sql: CREATE INDEX CONCURRENTLY idx_orders_created ON orders (created_at);
cost: CONCURRENTLY performs two table scans and cannot run inside a transaction block.
verify: Re-run `culprit lint-sql` on the updated migration to confirm no blocking index builds remain.
== sql-lint:blocking-drop-index:customers_pkey · Medium
title: DROP INDEX customers_pkey on customers (200K rows, 22.0 MB) without CONCURRENTLY acquires an ACCESS EXCLUSIVE lock
summary: The statement `DROP INDEX customers_pkey` drops `customers_pkey` on public.customers (200K rows, 22.0 MB) without `CONCURRENTLY`. Standard `DROP INDEX` in PostgreSQL acquires an `ACCESS EXCLUSIVE` lock on the table, blocking all concurrent reads and writes until in-flight queries finish.
impact: 200K rows, 22.0 MB
metric: Rows in snapshot
metric: Table size in snapshot
fix: Use `DROP INDEX CONCURRENTLY` (outside a transaction block) so concurrent reads and writes are not blocked.
sql: DROP INDEX CONCURRENTLY IF EXISTS customers_pkey;
cost: DROP INDEX CONCURRENTLY waits for active transactions using the index and cannot run inside a transaction block.
verify: Re-run `culprit lint-sql` after switching to `DROP INDEX CONCURRENTLY`.
== sql-lint:truncate:public.customers · High
title: TRUNCATE on customers (200K rows, 22.0 MB) acquires an exclusive table lock and deletes all rows
summary: The statement `TRUNCATE TABLE customers` truncates public.customers (200K rows, 22.0 MB). `TRUNCATE` acquires an `ACCESS EXCLUSIVE` / schema-modification lock on the table, blocking all concurrent queries while deallocating its storage.
impact: 200K rows, 22.0 MB
metric: Rows in snapshot
metric: Table size in snapshot
verify: Confirm `TRUNCATE` is intended for this environment before running.
== sql-lint:vacuum-full:public.orders · High
title: VACUUM FULL on orders (2.0M rows, 380 MB) rewrites the entire table under an ACCESS EXCLUSIVE lock
summary: The statement `VACUUM FULL orders` runs `VACUUM FULL` on public.orders (2.0M rows, 380 MB). Unlike standard `VACUUM`, `VACUUM FULL` rewrites every row and index into new disk files while holding an `ACCESS EXCLUSIVE` lock, blocking all concurrent reads and writes.
impact: 2.0M rows, 380 MB
metric: Rows in snapshot
metric: Table size in snapshot
fix: Run standard `VACUUM (ANALYZE)` (which reclaims dead tuples online without blocking reads or writes), or use `pg_repack` if physical table compaction is required.
sql: VACUUM (ANALYZE) public.orders;
verify: Replace `VACUUM FULL` with `VACUUM (ANALYZE)` or `pg_repack`.
== sql-lint:cluster:public.orders · High
title: CLUSTER on orders (2.0M rows, 380 MB) rewrites the table under an ACCESS EXCLUSIVE lock
summary: The statement `CLUSTER orders USING orders_pkey` runs `CLUSTER` on public.orders (2.0M rows, 380 MB). `CLUSTER` physically reorders and rewrites the table while holding an `ACCESS EXCLUSIVE` lock, blocking all concurrent reads and writes.
impact: 2.0M rows, 380 MB
metric: Rows in snapshot
metric: Table size in snapshot
fix: Use `pg_repack` to reorder a large production table online without holding an `ACCESS EXCLUSIVE` lock.
verify: Avoid running `CLUSTER` directly on large production tables during traffic.
== sql-lint:blocking-reindex:public.orders · High
title: REINDEX TABLE orders (2.0M rows, 380 MB) without CONCURRENTLY blocks reads and writes
summary: The statement `REINDEX TABLE orders` rebuilds all indexes on public.orders (2.0M rows, 380 MB) without `CONCURRENTLY`. Standard `REINDEX TABLE` holds `SHARE` and `ACCESS EXCLUSIVE` locks that block concurrent writes and reads.
impact: 2.0M rows, 380 MB
metric: Rows in snapshot
metric: Table size in snapshot
fix: Use `REINDEX TABLE CONCURRENTLY` (outside a transaction block) so concurrent reads and writes can continue.
sql: REINDEX TABLE CONCURRENTLY public.orders;
cost: REINDEX CONCURRENTLY cannot run inside a transaction block.
verify: Re-run `culprit lint-sql` after switching to `REINDEX TABLE CONCURRENTLY`.
== sql-lint:blocking-reindex:orders_pkey · High
title: REINDEX INDEX orders_pkey on orders (2.0M rows, 380 MB) without CONCURRENTLY blocks writes
summary: The statement `REINDEX INDEX orders_pkey` rebuilds `orders_pkey` on public.orders (2.0M rows, 380 MB) without `CONCURRENTLY`. Standard `REINDEX INDEX` blocks writes on the parent table and reads of the index until the rebuild finishes.
impact: 2.0M rows, 380 MB
metric: Rows in snapshot
metric: Table size in snapshot
fix: Use `REINDEX INDEX CONCURRENTLY` (outside a transaction block) so concurrent reads and writes can continue.
sql: REINDEX INDEX CONCURRENTLY orders_pkey;
cost: REINDEX CONCURRENTLY cannot run inside a transaction block.
verify: Re-run `culprit lint-sql` after switching to `REINDEX INDEX CONCURRENTLY`.
== sql-lint:not-null-without-default:public.orders.required_code · High
title: ADD COLUMN orders.required_code NOT NULL without DEFAULT fails on non-empty table (2.0M rows, 380 MB)
summary: The statement `ALTER TABLE orders ADD COLUMN required_code text NOT NULL` adds `required_code` as `NOT NULL` without a `DEFAULT` clause to public.orders (2.0M rows, 380 MB). Because existing rows would receive `NULL` for `required_code`, the database rejects the statement immediately at runtime.
impact: fails on existing rows
metric: Rows in snapshot
metric: Table size in snapshot
fix: Either supply a non-volatile `DEFAULT` value, or add `required_code` as nullable first, backfill existing rows, and then enforce `NOT NULL`.
verify: Re-run `culprit lint-sql` after adding a `DEFAULT` or making the initial column nullable.
== sql-lint:blocking-unique-constraint:public.orders:uq_code · High
title: ADD UNIQUE constraint on orders (2.0M rows, 380 MB) without USING INDEX builds the index under an ACCESS EXCLUSIVE lock
summary: The statement `ALTER TABLE orders ADD CONSTRAINT uq_code UNIQUE (required_code)` adds a UNIQUE constraint on public.orders (2.0M rows, 380 MB) directly. PostgreSQL builds the backing unique index while holding an `ACCESS EXCLUSIVE` lock on orders, blocking all reads and writes until the index build finishes.
impact: 2.0M rows, 380 MB
metric: Rows in snapshot
metric: Table size in snapshot
fix: Build the unique index first with `CREATE UNIQUE INDEX CONCURRENTLY`, then attach it with `ALTER TABLE public.orders ADD CONSTRAINT uq_code UNIQUE USING INDEX <index_name>;`.
verify: Re-run `culprit lint-sql` after switching to `CREATE UNIQUE INDEX CONCURRENTLY` + `ADD CONSTRAINT ... USING INDEX`.
== sql-lint:blocking-unique-constraint:public.orders:primary-key · High
title: ADD PRIMARY KEY constraint on orders (2.0M rows, 380 MB) without USING INDEX builds the index under an ACCESS EXCLUSIVE lock
summary: The statement `ALTER TABLE orders ADD PRIMARY KEY (id)` adds a PRIMARY KEY constraint on public.orders (2.0M rows, 380 MB) directly. PostgreSQL builds the backing unique index while holding an `ACCESS EXCLUSIVE` lock on orders, blocking all reads and writes until the index build finishes.
impact: 2.0M rows, 380 MB
metric: Rows in snapshot
metric: Table size in snapshot
fix: Build the unique index first with `CREATE UNIQUE INDEX CONCURRENTLY`, then attach it with `ALTER TABLE public.orders ADD CONSTRAINT primary-key PRIMARY KEY USING INDEX <index_name>;`.
verify: Re-run `culprit lint-sql` after switching to `CREATE UNIQUE INDEX CONCURRENTLY` + `ADD CONSTRAINT ... USING INDEX`.
== sql-lint:unvalidated-constraint:public.orders:chk_positive · High
title: ADD CHECK constraint on orders (2.0M rows, 380 MB) without NOT VALID scans the table under a lock
summary: The statement `ALTER TABLE orders ADD CONSTRAINT chk_positive CHECK (total > 0)` adds a CHECK constraint on public.orders (2.0M rows, 380 MB) without `NOT VALID`. PostgreSQL validates every existing row immediately while holding a `SHARE ROW EXCLUSIVE` / `ACCESS EXCLUSIVE` lock that blocks writes on orders.
impact: 2.0M rows, 380 MB
metric: Rows in snapshot
metric: Table size in snapshot
fix: Add the CHECK constraint with `NOT VALID` first, then run `ALTER TABLE public.orders VALIDATE CONSTRAINT chk_positive;` in a separate step (which only holds `SHARE UPDATE EXCLUSIVE` and allows concurrent reads and writes).
verify: Re-run `culprit lint-sql` after adding `NOT VALID` to the constraint definition.
== sql-lint:volatile-default:public.orders.token · High
title: ADD COLUMN orders.token with volatile DEFAULT gen_random_uuid() rewrites the table (2.0M rows, 380 MB)
summary: The statement `ALTER TABLE orders ADD COLUMN token uuid DEFAULT gen_random_uuid()` adds `token` to public.orders (2.0M rows, 380 MB) with volatile `DEFAULT gen_random_uuid()`, which cannot use metadata-only fast defaults because every existing row requires a newly evaluated value. The engine rewrites every row and index while holding an exclusive table lock.
impact: 2.0M rows, 380 MB
metric: Rows in snapshot
metric: Table size in snapshot
fix: Add `token` without a default, backfill existing rows in batches, and then set `ALTER TABLE public.orders ALTER COLUMN token SET DEFAULT gen_random_uuid();`.
verify: Verify in staging with `SET lock_timeout = '2s'` before running on production.
== sql-lint:table-rewrite:public.orders · High
title: Column type change on orders (2.0M rows, 380 MB) can rewrite the table under an exclusive lock
summary: The statement `ALTER TABLE orders ALTER COLUMN total TYPE numeric(14, 4)` alters a column type on public.orders (2.0M rows, 380 MB). Changing a column's representation rewrites every row and index while holding an `ACCESS EXCLUSIVE` / schema-modification lock, blocking all reads and writes on orders.
impact: 2.0M rows, 380 MB
metric: Rows in snapshot
metric: Table size in snapshot
fix: Prefer an expand-and-contract migration (add a new column, backfill in small batches, swap reads/writes) unless the engine guarantees a metadata-only cast.
verify: Verify in staging with `SET lock_timeout = '2s'` before running on production.
== sql-lint:set-not-null:public.orders · Medium
title: SET NOT NULL on orders (2.0M rows, 380 MB) scans the table under an ACCESS EXCLUSIVE lock unless backed by a valid constraint
summary: The statement `ALTER TABLE orders ALTER COLUMN status SET NOT NULL` runs `SET NOT NULL` on public.orders (2.0M rows, 380 MB). Without an existing validated `CHECK (col IS NOT NULL)` constraint, PostgreSQL scans the entire table while holding an `ACCESS EXCLUSIVE` lock.
impact: 2.0M rows, 380 MB
metric: Rows in snapshot
metric: Table size in snapshot
fix: Add `CHECK (col IS NOT NULL) NOT VALID`, run `ALTER TABLE ... VALIDATE CONSTRAINT ...` (which only takes `SHARE UPDATE EXCLUSIVE`), and then run `SET NOT NULL`.
verify: Re-run `culprit lint-sql` after splitting the constraint validation step.
== sql-lint:unindexed-fk:public.orders.customer_ref · High
title: New foreign key on orders.(customer_ref) has no supporting index
summary: The statement `ALTER TABLE orders ADD CONSTRAINT fk_customer FOREIGN KEY (customer_ref) REFERENCES customers (id)` adds a foreign key on public.orders.(customer_ref) referencing customers, but neither the snapshot nor the migration defines an index with (customer_ref) as its leading key. Deletes or key updates on the referenced parent table will scan orders on every check.
impact: unindexed FK
fix: Create an index on public.orders (customer_ref) before or alongside the foreign key.
sql: CREATE INDEX CONCURRENTLY ix_orders_customer_ref
    ON public.orders (customer_ref);
verify: Add the supporting `CREATE INDEX` to the migration and re-run `culprit lint-sql`.
== sql-lint:fk-type-mismatch:public.orders.customer_ref · Medium
title: Foreign key orders.customer_ref (bigint) does not match customers.id (integer)
summary: The statement `ALTER TABLE orders ADD CONSTRAINT fk_customer FOREIGN KEY (customer_ref) REFERENCES customers (id)` references public.customers.id (`integer`) from public.orders.customer_ref (`bigint`). Mismatched foreign key types either fail DDL validation or force implicit conversions during joins and referential integrity checks.
impact: bigint vs integer
fix: Align public.orders.customer_ref's type with public.customers.id (`integer`).
verify: Confirm both columns share the same data type before adding the constraint.
== sql-lint:unvalidated-constraint:public.orders:fk_customer · High
title: ADD FOREIGN KEY constraint on orders (2.0M rows, 380 MB) without NOT VALID scans the table under a lock
summary: The statement `ALTER TABLE orders ADD CONSTRAINT fk_customer FOREIGN KEY (customer_ref) REFERENCES customers (id)` adds a FOREIGN KEY constraint on public.orders (2.0M rows, 380 MB) without `NOT VALID`. PostgreSQL validates every existing row immediately while holding a `SHARE ROW EXCLUSIVE` / `ACCESS EXCLUSIVE` lock that blocks writes on orders.
impact: 2.0M rows, 380 MB
metric: Rows in snapshot
metric: Table size in snapshot
fix: Add the FOREIGN KEY constraint with `NOT VALID` first, then run `ALTER TABLE public.orders VALIDATE CONSTRAINT fk_customer;` in a separate step (which only holds `SHARE UPDATE EXCLUSIVE` and allows concurrent reads and writes).
verify: Re-run `culprit lint-sql` after adding `NOT VALID` to the constraint definition.
== sql-lint:workload-break:public.customers · High
title: DROP TABLE on customers breaks 1 active workload query
summary: The migration statement `DROP TABLE customers` runs `DROP TABLE` on `customers`, which is referenced by 1 active statement in the captured workload. Deploying this DDL while the current application code is running will cause immediate query errors.
impact: 1 active query
fix: Stop reading/writing `customers` in application code and deploy that change before running `DROP TABLE` in a later migration.
verify: Verify no active queries in `culprit top` reference `customers` before applying `DROP TABLE`.
== sql-lint:workload-break:public.orders.note · High
title: DROP COLUMN on orders.note breaks 1 active workload query
summary: The migration statement `ALTER TABLE orders DROP COLUMN note` runs `DROP COLUMN` on `orders.note`, which is referenced by 1 active statement in the captured workload. Deploying this DDL while the current application code is running will cause immediate query errors.
impact: 1 active query
fix: Stop reading/writing `orders.note` in application code and deploy that change before running `DROP COLUMN` in a later migration.
verify: Verify no active queries in `culprit top` reference `orders.note` before applying `DROP COLUMN`.
== sql-lint:workload-break:public.orders.status · High
title: RENAME COLUMN on orders.status breaks 2 active workload queries
summary: The migration statement `ALTER TABLE orders RENAME COLUMN status TO state` runs `RENAME COLUMN` on `orders.status`, which is referenced by 2 active statements in the captured workload. Deploying this DDL while the current application code is running will cause immediate query errors.
impact: 2 active queries
fix: Stop reading/writing `orders.status` in application code and deploy that change before running `RENAME COLUMN` in a later migration.
verify: Verify no active queries in `culprit top` reference `orders.status` before applying `RENAME COLUMN`.
== sql-lint:workload-break:public.orders · High
title: RENAME TABLE on orders breaks 3 active workload queries
summary: The migration statement `ALTER TABLE orders RENAME TO orders_old` runs `RENAME TABLE` on `orders`, which is referenced by 3 active statements in the captured workload. Deploying this DDL while the current application code is running will cause immediate query errors.
impact: 3 active queries
fix: Stop reading/writing `orders` in application code and deploy that change before running `RENAME TABLE` in a later migration.
verify: Verify no active queries in `culprit top` reference `orders` before applying `RENAME TABLE`."#,
    );
}

#[test]
fn postgres_concurrent_statements_in_a_transaction_say_so_in_english() {
    let s = snapshot("postgres-demo.json");
    let sql = "BEGIN;\n\
               CREATE INDEX CONCURRENTLY idx_tx ON orders (created_at);\n\
               REINDEX TABLE CONCURRENTLY orders;\n\
               DROP INDEX CONCURRENTLY;\n\
               CREATE INDEX idx_in_tx ON orders (created_at);\n\
               DROP INDEX orders_status_idx;\n\
               REINDEX TABLE orders;\n\
               ALTER TABLE orders ADD CONSTRAINT fk_customer FOREIGN KEY (customer_id) REFERENCES customers (id) NOT VALID;\n\
               COMMIT;";
    same(
        &words(&lint_sql(&s, sql)),
        r#"== sql-lint:concurrent-in-tx:idx_tx · High
title: CREATE INDEX CONCURRENTLY (idx_tx) cannot run inside a transaction block
summary: The statement `CREATE INDEX CONCURRENTLY idx_tx ON orders (created_at)` uses `CONCURRENTLY` inside an explicit `BEGIN ... COMMIT` transaction block. PostgreSQL immediately aborts `CREATE INDEX CONCURRENTLY`, `DROP INDEX CONCURRENTLY` and `REINDEX CONCURRENTLY` when executed inside a transaction block.
impact: aborts in transaction
fix: Run the `CONCURRENTLY` statement outside `BEGIN ... COMMIT` (for example `disable_ddl_transaction!` in Rails, `atomic = False` in Django, or `-- migrate:no-transaction`).
verify: Remove the surrounding `BEGIN`/`COMMIT` and re-run `culprit lint-sql`.
== sql-lint:concurrent-in-tx:orders · High
title: REINDEX TABLE CONCURRENTLY (orders) cannot run inside a transaction block
summary: The statement `REINDEX TABLE CONCURRENTLY orders` uses `CONCURRENTLY` inside an explicit `BEGIN ... COMMIT` transaction block. PostgreSQL immediately aborts `CREATE INDEX CONCURRENTLY`, `DROP INDEX CONCURRENTLY` and `REINDEX CONCURRENTLY` when executed inside a transaction block.
impact: aborts in transaction
fix: Run the `CONCURRENTLY` statement outside `BEGIN ... COMMIT` (for example `disable_ddl_transaction!` in Rails, `atomic = False` in Django, or `-- migrate:no-transaction`).
verify: Remove the surrounding `BEGIN`/`COMMIT` and re-run `culprit lint-sql`.
== sql-lint:concurrent-in-tx:index · High
title: DROP INDEX CONCURRENTLY (index) cannot run inside a transaction block
summary: The statement `DROP INDEX CONCURRENTLY` uses `CONCURRENTLY` inside an explicit `BEGIN ... COMMIT` transaction block. PostgreSQL immediately aborts `CREATE INDEX CONCURRENTLY`, `DROP INDEX CONCURRENTLY` and `REINDEX CONCURRENTLY` when executed inside a transaction block.
impact: aborts in transaction
fix: Run the `CONCURRENTLY` statement outside `BEGIN ... COMMIT` (for example `disable_ddl_transaction!` in Rails, `atomic = False` in Django, or `-- migrate:no-transaction`).
verify: Remove the surrounding `BEGIN`/`COMMIT` and re-run `culprit lint-sql`.
== sql-lint:blocking-index:public.orders · High
title: CREATE INDEX on orders (2.0M rows, 380 MB) without CONCURRENTLY blocks writes until the build finishes
summary: The statement `CREATE INDEX idx_in_tx ON orders (created_at)` builds an index on public.orders (2.0M rows, 380 MB) without `CONCURRENTLY`. Standard `CREATE INDEX` in PostgreSQL acquires a `SHARE` lock on the table for the entire index build, blocking all `INSERT`, `UPDATE` and `DELETE` statements.
impact: 2.0M rows, 380 MB
metric: Rows in snapshot
metric: Table size in snapshot
fix: Use `CREATE INDEX CONCURRENTLY` (outside a transaction block) so writes continue while the index builds.
cost: CONCURRENTLY performs two table scans and cannot run inside a transaction block.
verify: Re-run `culprit lint-sql` on the updated migration to confirm no blocking index builds remain.
== sql-lint:blocking-drop-index:orders_status_idx · High
title: DROP INDEX orders_status_idx on orders (2.0M rows, 380 MB) without CONCURRENTLY acquires an ACCESS EXCLUSIVE lock
summary: The statement `DROP INDEX orders_status_idx` drops `orders_status_idx` on public.orders (2.0M rows, 380 MB) without `CONCURRENTLY`. Standard `DROP INDEX` in PostgreSQL acquires an `ACCESS EXCLUSIVE` lock on the table, blocking all concurrent reads and writes until in-flight queries finish.
impact: 2.0M rows, 380 MB
metric: Rows in snapshot
metric: Table size in snapshot
fix: Use `DROP INDEX CONCURRENTLY` (outside a transaction block) so concurrent reads and writes are not blocked.
cost: DROP INDEX CONCURRENTLY waits for active transactions using the index and cannot run inside a transaction block.
verify: Re-run `culprit lint-sql` after switching to `DROP INDEX CONCURRENTLY`.
== sql-lint:blocking-reindex:public.orders · High
title: REINDEX TABLE orders (2.0M rows, 380 MB) without CONCURRENTLY blocks reads and writes
summary: The statement `REINDEX TABLE orders` rebuilds all indexes on public.orders (2.0M rows, 380 MB) without `CONCURRENTLY`. Standard `REINDEX TABLE` holds `SHARE` and `ACCESS EXCLUSIVE` locks that block concurrent writes and reads.
impact: 2.0M rows, 380 MB
metric: Rows in snapshot
metric: Table size in snapshot
fix: Use `REINDEX TABLE CONCURRENTLY` (outside a transaction block) so concurrent reads and writes can continue.
cost: REINDEX CONCURRENTLY cannot run inside a transaction block.
verify: Re-run `culprit lint-sql` after switching to `REINDEX TABLE CONCURRENTLY`.
== sql-lint:unindexed-fk:public.orders.customer_id · High
title: New foreign key on orders.(customer_id) has no supporting index
summary: The statement `ALTER TABLE orders ADD CONSTRAINT fk_customer FOREIGN KEY (customer_id) REFERENCES customers (id) NOT VALID` adds a foreign key on public.orders.(customer_id) referencing customers, but neither the snapshot nor the migration defines an index with (customer_id) as its leading key. Deletes or key updates on the referenced parent table will scan orders on every check.
impact: unindexed FK
fix: Create an index on public.orders (customer_id) before or alongside the foreign key.
verify: Add the supporting `CREATE INDEX` to the migration and re-run `culprit lint-sql`."#,
    );
}

#[test]
fn table_sizes_are_said_with_what_the_snapshot_knows() {
    let mut s = snapshot("postgres-demo.json");
    // Rows without a size, and a size without rows.
    for t in &mut s.tables {
        if t.name == "customers" {
            (t.total_bytes, t.data_bytes) = (None, None);
        }
        if t.name == "order_items" {
            (t.rows, t.storage, t.total_bytes) = (None, None, Some(500.0 * 1024.0 * 1024.0));
        }
    }
    let sql = "VACUUM FULL customers;\nVACUUM FULL order_items;";
    same(
        &words(&lint_sql(&s, sql)),
        r#"== sql-lint:vacuum-full:public.customers · High
title: VACUUM FULL on customers (200K rows) rewrites the entire table under an ACCESS EXCLUSIVE lock
summary: The statement `VACUUM FULL customers` runs `VACUUM FULL` on public.customers (200K rows). Unlike standard `VACUUM`, `VACUUM FULL` rewrites every row and index into new disk files while holding an `ACCESS EXCLUSIVE` lock, blocking all concurrent reads and writes.
impact: 200K rows
metric: Rows in snapshot
fix: Run standard `VACUUM (ANALYZE)` (which reclaims dead tuples online without blocking reads or writes), or use `pg_repack` if physical table compaction is required.
sql: VACUUM (ANALYZE) public.customers;
verify: Replace `VACUUM FULL` with `VACUUM (ANALYZE)` or `pg_repack`.
== sql-lint:vacuum-full:public.order_items · High
title: VACUUM FULL on order_items (500 MB) rewrites the entire table under an ACCESS EXCLUSIVE lock
summary: The statement `VACUUM FULL order_items` runs `VACUUM FULL` on public.order_items (500 MB). Unlike standard `VACUUM`, `VACUUM FULL` rewrites every row and index into new disk files while holding an `ACCESS EXCLUSIVE` lock, blocking all concurrent reads and writes.
impact: 500 MB
metric: Table size in snapshot
fix: Run standard `VACUUM (ANALYZE)` (which reclaims dead tuples online without blocking reads or writes), or use `pg_repack` if physical table compaction is required.
sql: VACUUM (ANALYZE) public.order_items;
verify: Replace `VACUUM FULL` with `VACUUM (ANALYZE)` or `pg_repack`."#,
    );
}

#[test]
fn mysql_migrations_say_what_they_block_in_english() {
    let s = snapshot("mysql-demo.json");
    let sql = "CREATE INDEX idx_orders_created ON orders (created_at);\n\
               DROP INDEX ix_customers_region ON customers;\n\
               ALTER TABLE orders ADD COLUMN notes varchar(120);\n\
               ALTER TABLE customers ADD COLUMN token char(36) DEFAULT uuid();\n\
               ALTER TABLE orders MODIFY COLUMN total decimal(14,4);\n\
               ALTER TABLE customers ADD COLUMN code varchar(10) NOT NULL;";
    same(
        &words(&lint_sql(&s, sql)),
        r#"== sql-lint:blocking-index:shop.orders · Medium
title: CREATE INDEX on orders (499K rows, 66.1 MB) without LOCK=NONE can fall back to a table lock
summary: The statement `CREATE INDEX idx_orders_created ON orders (created_at)` builds an index on shop.orders (499K rows, 66.1 MB) without `ALGORITHM=INPLACE LOCK=NONE`. Without explicit `LOCK=NONE`, MySQL/MariaDB can fall back to `LOCK=SHARED` or `ALGORITHM=COPY` if an online build is not supported, blocking concurrent writes until the build finishes.
impact: 499K rows, 66.1 MB
metric: Rows in snapshot
metric: Table size in snapshot
fix: Add `ALGORITHM=INPLACE LOCK=NONE` so the server guarantees an online build and fails immediately instead of locking the table.
sql: CREATE INDEX idx_orders_created ON orders (created_at) ALGORITHM=INPLACE LOCK=NONE;
cost: Online InnoDB index builds buffer concurrent DML in innodb_online_alter_log_max_size while running.
verify: Re-run `culprit lint-sql` on the updated migration to confirm no blocking index builds remain.
== sql-lint:blocking-drop-index:ix_customers_region · Medium
title: DROP INDEX ix_customers_region on customers (199K rows, 21.0 MB) without LOCK=NONE can block concurrent writes
summary: The statement `DROP INDEX ix_customers_region ON customers` drops `ix_customers_region` on shop.customers (199K rows, 21.0 MB) without `ALGORITHM=INPLACE LOCK=NONE`. Specifying `ALGORITHM=INPLACE, LOCK=NONE` guarantees an online drop that never falls back to a blocking table lock.
impact: 199K rows, 21.0 MB
metric: Rows in snapshot
metric: Table size in snapshot
fix: Drop the index with `ALGORITHM=INPLACE, LOCK=NONE` so concurrent reads and writes are not blocked.
sql: ALTER TABLE `shop`.`customers` DROP INDEX `ix_customers_region`, ALGORITHM=INPLACE, LOCK=NONE;
verify: Re-run `culprit lint-sql` after adding `ALGORITHM=INPLACE, LOCK=NONE`.
== sql-lint:mysql-online-ddl:shop.orders · Medium
title: ALTER TABLE on orders (499K rows, 66.1 MB) without LOCK=NONE or ALGORITHM=INSTANT can lock the table
summary: The statement `ALTER TABLE orders ADD COLUMN notes varchar(120)` alters shop.orders (499K rows, 66.1 MB) without specifying `ALGORITHM=INSTANT` or `ALGORITHM=INPLACE, LOCK=NONE`. Without an explicit online DDL clause, MySQL/MariaDB can fall back to `ALGORITHM=COPY` or `LOCK=SHARED`, blocking concurrent writes while rebuilding the table.
impact: 499K rows, 66.1 MB
metric: Rows in snapshot
metric: Table size in snapshot
fix: Append `ALGORITHM=INPLACE, LOCK=NONE` (or `ALGORITHM=INSTANT` for compatible column additions on MySQL 8.0+) so the database rejects any blocking table copy immediately.
sql: ALTER TABLE orders ADD COLUMN notes varchar(120), ALGORITHM=INPLACE, LOCK=NONE;
verify: Re-run `culprit lint-sql` after adding `ALGORITHM=INPLACE, LOCK=NONE` or `ALGORITHM=INSTANT`.
== sql-lint:volatile-default:shop.customers.token · Medium
title: ADD COLUMN customers.token with volatile DEFAULT uuid() rewrites the table (199K rows, 21.0 MB)
summary: The statement `ALTER TABLE customers ADD COLUMN token char(36) DEFAULT uuid()` adds `token` to shop.customers (199K rows, 21.0 MB) with volatile `DEFAULT uuid()`, which cannot use metadata-only fast defaults because every existing row requires a newly evaluated value. The engine rewrites every row and index while holding an exclusive table lock.
impact: 199K rows, 21.0 MB
metric: Rows in snapshot
metric: Table size in snapshot
fix: Add `token` without a default, backfill existing rows in batches, and then set `ALTER TABLE shop.customers ALTER COLUMN token SET DEFAULT uuid();`.
verify: Verify in staging with `SET lock_timeout = '2s'` before running on production.
== sql-lint:table-rewrite:shop.orders · Medium
title: Column type change on orders (499K rows, 66.1 MB) can rewrite the table under an exclusive lock
summary: The statement `ALTER TABLE orders MODIFY COLUMN total decimal(14,4)` alters a column type on shop.orders (499K rows, 66.1 MB). Changing a column's representation rewrites every row and index while holding an `ACCESS EXCLUSIVE` / schema-modification lock, blocking all reads and writes on orders.
impact: 499K rows, 66.1 MB
metric: Rows in snapshot
metric: Table size in snapshot
fix: Prefer an expand-and-contract migration (add a new column, backfill in small batches, swap reads/writes) unless the engine guarantees a metadata-only cast.
verify: Verify in staging with `SET lock_timeout = '2s'` before running on production.
== sql-lint:not-null-without-default:shop.customers.code · High
title: ADD COLUMN customers.code NOT NULL without DEFAULT fails on non-empty table (199K rows, 21.0 MB)
summary: The statement `ALTER TABLE customers ADD COLUMN code varchar(10) NOT NULL` adds `code` as `NOT NULL` without a `DEFAULT` clause to shop.customers (199K rows, 21.0 MB). Because existing rows would receive `NULL` for `code`, the database rejects the statement immediately at runtime.
impact: fails on existing rows
metric: Rows in snapshot
metric: Table size in snapshot
fix: Either supply a non-volatile `DEFAULT` value, or add `code` as nullable first, backfill existing rows, and then enforce `NOT NULL`.
verify: Re-run `culprit lint-sql` after adding a `DEFAULT` or making the initial column nullable."#,
    );
}

#[test]
fn sql_server_migrations_say_what_they_block_in_english() {
    let s = snapshot("mssql-demo.json");
    assert_eq!(s.server.engine, Engine::Mssql);
    let sql = "CREATE INDEX ix_orders_created ON shop.orders (created_at);\n\
               ALTER TABLE shop.orders ALTER COLUMN status varchar(40);";
    same(
        &words(&lint_sql(&s, sql)),
        r#"== sql-lint:blocking-index:shop.orders · High
title: CREATE INDEX on orders (1.0M rows, 163 MB) without ONLINE = ON blocks writes during the build
summary: The statement `CREATE INDEX ix_orders_created ON shop.orders (created_at)` builds an index on shop.orders (1.0M rows, 163 MB) without `WITH (ONLINE = ON)`, which this SQL Server edition supports. Offline index builds hold shared or schema-modification locks that block writers.
impact: 1.0M rows, 163 MB
metric: Rows in snapshot
metric: Table size in snapshot
fix: Add `WITH (ONLINE = ON)` to the `CREATE INDEX` statement so concurrent reads and writes continue.
sql: CREATE INDEX ix_orders_created ON shop.orders (created_at) WITH (ONLINE = ON);
cost: Online index builds use additional tempdb version store space while running.
verify: Re-run `culprit lint-sql` on the updated migration to confirm no blocking index builds remain."#,
    );
}
