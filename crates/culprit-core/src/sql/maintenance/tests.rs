use super::*;

#[test]
fn each_sql_grammar_classifies_only_its_own_current_maintenance_operations() {
    for engine in Engine::ALL {
        let cases = match engine.sql_syntax() {
            SqlSyntax::Mssql => vec![
                ("ALTER INDEX [ix] ON [dbo].[orders] REBUILD WITH (ONLINE = ON)", MaintenanceKind::IndexRebuild),
                ("ALTER INDEX ALL ON dbo.orders REORGANIZE", MaintenanceKind::IndexReorganize),
                ("UPDATE STATISTICS dbo.orders ix", MaintenanceKind::Statistics),
            ],
            SqlSyntax::Postgres => vec![
                ("CLUSTER orders USING ix", MaintenanceKind::Cluster),
                ("REINDEX (VERBOSE) INDEX CONCURRENTLY public.ix", MaintenanceKind::IndexReindex),
                ("VACUUM (ANALYZE) public.orders", MaintenanceKind::Vacuum),
                ("ANALYZE public.orders", MaintenanceKind::Statistics),
            ],
            SqlSyntax::Mysql => vec![
                ("ALTER TABLE orders ADD INDEX ix(id)", MaintenanceKind::IndexCreate),
                ("CREATE OR REPLACE INDEX ix ON orders(id)", MaintenanceKind::IndexReplace),
                ("OPTIMIZE LOCAL TABLE orders", MaintenanceKind::TableOptimize),
                ("REPAIR NO_WRITE_TO_BINLOG TABLE orders QUICK", MaintenanceKind::TableRepair),
                ("ANALYZE TABLE orders", MaintenanceKind::Statistics),
            ],
            SqlSyntax::Sqlite => vec![
                ("REINDEX main.ix", MaintenanceKind::IndexReindex),
                ("VACUUM", MaintenanceKind::Vacuum),
                ("ANALYZE main.orders", MaintenanceKind::Statistics),
            ],
        };
        for (sql, kind) in cases {
            assert_eq!(maintenance_statement(sql, engine).unwrap(), Some(kind), "{engine:?}: {sql}");
        }
        assert_eq!(
            maintenance_statement("CREATE UNIQUE INDEX ix ON orders (id)", engine).unwrap(),
            Some(MaintenanceKind::IndexCreate)
        );
    }
}

#[test]
fn keywords_in_literals_names_comments_modules_and_unrelated_alterations_are_not_maintenance() {
    for engine in Engine::ALL {
        for sql in [
            "SELECT 'ALTER INDEX ix ON orders REBUILD'",
            "SELECT rebuild FROM orders",
            "/* VACUUM */ SELECT id FROM orders",
        ] {
            assert_eq!(maintenance_statement(sql, engine).unwrap(), None, "{engine:?}: {sql}");
        }
    }
    for sql in [
        "ALTER INDEX ix ON dbo.orders DISABLE",
        "ALTER INDEX ix ON dbo.orders SET (ALLOW_PAGE_LOCKS = OFF)",
        "EXEC dbo.rebuild_indexes",
        "CREATE PROCEDURE dbo.job AS ALTER INDEX ix ON dbo.orders REBUILD",
    ] {
        assert_eq!(maintenance_statement(sql, Engine::Mssql).unwrap(), None, "{sql}");
    }
}

#[test]
fn malformed_or_multiple_statements_cannot_supply_current_operation_classification() {
    for (engine, sql) in [
        (Engine::Mssql, "ALTER INDEX ix ON dbo.orders REBUILD; SELECT 1"),
        (Engine::Mssql, "ALTER INDEX ix ON dbo.orders REBUILD WAITFOR DELAY '00:00:01'"),
        (Engine::Mssql, "ALTER INDEX ix ON dbo.orders REBUILD ("),
        (Engine::Mssql, "CREATE INDEX"),
        (Engine::Postgres, "REINDEX"),
        (Engine::Mysql, "OPTIMIZE TABLE"),
        (Engine::Mysql, "/*! OPTIMIZE TABLE orders */"),
    ] {
        assert!(maintenance_statement(sql, engine).is_err(), "{engine:?}: {sql}");
    }
}

#[test]
fn shared_grammar_does_not_make_native_syntax_options_universal() {
    assert_eq!(
        maintenance_statement("CREATE INDEX CONCURRENTLY ON public.orders (id)", Engine::Postgres).unwrap(),
        Some(MaintenanceKind::IndexCreate)
    );
    assert!(maintenance_statement("CREATE INDEX CONCURRENTLY ix ON orders (id)", Engine::Mssql).is_err());
    assert!(maintenance_statement("CREATE NONCLUSTERED INDEX ix ON orders (id)", Engine::Postgres).is_err());
    assert_eq!(maintenance_statement("OPTIMIZE TABLE orders", Engine::Postgres).unwrap(), None);
    assert_eq!(maintenance_statement("ALTER INDEX ix ON orders REBUILD", Engine::Mysql).unwrap(), None);
}

#[test]
fn index_additions_and_replacement_are_leading_grammar_not_keywords_in_unrelated_alterations() {
    for sql in [
        "ALTER TABLE orders ADD INDEX ix(id)",
        "ALTER TABLE orders ADD KEY (id)",
        "ALTER TABLE orders ADD UNIQUE INDEX ix USING BTREE (id)",
        "ALTER TABLE orders ADD INDEX IF NOT EXISTS ix (id)",
    ] {
        assert_eq!(maintenance_statement(sql, Engine::Mysql).unwrap(), Some(MaintenanceKind::IndexCreate), "{sql}");
    }
    for sql in [
        "ALTER TABLE orders ADD COLUMN rebuild INT",
        "ALTER TABLE orders RENAME COLUMN optimize TO rebuild",
        "SELECT 'CREATE OR REPLACE INDEX ix ON orders(id)'",
    ] {
        assert_eq!(maintenance_statement(sql, Engine::Mysql).unwrap(), None, "{sql}");
    }
    assert_eq!(
        maintenance_statement("CREATE OR REPLACE UNIQUE INDEX ix USING BTREE ON orders (id)", Engine::Mysql).unwrap(),
        Some(MaintenanceKind::IndexReplace)
    );
    assert!(maintenance_statement("ALTER TABLE orders ADD INDEX ix", Engine::Mysql).is_err());
    assert!(maintenance_statement("ALTER TABLE orders ADD INDEX ix id", Engine::Mysql).is_err());
    for sql in ["CLUSTER", "CLUSTER (VERBOSE) orders USING ix", "CLUSTER VERBOSE orders"] {
        assert_eq!(maintenance_statement(sql, Engine::Postgres).unwrap(), Some(MaintenanceKind::Cluster), "{sql}");
    }
    assert_eq!(maintenance_statement("CLUSTER orders USING ix", Engine::Mssql).unwrap(), None);
    assert_eq!(maintenance_statement("ALTER TABLE orders ADD INDEX ix(id)", Engine::Postgres).unwrap(), None);
}
