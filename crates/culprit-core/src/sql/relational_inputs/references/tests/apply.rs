use super::*;

const SQL: &str = "SELECT d.input_id FROM @Rows AS r CROSS APPLY (SELECT r.id AS input_id) AS d";

#[test]
fn a_closed_direct_column_apply_keeps_only_the_original_physical_parameter_alias() {
    for projection in ["r.id", "r.id AS input_id", "r.id,r.Code AS input_code", "[r].[odd.name] AS [input.name]"] {
        let sql = format!("SELECT COUNT_BIG(*) FROM @Rows AS r CROSS APPLY (SELECT {projection}) AS d");
        assert_eq!(mssql_relational_references(&sql), Some(vec![parameter("@Rows", Some("r"))]), "{sql}");
    }
    let sql = "SELECT [外].[入力名] FROM @Rows AS [内] CROSS APPLY (SELECT [内].[名前] AS [入力名]) AS [外]";
    assert_eq!(mssql_relational_references(sql), Some(vec![parameter("@Rows", Some("内"))]));
}

#[test]
fn projection_aliases_require_an_exact_single_source_and_cannot_shadow_it() {
    for sql in [
        SQL.replace("@Rows AS r", "@Rows r"),
        SQL.replace("@Rows AS r", "@Rows"),
        SQL.replace(") AS d", ") d"),
        SQL.replace(") AS d", ") AS R"),
        SQL.replace("SELECT r.id AS input_id", "SELECT R.id AS input_id"),
        SQL.replace("SELECT r.id AS input_id", "SELECT foreign.id AS input_id"),
        SQL.replace("SELECT r.id AS input_id", "SELECT id AS input_id"),
        SQL.replace("SELECT r.id AS input_id", "SELECT r.id AS same,r.Code AS SAME"),
        SQL.replace("SELECT r.id AS input_id", "SELECT r.id AS first,r.id AS second"),
        SQL.replace("@Rows AS r", "@Rows AS \"r\""),
        SQL.replace(") AS d", ") AS \"d\""),
    ] {
        assert!(mssql_relational_references(&sql).is_none(), "{sql}");
    }
}

#[test]
fn a_single_row_apply_cannot_hide_another_source_expression_or_population_change() {
    for projection in [
        "*",
        "r.*",
        "r.id+1 AS input_id",
        "(r.id) AS input_id",
        "ABS(r.id) AS input_id",
        "dbo.hidden(r.id) AS input_id",
        "unknown_function(r.id) AS input_id",
        "COUNT_BIG(*) AS input_id",
        "TOP(1) r.id AS input_id",
        "DISTINCT r.id AS input_id",
        "1 AS input_id",
        "NULL AS input_id",
        "r.id AS input_id WHERE r.id>0",
        "r.id AS input_id FROM @Other AS other",
        "r.id AS input_id UNION ALL SELECT r.id",
        "(SELECT id FROM @Other) AS input_id",
        "r.id AS input_id COLLATE Latin1_General_100_CI_AS",
        "r.id AS input_id+1",
        "r.id AS input_id,",
    ] {
        let sql = format!("SELECT d.input_id FROM @Rows AS r CROSS APPLY (SELECT {projection}) AS d");
        assert!(mssql_relational_references(&sql).is_none(), "{sql}");
    }
}

#[test]
fn repeated_joined_derived_cte_and_outer_apply_scopes_remain_unresolved() {
    for sql in [
        SQL.replace("CROSS APPLY", "OUTER APPLY"),
        SQL.replace("CROSS APPLY", "APPLY"),
        SQL.replace("(SELECT r.id AS input_id)", "dbo.expand(r.id)"),
        SQL.replace("@Rows AS r", "dbo.Items AS r"),
        SQL.replace("@Rows AS r", "(SELECT * FROM @Rows AS inner_rows) AS r"),
        SQL.replace("@Rows AS r", "@Rows AS r CROSS JOIN @Other AS other"),
        format!("{SQL} CROSS APPLY (SELECT d.input_id AS another_id) AS e"),
        format!("{SQL} JOIN @Other AS other ON d.input_id=other.id"),
        format!("{SQL} CROSS JOIN @Other AS other"),
        format!("{SQL} WHERE EXISTS(SELECT 1 FROM @Other)"),
        format!("{SQL}; SELECT * FROM @Other"),
        "WITH input AS (SELECT * FROM @Rows AS r) SELECT d.id FROM input AS i CROSS APPLY (SELECT i.id) AS d".into(),
    ] {
        assert!(mssql_relational_references(&sql).is_none(), "{sql}");
    }
}

#[test]
fn apply_inventory_does_not_assign_variable_kind_or_retain_values_from_comments_or_expressions() {
    for name in ["@Rows", "@Local", "@unknown"] {
        let sql = SQL.replace("@Rows", name);
        let sql = format!("{sql} WHERE (d.input_id > 9264) /* FROM @Other; EXEC hidden */");
        assert_eq!(mssql_relational_references(&sql), Some(vec![parameter(name, Some("r"))]));
    }
    for expression in ["dbo.hidden(d.input_id)", "unknown_function(d.input_id)", "(SELECT id FROM @Other)"] {
        let sql = SQL.replacen("SELECT d.input_id", &format!("SELECT {expression}"), 1);
        assert!(mssql_relational_references(&sql).is_none(), "{sql}");
    }
}

#[test]
fn apply_preserves_whole_statement_byte_and_expression_bounds_without_returning_a_prefix() {
    let bounded = format!("{SQL} /*{}*/", "x".repeat(MAX_RELATIONAL_SQL_BYTES - SQL.len() - 5));
    assert_eq!(bounded.len(), MAX_RELATIONAL_SQL_BYTES);
    assert!(mssql_relational_references(&bounded).is_some());
    assert!(mssql_relational_references(&(bounded + " ")).is_none());
    let depth = expressions::MAX_EXPRESSION_DEPTH;
    let sql =
        SQL.replacen("SELECT d.input_id", &format!("SELECT {}d.input_id{}", "(".repeat(depth), ")".repeat(depth)), 1);
    assert!(mssql_relational_references(&sql).is_some());
    let sql = SQL.replacen(
        "SELECT d.input_id",
        &format!("SELECT {}d.input_id{}", "(".repeat(depth + 1), ")".repeat(depth + 1)),
        1,
    );
    assert!(mssql_relational_references(&sql).is_none());
}
