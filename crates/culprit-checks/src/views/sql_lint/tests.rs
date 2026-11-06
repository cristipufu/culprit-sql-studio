use culprit_core::{Engine, Snapshot};

use super::alter_table::normalized_base_type;
use super::{apply_sql_fixes, lint_sql};

fn postgres_snapshot() -> Snapshot {
    serde_json::from_str(include_str!("../../../../../fixtures/postgres-demo.json")).unwrap()
}

#[test]
fn flags_blocking_create_index_unless_concurrent() {
    let mut s = postgres_snapshot();
    s.tables[0].rows = Some(500_000.0);
    let table = s.tables[0].name.clone();
    let sql = format!("CREATE INDEX idx_test ON {table} (created_at);");
    let out = lint_sql(&s, &sql);
    assert_eq!(out.len(), 1);
    assert!(out[0].id.starts_with("sql-lint:blocking-index:"));
    assert!(out[0].fix.as_ref().unwrap().sql.as_deref().unwrap().contains("CREATE INDEX CONCURRENTLY"));

    let ok_sql = format!("CREATE INDEX CONCURRENTLY idx_test ON {table} (created_at);");
    assert!(lint_sql(&s, &ok_sql).is_empty());
}

#[test]
fn timescale_hypertables_do_not_require_concurrent_index_ddl() {
    let mut s = postgres_snapshot();
    s.tables[0].rows = Some(500_000.0);
    s.tables[0].kind = Some("hypertable".into());
    let table = s.tables[0].name.clone();
    let schema = s.tables[0].schema.clone();
    let idx_name = s.indexes.iter().find(|i| i.schema == schema && i.table == table).map(|i| i.name.clone()).unwrap();

    let sql = format!(
        "CREATE INDEX idx_hypertable_time ON {table} (created_at);\n\
         DROP INDEX {idx_name};\n\
         REINDEX TABLE {table};"
    );
    assert!(lint_sql(&s, &sql).is_empty());
}

#[test]
fn flags_unindexed_and_mismatched_foreign_key_additions() {
    let mut s = postgres_snapshot();
    let child = s.tables[0].name.clone();
    let parent = s.tables[1].name.clone();
    let schema = s.tables[0].schema.clone();
    s.columns.push(culprit_core::ColumnInfo {
        schema: schema.clone(),
        table: child.clone(),
        name: "parent_ref_id".into(),
        data_type: "integer".into(),
        nullable: true,
        default: None,
        generated: false,
        ordinal: 99,
    });
    s.columns.retain(|c| !(c.table == parent && c.name == "id"));
    s.columns.push(culprit_core::ColumnInfo {
        schema: schema.clone(),
        table: parent.clone(),
        name: "id".into(),
        data_type: "bigint".into(),
        nullable: false,
        default: None,
        generated: false,
        ordinal: 1,
    });
    let sql =
        format!("ALTER TABLE {child} ADD CONSTRAINT fk_parent FOREIGN KEY (parent_ref_id) REFERENCES {parent} (id);");
    let out = lint_sql(&s, &sql);
    assert!(out.iter().any(|f| f.id.starts_with("sql-lint:unindexed-fk:")));
    assert!(out.iter().any(|f| f.id.starts_with("sql-lint:fk-type-mismatch:")));

    // Including a CREATE INDEX in the same migration clears the unindexed-fk warning.
    let with_index = format!("CREATE INDEX CONCURRENTLY idx_child_parent ON {child} (parent_ref_id);\n{sql}");
    let out2 = lint_sql(&s, &with_index);
    assert!(!out2.iter().any(|f| f.id.starts_with("sql-lint:unindexed-fk:")));
    assert!(out2.iter().any(|f| f.id.starts_with("sql-lint:fk-type-mismatch:")));
}

#[test]
fn flags_table_rewrite_and_set_not_null_on_large_tables() {
    let mut s = postgres_snapshot();
    s.tables[0].rows = Some(1_200_000.0);
    let table = s.tables[0].name.clone();
    let sql = format!(
        "ALTER TABLE {table} ALTER COLUMN payload TYPE text;\nALTER TABLE {table} ALTER COLUMN payload SET NOT NULL;"
    );
    let out = lint_sql(&s, &sql);
    assert!(out.iter().any(|f| f.id.starts_with("sql-lint:table-rewrite:")));
    assert!(out.iter().any(|f| f.id.starts_with("sql-lint:set-not-null:")));
}

#[test]
fn flags_concurrent_index_in_transaction_and_multi_action_alter_table() {
    let mut s = postgres_snapshot();
    s.tables[0].rows = Some(500_000.0);
    let table = s.tables[0].name.clone();
    let sql = format!(
        "BEGIN;\nCREATE INDEX CONCURRENTLY idx_tx ON {table} (created_at);\nCOMMIT;\n\
         ALTER TABLE {table} ADD COLUMN token uuid DEFAULT gen_random_uuid(), \
         ADD COLUMN created_ts timestamptz DEFAULT now(), \
         ADD CONSTRAINT chk_positive CHECK (amount > 0), \
         ALTER COLUMN status SET NOT NULL;"
    );
    let out = lint_sql(&s, &sql);
    assert!(out.iter().any(|f| f.id == "sql-lint:concurrent-in-tx:idx_tx"), "{out:#?}");
    assert!(out.iter().any(|f| f.id.ends_with(".token") && f.id.starts_with("sql-lint:volatile-default:")));
    assert!(!out.iter().any(|f| f.id.ends_with(".created_ts")));
    assert!(
        out.iter().any(|f| f.id.starts_with("sql-lint:unvalidated-constraint:") && f.id.ends_with(":chk_positive"))
    );
    assert!(out.iter().any(|f| f.id.starts_with("sql-lint:set-not-null:")));

    // Adding NOT VALID to the CHECK constraint suppresses the unvalidated-constraint warning.
    let valid_sql = format!("ALTER TABLE {table} ADD CONSTRAINT chk_positive CHECK (amount > 0) NOT VALID;");
    assert!(lint_sql(&s, &valid_sql).is_empty());
}

#[test]
fn flags_blocking_drop_index_truncate_and_workload_breaking_ddl() {
    let mut s = postgres_snapshot();
    s.tables[0].rows = Some(500_000.0);
    let table = s.tables[0].name.clone();
    let schema = s.tables[0].schema.clone();
    let idx_name = s.indexes.iter().find(|i| i.schema == schema && i.table == table).map(|i| i.name.clone()).unwrap();
    if let Some(w) = s.workload.as_mut()
        && let Some(q) = w.queries.first_mut()
    {
        q.text = format!("SELECT id, legacy_col FROM {schema}.{table} WHERE id = $1");
        q.plan = None;
        q.last_seen = None;
    }
    let sql = format!(
        "DROP INDEX {idx_name};\n\
         TRUNCATE TABLE {table};\n\
         ALTER TABLE {table} DROP COLUMN legacy_col;"
    );
    let out = lint_sql(&s, &sql);
    assert!(out.iter().any(|f| f.id == format!("sql-lint:blocking-drop-index:{idx_name}")), "{out:#?}");
    assert!(out.iter().any(|f| f.id == format!("sql-lint:truncate:{schema}.{table}")));
    assert!(out.iter().any(|f| f.id == format!("sql-lint:workload-break:{schema}.{table}.legacy_col")));
}

#[test]
fn flags_not_null_without_default_blocking_unique_and_recognizes_validated_not_null() {
    let mut s = postgres_snapshot();
    s.tables[0].rows = Some(500_000.0);
    let table = s.tables[0].name.clone();
    let schema = s.tables[0].schema.clone();
    let sql = format!(
        "ALTER TABLE {table} ADD COLUMN required_code text NOT NULL;\n\
         ALTER TABLE {table} ADD CONSTRAINT uq_code UNIQUE (required_code);"
    );
    let out = lint_sql(&s, &sql);
    assert!(out.iter().any(|f| f.id == format!("sql-lint:not-null-without-default:{schema}.{table}.required_code")));
    assert!(out.iter().any(|f| f.id == format!("sql-lint:blocking-unique-constraint:{schema}.{table}:uq_code")));

    let safe_sql = format!(
        "ALTER TABLE {table} ADD CONSTRAINT chk_status_nn CHECK (status IS NOT NULL) NOT VALID;\n\
         ALTER TABLE {table} VALIDATE CONSTRAINT chk_status_nn;\n\
         ALTER TABLE {table} ALTER COLUMN status SET NOT NULL;\n\
         CREATE UNIQUE INDEX CONCURRENTLY idx_uq_code ON {table} (required_code);\n\
         ALTER TABLE {table} ADD CONSTRAINT uq_code UNIQUE USING INDEX idx_uq_code;"
    );
    assert!(lint_sql(&s, &safe_sql).is_empty());
}

#[test]
fn skips_metadata_only_type_widenings_and_flags_rewriting_type_changes() {
    let mut s = postgres_snapshot();
    s.tables[0].rows = Some(500_000.0);
    let table = s.tables[0].name.clone();
    let schema = s.tables[0].schema.clone();
    s.columns.push(culprit_core::ColumnInfo {
        schema: schema.clone(),
        table: table.clone(),
        name: "short_code".into(),
        data_type: "character varying(50)".into(),
        nullable: true,
        default: None,
        generated: false,
        ordinal: 90,
    });
    s.columns.push(culprit_core::ColumnInfo {
        schema: schema.clone(),
        table: table.clone(),
        name: "amount".into(),
        data_type: "numeric(10,2)".into(),
        nullable: true,
        default: None,
        generated: false,
        ordinal: 91,
    });

    // Metadata-only widenings in PostgreSQL do not rewrite the table.
    let widen_sql = format!(
        "ALTER TABLE {table} ALTER COLUMN short_code TYPE varchar(120);\n\
         ALTER TABLE {table} ALTER COLUMN short_code TYPE text;\n\
         ALTER TABLE {table} ALTER COLUMN amount TYPE numeric(14, 2);\n\
         ALTER TABLE {table} ALTER COLUMN amount TYPE numeric;"
    );
    assert!(lint_sql(&s, &widen_sql).is_empty());

    // Narrowing length, changing scale, or supplying a USING clause rewrites the table.
    let narrow_sql = format!("ALTER TABLE {table} ALTER COLUMN short_code TYPE varchar(20);");
    assert!(lint_sql(&s, &narrow_sql).iter().any(|f| f.id.starts_with("sql-lint:table-rewrite:")));

    let scale_sql = format!("ALTER TABLE {table} ALTER COLUMN amount TYPE numeric(14, 4);");
    assert!(lint_sql(&s, &scale_sql).iter().any(|f| f.id.starts_with("sql-lint:table-rewrite:")));

    let using_sql = format!("ALTER TABLE {table} ALTER COLUMN short_code TYPE text USING trim(short_code);");
    assert!(lint_sql(&s, &using_sql).iter().any(|f| f.id.starts_with("sql-lint:table-rewrite:")));
}

#[test]
fn flags_vacuum_full_cluster_blocking_reindex_and_suppresses_concurrent_quick_fix_in_tx() {
    let mut s = postgres_snapshot();
    s.tables[0].rows = Some(500_000.0);
    let table = s.tables[0].name.clone();
    let schema = s.tables[0].schema.clone();
    let idx_name = s.indexes.iter().find(|i| i.schema == schema && i.table == table).map(|i| i.name.clone()).unwrap();

    let maint_sql = format!(
        "VACUUM FULL {table};\n\
         CLUSTER {table} USING {idx_name};\n\
         REINDEX TABLE {table};\n\
         REINDEX INDEX {idx_name};"
    );
    let out = lint_sql(&s, &maint_sql);
    assert!(out.iter().any(|f| f.id == format!("sql-lint:vacuum-full:{schema}.{table}")), "{out:#?}");
    assert!(out.iter().any(|f| f.id == format!("sql-lint:cluster:{schema}.{table}")));
    assert!(out.iter().any(|f| f.id == format!("sql-lint:blocking-reindex:{schema}.{table}")));
    assert!(out.iter().any(|f| f.id == format!("sql-lint:blocking-reindex:{idx_name}")));

    // Inside BEGIN ... COMMIT, CREATE INDEX omits the CONCURRENTLY quick-fix SQL.
    let tx_sql = format!("BEGIN;\ncreate  unique\n  index idx_new on {table} (id);\nCOMMIT;");
    let tx_out = lint_sql(&s, &tx_sql);
    assert_eq!(tx_out.len(), 1);
    assert!(tx_out[0].fix.as_ref().unwrap().sql.is_none());

    // Outside a transaction, mixed-case / multiline CREATE UNIQUE INDEX gets a clean CONCURRENTLY quick fix.
    let non_tx_sql = format!("create  unique\n  index idx_new on {table} (id);");
    let non_tx_out = lint_sql(&s, &non_tx_sql);
    assert_eq!(
        non_tx_out[0].fix.as_ref().unwrap().sql.as_deref(),
        Some(format!("CREATE UNIQUE INDEX CONCURRENTLY idx_new on {table} (id);").as_str())
    );

    // MySQL online DDL: CREATE INDEX, DROP INDEX, and ALTER TABLE without LOCK=NONE or ALGORITHM=INSTANT are flagged.
    let mut mysql_s = s.clone();
    mysql_s.server.engine = Engine::Mysql;
    let mysql_sql = format!(
        "CREATE INDEX idx_mysql_test ON {table} (created_at);\n\
         DROP INDEX {idx_name} ON {table};\n\
         ALTER TABLE {table} ADD COLUMN notes varchar(120);"
    );
    let mysql_out = lint_sql(&mysql_s, &mysql_sql);
    assert!(mysql_out.iter().any(|f| f.id == format!("sql-lint:blocking-index:{schema}.{table}")), "{mysql_out:#?}");
    assert!(mysql_out.iter().any(|f| f.id == format!("sql-lint:blocking-drop-index:{idx_name}")));
    assert!(mysql_out.iter().any(|f| f.id == format!("sql-lint:mysql-online-ddl:{schema}.{table}")));

    let mysql_safe = format!(
        "CREATE INDEX idx_mysql_test ON {table} (created_at) ALGORITHM=INPLACE LOCK=NONE;\n\
         DROP INDEX {idx_name} ON {table} ALGORITHM=INPLACE LOCK=NONE;\n\
         ALTER TABLE {table} ADD COLUMN notes varchar(120), ALGORITHM=INSTANT;"
    );
    assert!(lint_sql(&mysql_s, &mysql_safe).is_empty());

    // Auto-fixing rewrites fixable statements in place and resolves their lint findings.
    let raw_migration = format!(
        "-- migration header\n\
         CREATE INDEX /* keep comment */ idx_auto_fix ON {table} (created_at);\n\
         DROP INDEX {idx_name};\n"
    );
    let (rewritten, count) = apply_sql_fixes(&s, &raw_migration);
    assert_eq!(count, 2);
    assert!(rewritten.contains("-- migration header"));
    assert!(rewritten.contains("CREATE INDEX CONCURRENTLY /* keep comment */ idx_auto_fix"));
    assert!(rewritten.contains(&format!("DROP INDEX CONCURRENTLY IF EXISTS {idx_name};")));
    assert!(lint_sql(&s, &rewritten).is_empty(), "{:?}", lint_sql(&s, &rewritten));

    // Dollar-quoted PL/pgSQL function bodies containing BEGIN ...; END; do not corrupt transaction state.
    let plpgsql_sql = format!(
        "CREATE OR REPLACE FUNCTION touch_row() RETURNS trigger AS $$\n\
         BEGIN\n\
           NEW.updated_at = now();\n\
           RETURN NEW;\n\
         END;\n\
         $$ LANGUAGE plpgsql;\n\
         CREATE INDEX CONCURRENTLY idx_after_fn ON {table} (created_at);\n"
    );
    assert!(lint_sql(&s, &plpgsql_sql).is_empty());

    // Validating a foreign key constraint does not suppress SET NOT NULL on the same table.
    let fk_val_sql = format!(
        "ALTER TABLE {table} ADD CONSTRAINT fk_ref FOREIGN KEY (id) REFERENCES {table} (id) NOT VALID;\n\
         ALTER TABLE {table} VALIDATE CONSTRAINT fk_ref;\n\
         ALTER TABLE {table} ALTER COLUMN status SET NOT NULL;"
    );
    assert!(lint_sql(&s, &fk_val_sql).iter().any(|f| f.id.starts_with("sql-lint:set-not-null:")));

    // Multiple separate ALTER TABLE statements on the same table in one script are each linted independently.
    let multi_alter_sql = format!(
        "ALTER TABLE {table} ALTER COLUMN col_a SET NOT NULL;\n\
         ALTER TABLE {table} ALTER COLUMN col_b SET NOT NULL;"
    );
    let multi_alter_out = lint_sql(&s, &multi_alter_sql);
    assert_eq!(multi_alter_out.iter().filter(|f| f.id == format!("sql-lint:set-not-null:{schema}.{table}")).count(), 2);

    // MySQL unsigned vs signed integer types on FK columns are caught as a type mismatch.
    assert_eq!(normalized_base_type("bigint(20) unsigned"), "bigint unsigned");
    assert_eq!(normalized_base_type("bigint(20)"), "bigint");

    // Nested block comments, carriage-return line comments, and escaped brackets with semicolons are lexed cleanly.
    let nested_and_cr = format!(
        "-- header comment\rCREATE /* outer /* INDEX */ inner */ INDEX [idx]]semi;colon] ON {table} (created_at);"
    );
    let (rewritten_nested, nested_count) = apply_sql_fixes(&s, &nested_and_cr);
    assert_eq!(nested_count, 1);
    assert!(rewritten_nested.contains("CREATE /* outer /* INDEX */ inner */ INDEX CONCURRENTLY [idx]]semi;colon]"));
    assert!(lint_sql(&s, &rewritten_nested).is_empty());
}
