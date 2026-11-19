use super::*;

const PASSTHROUGH: &str = "(SELECT * FROM @Rows AS r) AS d";

#[test]
fn a_closed_passthrough_retains_only_its_exact_physical_parameter_alias() {
    for sql in [
        format!("SELECT d.id FROM {PASSTHROUGH}"),
        format!("SELECT COUNT_BIG(*) FROM {PASSTHROUGH} WHERE (d.id > 10000)"),
        "SELECT [外].id FROM (select /* not another FROM */ * from @Rows as [内]) as [外]".into(),
        ";SELECT d.id FROM (SELECT * FROM @Rows AS r) AS d;".into(),
    ] {
        let alias = if sql.contains("[内]") { "内" } else { "r" };
        assert_eq!(mssql_relational_references(&sql), Some(vec![parameter("@Rows", Some(alias))]), "{sql}");
    }
}

#[test]
fn one_passthrough_in_either_join_slot_preserves_the_full_native_inventory() {
    let named = MssqlRelationReference {
        name: MssqlRelationName::Named(vec!["dbo".into(), "Items".into()]),
        alias: Some("i".into()),
    };
    let left = format!("SELECT d.id FROM {PASSTHROUGH} JOIN dbo.Items AS i ON (d.id=i.id)");
    assert_eq!(mssql_relational_references(&left), Some(vec![parameter("@Rows", Some("r")), named.clone()]));
    let right = format!("SELECT d.id FROM dbo.Items AS i JOIN {PASSTHROUGH} ON (d.id=i.id)");
    assert_eq!(mssql_relational_references(&right), Some(vec![named, parameter("@Rows", Some("r"))]));
}

#[test]
fn passthrough_aliases_are_explicit_and_cannot_collide_with_any_physical_input() {
    for sql in [
        "SELECT * FROM (SELECT * FROM @Rows) AS d",
        "SELECT * FROM (SELECT * FROM @Rows r) AS d",
        "SELECT * FROM (SELECT * FROM @Rows AS r) d",
        "SELECT * FROM (SELECT * FROM @Rows AS r)",
        "SELECT * FROM (SELECT * FROM @Rows AS r) AS R",
        "SELECT * FROM (SELECT * FROM @Rows AS r) AS d JOIN dbo.Items AS R ON d.id=R.id",
        "SELECT * FROM (SELECT * FROM @Rows AS r) AS d JOIN dbo.Items AS D ON d.id=D.id",
        "SELECT * FROM (SELECT * FROM @Rows AS r) AS d CROSS JOIN dbo.Items",
        "SELECT * FROM (SELECT * FROM @Rows AS SELECT) AS d",
        "SELECT * FROM (SELECT * FROM @Rows AS r) AS SELECT",
        "SELECT * FROM (SELECT * FROM @Rows AS \"r\") AS d",
        "SELECT * FROM (SELECT * FROM @Rows AS r) AS \"d\"",
    ] {
        assert!(mssql_relational_references(sql).is_none(), "{sql}");
    }
}

#[test]
fn a_passthrough_never_admits_projection_population_or_nested_scope_changes() {
    for inner in [
        "SELECT id FROM @Rows AS r",
        "SELECT r.* FROM @Rows AS r",
        "SELECT *,1 AS extra FROM @Rows AS r",
        "SELECT r.id AS other FROM @Rows AS r",
        "SELECT TOP(1) * FROM @Rows AS r",
        "SELECT DISTINCT * FROM @Rows AS r",
        "SELECT COUNT_BIG(*) FROM @Rows AS r",
        "SELECT * FROM @Rows AS r WHERE r.id > 0",
        "SELECT * FROM @Rows AS r GROUP BY r.id",
        "SELECT * FROM @Rows AS r ORDER BY r.id",
        "SELECT * FROM @Rows AS r JOIN @Other AS o ON r.id=o.id",
        "SELECT * FROM dbo.Items AS r",
        "SELECT * FROM [@Rows] AS r",
        "SELECT * FROM @@Rows AS r",
        "SELECT * FROM dbo.expand(@Rows) AS r",
        "SELECT * FROM (SELECT * FROM @Rows AS r) AS nested",
        "SELECT * FROM @Rows AS r UNION ALL SELECT * FROM @Other AS o",
        "SELECT * FROM @Rows AS r;SELECT * FROM @Other AS o",
    ] {
        let sql = format!("SELECT * FROM ({inner}) AS d");
        assert!(mssql_relational_references(&sql).is_none(), "{sql}");
    }
    for sql in [
        "SELECT * FROM (SELECT * FROM @Rows AS r) AS d(id)",
        "SELECT * FROM (SELECT * FROM @Rows AS r) AS d CROSS JOIN (SELECT * FROM @Other AS o) AS e",
        "WITH d AS (SELECT * FROM @Rows AS r) SELECT * FROM d",
        "SELECT (SELECT * FROM @Rows AS r) AS d FROM @Other AS o",
        "SELECT * FROM @Other AS o WHERE EXISTS (SELECT * FROM @Rows AS r)",
        "SELECT * FROM @Other AS o WHERE o.id IN (SELECT * FROM @Rows AS r)",
        "SELECT * FROM @Other AS o CROSS APPLY (SELECT * FROM @Rows AS r) AS d",
    ] {
        assert!(mssql_relational_references(sql).is_none(), "{sql}");
    }
}

#[test]
fn a_valid_passthrough_cannot_hide_an_unrecognized_outer_expression_or_statement() {
    for expression in ["dbo.hidden(d.id)", "unknown_function(d.id)", "(SELECT COUNT_BIG(*) FROM @Other)"] {
        let sql = format!("SELECT {expression} FROM {PASSTHROUGH}");
        assert!(mssql_relational_references(&sql).is_none(), "{sql}");
    }
    for tail in [
        "WHERE EXISTS (SELECT 1 FROM @Other)",
        "WHERE d.id IN (SELECT id FROM @Other)",
        "UNION ALL SELECT * FROM @Other",
        "CROSS APPLY dbo.expand(d.id) AS f",
        "WHERE (d.id > 0",
        "; SELECT * FROM @Other",
    ] {
        let sql = format!("SELECT * FROM {PASSTHROUGH} {tail}");
        assert!(mssql_relational_references(&sql).is_none(), "{sql}");
    }
}

#[test]
fn passthroughs_preserve_existing_expression_reference_and_sql_byte_limits() {
    let depth = expressions::MAX_EXPRESSION_DEPTH;
    let sql = format!("SELECT {}d.id{} FROM {PASSTHROUGH}", "(".repeat(depth), ")".repeat(depth));
    assert!(mssql_relational_references(&sql).is_some());
    let sql = format!("SELECT {}d.id{} FROM {PASSTHROUGH}", "(".repeat(depth + 1), ")".repeat(depth + 1));
    assert!(mssql_relational_references(&sql).is_none());
    let mut sql = format!("SELECT * FROM {PASSTHROUGH}");
    for at in 1..MAX_RELATIONAL_REFERENCES {
        sql.push_str(&format!(" CROSS JOIN @Rows{at} AS r{at}"));
    }
    assert_eq!(mssql_relational_references(&sql).unwrap().len(), MAX_RELATIONAL_REFERENCES);
    sql.push_str(" CROSS JOIN @Overflow AS overflow");
    assert!(mssql_relational_references(&sql).is_none());
    let sql = format!("SELECT * FROM {PASSTHROUGH}");
    let bounded = format!("{sql} /*{}*/", "x".repeat(MAX_RELATIONAL_SQL_BYTES - sql.len() - 5));
    assert_eq!(bounded.len(), MAX_RELATIONAL_SQL_BYTES);
    assert!(mssql_relational_references(&bounded).is_some());
    assert!(mssql_relational_references(&(bounded + " ")).is_none());
}
