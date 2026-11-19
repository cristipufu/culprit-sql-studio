use super::*;

fn statements(projection: &str) -> [String; 2] {
    bodies(&format!("SELECT {projection} FROM @Rows AS r"))
}

fn bodies(body: &str) -> [String; 2] {
    [format!("SELECT d.id FROM ({body}) AS d"), format!("WITH input AS ({body}) SELECT d.id FROM input AS d")]
}

#[test]
fn direct_projection_lists_preserve_the_original_parameter_alias_in_both_passthrough_forms() {
    for projection in ["r.id", "r.Id,r.Code", "[r].[Code],[r].[Id]", "r.[odd.name],r.[escaped]]name]"] {
        for sql in statements(projection) {
            assert_eq!(mssql_relational_references(&sql), Some(vec![parameter("@Rows", Some("r"))]), "{sql}");
        }
    }
    for sql in [
        "SELECT [外].[名前] FROM (SELECT [内].[名前],[内].[符号] FROM @Rows AS [内]) AS [外]",
        "WITH [入力] AS (SELECT [内].[名前],[内].[符号] FROM @Rows AS [内]) SELECT [外].[名前] FROM [入力] AS [外]",
    ] {
        assert_eq!(mssql_relational_references(sql), Some(vec![parameter("@Rows", Some("内"))]), "{sql}");
    }
}

#[test]
fn unqualified_foreign_duplicate_and_renamed_columns_are_not_direct_projections() {
    for projection in [
        "",
        "id",
        "id,Code",
        "foreign.id",
        "R.id",
        "r.id,foreign.Code",
        "r.id,r.id",
        "r.Id,r.id",
        "r.id,[r].[id]",
        "r.id AS other",
        "r.id AS id",
        "r.id other",
        "other=r.id",
        "r.id,",
        ",r.id",
        "r.id,,r.Code",
        "r.id,r.Code AS other",
        "r.*",
        "*,r.id",
        "r.id,*",
        "dbo.r.id",
        "@Rows.id",
        "r.\"id\"",
        "\"r\".id",
    ] {
        for sql in statements(projection) {
            assert!(mssql_relational_references(&sql).is_none(), "{sql}");
        }
    }
}

#[test]
fn expressions_values_aggregates_and_population_changes_cannot_hide_in_a_column_list() {
    for projection in [
        "r.id+1",
        "(r.id)",
        "ABS(r.id)",
        "dbo.hidden(r.id)",
        "CAST(r.id AS bigint)",
        "CONVERT(bigint,r.id)",
        "r.Code COLLATE Latin1_General_100_CI_AS",
        "ISNULL(r.id,0)",
        "COUNT_BIG(*)",
        "DISTINCT r.id",
        "TOP(1) r.id",
        "1",
        "NULL",
        "'r.id'",
        "r.id,N'private'",
        "(SELECT id FROM @Other)",
        "r.id INTO #copy",
    ] {
        for sql in statements(projection) {
            assert!(mssql_relational_references(&sql).is_none(), "{sql}");
        }
    }
    for tail in [
        "WHERE r.id>0",
        "GROUP BY r.id",
        "ORDER BY r.id",
        "JOIN @Other AS o ON r.id=o.id",
        "UNION ALL SELECT id FROM @Other",
    ] {
        for sql in bodies(&format!("SELECT r.id FROM @Rows AS r {tail}")) {
            assert!(mssql_relational_references(&sql).is_none(), "{sql}");
        }
    }
}

#[test]
fn direct_columns_do_not_relax_alias_inventory_or_nested_scope_guards() {
    for sql in [
        "SELECT d.id FROM (SELECT r.id FROM @Rows r) AS d",
        "SELECT d.id FROM (SELECT r.id FROM @Rows AS r) AS R",
        "SELECT d.id FROM (SELECT r.id FROM @Rows AS r) AS d JOIN dbo.Items AS r ON d.id=r.id",
        "WITH input AS (SELECT r.id FROM @Rows AS r) SELECT d.id FROM input AS d JOIN input AS e ON d.id=e.id",
        "WITH input AS (SELECT r.id FROM @Rows AS r) SELECT d.id FROM input AS r",
        "WITH input(id) AS (SELECT r.id FROM @Rows AS r) SELECT d.id FROM input AS d",
        "SELECT d.id FROM (SELECT r.id FROM (SELECT * FROM @Rows AS inner_rows) AS r) AS d",
        "SELECT dbo.hidden(d.id) FROM (SELECT r.id FROM @Rows AS r) AS d",
        "WITH input AS (SELECT r.id FROM @Rows AS r) SELECT d.id FROM input AS d WHERE EXISTS(SELECT 1 FROM @Other)",
    ] {
        assert!(mssql_relational_references(sql).is_none(), "{sql}");
    }
}

#[test]
fn projected_passthrough_joins_keep_all_original_relations_without_assigning_variable_kind() {
    let sql =
        "WITH input AS (SELECT l.id,l.Code FROM @Local AS l) SELECT d.id FROM input AS d JOIN @Rows AS r ON d.id=r.id";
    assert_eq!(
        mssql_relational_references(sql),
        Some(vec![parameter("@Local", Some("l")), parameter("@Rows", Some("r"))])
    );
    let sql = "SELECT d.id FROM dbo.Items AS i JOIN (SELECT r.id,r.Code FROM @Rows AS r) AS d ON d.id=i.id";
    assert_eq!(
        mssql_relational_references(sql),
        Some(vec![
            MssqlRelationReference {
                name: MssqlRelationName::Named(vec!["dbo".into(), "Items".into()]),
                alias: Some("i".into())
            },
            parameter("@Rows", Some("r"))
        ])
    );
}

#[test]
fn direct_projection_lists_preserve_existing_statement_and_reference_bounds() {
    let sql = statements("r.id,r.Code")[0].clone();
    let bounded = format!("{sql} /*{}*/", "x".repeat(MAX_RELATIONAL_SQL_BYTES - sql.len() - 5));
    assert!(mssql_relational_references(&bounded).is_some());
    assert!(mssql_relational_references(&(bounded + " ")).is_none());
    let mut sql = statements("r.id,r.Code")[1].clone();
    for at in 1..MAX_RELATIONAL_REFERENCES {
        sql.push_str(&format!(" CROSS JOIN @Rows{at} AS r{at}"));
    }
    assert_eq!(mssql_relational_references(&sql).unwrap().len(), MAX_RELATIONAL_REFERENCES);
    sql.push_str(" CROSS JOIN @Overflow AS overflow");
    assert!(mssql_relational_references(&sql).is_none());
}
