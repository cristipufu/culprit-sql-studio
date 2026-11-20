use super::*;

const CTE: &str = "WITH input AS (SELECT * FROM @Rows AS r)";

#[test]
fn a_single_closed_cte_retains_its_original_parameter_and_inner_alias() {
    for sql in [
        format!("{CTE} SELECT d.id FROM input AS d"),
        format!(";{CTE} SELECT COUNT_BIG(*) FROM input AS d WHERE (d.id > 10000);"),
        "WITH input AS (SELECT r.id AS other FROM @Rows AS r) SELECT * FROM input AS d".into(),
        "WITH [入力] AS (select /* only this FROM */ * from @Rows as [内]) SELECT [外].id FROM [入力] AS [外]".into(),
    ] {
        let alias = if sql.contains("[内]") { "内" } else { "r" };
        assert_eq!(mssql_relational_references(&sql), Some(vec![parameter("@Rows", Some(alias))]), "{sql}");
    }
}

#[test]
fn one_cte_in_either_join_slot_preserves_every_original_relation() {
    let named = MssqlRelationReference {
        name: MssqlRelationName::Named(vec!["dbo".into(), "Items".into()]),
        alias: Some("i".into()),
    };
    let left = format!("{CTE} SELECT d.id FROM input AS d JOIN dbo.Items AS i ON d.id=i.id");
    assert_eq!(mssql_relational_references(&left), Some(vec![parameter("@Rows", Some("r")), named.clone()]));
    let right = format!("{CTE} SELECT d.id FROM dbo.Items AS i JOIN input AS d ON d.id=i.id");
    assert_eq!(mssql_relational_references(&right), Some(vec![named, parameter("@Rows", Some("r"))]));
}

#[test]
fn cte_names_and_inner_outer_and_other_aliases_are_explicit_and_noncolliding() {
    for sql in [
        "WITH input AS (SELECT * FROM @Rows) SELECT * FROM input AS d",
        "WITH input AS (SELECT * FROM @Rows r) SELECT * FROM input AS d",
        "WITH input AS (SELECT * FROM @Rows AS input) SELECT * FROM input AS d",
        "WITH input AS (SELECT * FROM @Rows AS r) SELECT * FROM input",
        "WITH input AS (SELECT * FROM @Rows AS r) SELECT * FROM input d",
        "WITH input AS (SELECT * FROM @Rows AS r) SELECT * FROM input AS INPUT",
        "WITH input AS (SELECT * FROM @Rows AS r) SELECT * FROM input AS R",
        "WITH input AS (SELECT * FROM @Rows AS r) SELECT * FROM INPUT AS d",
        "WITH input AS (SELECT * FROM @Rows AS r) SELECT * FROM input AS d JOIN dbo.Items AS R ON d.id=R.id",
        "WITH input AS (SELECT * FROM @Rows AS r) SELECT * FROM input AS d JOIN dbo.Items AS D ON d.id=D.id",
        "WITH input AS (SELECT * FROM @Rows AS r) SELECT * FROM input AS d JOIN dbo.Items AS INPUT ON d.id=INPUT.id",
        "WITH input AS (SELECT * FROM @Rows AS r) SELECT * FROM input AS d CROSS JOIN dbo.Items",
        "WITH \"input\" AS (SELECT * FROM @Rows AS r) SELECT * FROM \"input\" AS d",
        "WITH input AS (SELECT * FROM @Rows AS \"r\") SELECT * FROM input AS d",
        "WITH input AS (SELECT * FROM @Rows AS r) SELECT * FROM input AS \"d\"",
    ] {
        assert!(mssql_relational_references(sql).is_none(), "{sql}");
    }
}

#[test]
fn projection_population_native_functions_and_nested_scopes_are_not_passthrough_definitions() {
    for body in [
        "SELECT id FROM @Rows AS r",
        "SELECT r.* FROM @Rows AS r",
        "SELECT *,1 AS extra FROM @Rows AS r",
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
        "SELECT * FROM input AS r",
        "SELECT * FROM @Rows AS r UNION ALL SELECT * FROM input AS r",
        "SELECT * FROM @Rows AS r;SELECT * FROM @Other AS o",
    ] {
        let sql = format!("WITH input AS ({body}) SELECT * FROM input AS d");
        assert!(mssql_relational_references(&sql).is_none(), "{sql}");
    }
}

#[test]
fn a_cte_must_be_one_definition_used_once_without_renaming_or_another_passthrough_scope() {
    for sql in [
        "WITH input(id) AS (SELECT * FROM @Rows AS r) SELECT * FROM input AS d",
        "WITH input AS (SELECT * FROM @Rows AS r), other AS (SELECT * FROM @Other AS o) SELECT * FROM input AS d",
        "WITH input AS (WITH nested AS (SELECT * FROM @Rows AS r) SELECT * FROM nested AS n) SELECT * FROM input AS d",
        "WITH input AS (SELECT * FROM @Rows AS r) SELECT * FROM @Other AS o",
        "WITH input AS (SELECT * FROM @Rows AS r) SELECT * FROM dbo.input AS d",
        "WITH input AS (SELECT * FROM @Rows AS r) SELECT * FROM input AS d JOIN input AS e ON d.id=e.id",
        "WITH input AS (SELECT * FROM @Rows AS r) SELECT * FROM input AS d JOIN (SELECT * FROM @Other AS o) AS e ON d.id=e.id",
        "WITH input AS (SELECT * FROM @Rows AS r) SELECT * FROM input AS d(id)",
        "WITH input AS (SELECT * FROM @Rows AS r) SELECT * FROM input() AS d",
        "WITH input AS (SELECT * FROM @Rows AS r SELECT * FROM input AS d",
        "WITH RECURSIVE input AS (SELECT * FROM @Rows AS r) SELECT * FROM input AS d",
    ] {
        assert!(mssql_relational_references(sql).is_none(), "{sql}");
    }
}

#[test]
fn a_closed_cte_cannot_hide_unknown_outer_calls_subqueries_or_another_statement() {
    for expression in ["dbo.hidden(d.id)", "unknown_function(d.id)", "(SELECT COUNT_BIG(*) FROM @Other)"] {
        let sql = format!("{CTE} SELECT {expression} FROM input AS d");
        assert!(mssql_relational_references(&sql).is_none(), "{sql}");
    }
    for tail in [
        "WHERE EXISTS (SELECT 1 FROM @Other)",
        "WHERE d.id IN (SELECT id FROM @Other)",
        "UNION ALL SELECT * FROM @Other",
        "CROSS APPLY dbo.expand(d.id) AS f",
        "WHERE (d.id > 0",
        ";SELECT * FROM @Other",
    ] {
        let sql = format!("{CTE} SELECT * FROM input AS d {tail}");
        assert!(mssql_relational_references(&sql).is_none(), "{sql}");
    }
}

#[test]
fn cte_binding_does_not_assign_a_kind_to_the_original_parameter_token() {
    let sql = "WITH input AS (SELECT * FROM @Local AS l) SELECT d.id FROM input AS d JOIN @Rows AS r ON d.id=r.id";
    assert_eq!(
        mssql_relational_references(sql),
        Some(vec![parameter("@Local", Some("l")), parameter("@Rows", Some("r"))])
    );
}

#[test]
fn ctes_reuse_existing_expression_reference_and_sql_byte_bounds() {
    let depth = expressions::MAX_EXPRESSION_DEPTH;
    let sql = format!("{CTE} SELECT {}d.id{} FROM input AS d", "(".repeat(depth), ")".repeat(depth));
    assert!(mssql_relational_references(&sql).is_some());
    let excessive = format!("{CTE} SELECT {}d.id{} FROM input AS d", "(".repeat(depth + 1), ")".repeat(depth + 1));
    assert!(mssql_relational_references(&excessive).is_none());
    let mut sql = format!("{CTE} SELECT * FROM input AS d");
    for at in 1..MAX_RELATIONAL_REFERENCES {
        sql.push_str(&format!(" CROSS JOIN @Rows{at} AS r{at}"));
    }
    assert_eq!(mssql_relational_references(&sql).unwrap().len(), MAX_RELATIONAL_REFERENCES);
    sql.push_str(" CROSS JOIN @Overflow AS overflow");
    assert!(mssql_relational_references(&sql).is_none());
    let sql = format!("{CTE} SELECT * FROM input AS d");
    let bounded = format!("{sql} /*{}*/", "x".repeat(MAX_RELATIONAL_SQL_BYTES - sql.len() - 5));
    assert_eq!(bounded.len(), MAX_RELATIONAL_SQL_BYTES);
    assert!(mssql_relational_references(&bounded).is_some());
    assert!(mssql_relational_references(&(bounded + " ")).is_none());
}

const CHAIN: &str = "WITH seed AS (SELECT r.id FROM @Rows AS r), input AS (SELECT p.id FROM seed AS p)";

#[test]
fn two_single_use_ctes_keep_the_exact_original_parameter_and_native_inner_alias() {
    for sql in [
        format!("{CHAIN} SELECT d.id FROM input AS d"),
        "WITH seed AS (SELECT * FROM @Rows AS r), input AS (SELECT * FROM seed AS p) SELECT COUNT_BIG(*) FROM input AS d WHERE (d.id > 10000)".into(),
        "WITH seed AS (SELECT r.id AS source_id FROM @Rows AS r), input AS (SELECT p.source_id AS input_id FROM seed AS p) SELECT d.input_id FROM input AS d".into(),
        ";WITH [元] AS (SELECT [内].[名前] FROM @Rows AS [内]), [入力] AS (SELECT [中].[名前] FROM [元] AS [中]) SELECT [外].[名前] FROM [入力] AS [外];".into(),
    ] {
        let alias = if sql.contains("[内]") { "内" } else { "r" };
        assert_eq!(mssql_relational_references(&sql), Some(vec![parameter("@Rows", Some(alias))]), "{sql}");
    }
}

#[test]
fn a_chained_cte_in_either_join_slot_keeps_the_whole_original_native_inventory() {
    let named = MssqlRelationReference {
        name: MssqlRelationName::Named(vec!["dbo".into(), "Items".into()]),
        alias: Some("i".into()),
    };
    let left = format!("{CHAIN} SELECT d.id FROM input AS d JOIN dbo.Items AS i ON d.id=i.id");
    assert_eq!(mssql_relational_references(&left), Some(vec![parameter("@Rows", Some("r")), named.clone()]));
    let right = format!("{CHAIN} SELECT d.id FROM dbo.Items AS i JOIN input AS d ON d.id=i.id");
    assert_eq!(mssql_relational_references(&right), Some(vec![named, parameter("@Rows", Some("r"))]));
}

#[test]
fn every_chain_name_and_alias_is_explicit_distinct_and_not_shadowed() {
    for sql in [
        CHAIN.replace("seed AS (", "seed(id) AS ("),
        CHAIN.replace("input AS (", "input(id) AS ("),
        CHAIN.replace("input AS (", "SEED AS ("),
        CHAIN.replace("@Rows AS r", "@Rows r"),
        CHAIN.replace("seed AS p", "seed p"),
        CHAIN.replace("seed AS p", "SEED AS p"),
        CHAIN.replace("p.id", "R.id").replace("seed AS p", "seed AS R"),
        CHAIN.replace("p.id", "seed.id").replace("seed AS p", "seed AS seed"),
        CHAIN.replace("p.id", "input.id").replace("seed AS p", "seed AS input"),
        CHAIN.replace("p.id", "d.id").replace("seed AS p", "seed AS d"),
        CHAIN.replace("r.id", "seed.id").replace("@Rows AS r", "@Rows AS seed"),
        CHAIN.replace("r.id", "input.id").replace("@Rows AS r", "@Rows AS input"),
        CHAIN.replace("seed AS (", "\"seed\" AS ("),
    ] {
        let sql = format!("{sql} SELECT d.id FROM input AS d");
        assert!(mssql_relational_references(&sql).is_none(), "{sql}");
    }
    for alias in ["r", "p", "d", "seed", "input"] {
        let sql = format!("{CHAIN} SELECT d.id FROM input AS d JOIN dbo.Items AS {alias} ON d.id={alias}.id");
        assert!(mssql_relational_references(&sql).is_none(), "{sql}");
    }
    for reference in ["input d", "input AS p", "input AS R", "input AS seed", "INPUT AS d"] {
        let sql = format!("{CHAIN} SELECT d.id FROM {reference}");
        assert!(mssql_relational_references(&sql).is_none(), "{sql}");
    }
}

#[test]
fn the_predecessor_and_terminal_cte_are_each_consumed_once_without_recursion_or_an_extra_scope() {
    for sql in [
        format!("{CHAIN} SELECT * FROM seed AS d"),
        format!("{CHAIN} SELECT * FROM @Other AS o"),
        format!("{CHAIN} SELECT * FROM input AS d JOIN seed AS e ON d.id=e.id"),
        format!("{CHAIN} SELECT * FROM input AS d JOIN input AS e ON d.id=e.id"),
        format!("{CHAIN}, third AS (SELECT * FROM input AS d) SELECT * FROM third AS e"),
        CHAIN.replace("FROM seed AS p", "FROM input AS p") + " SELECT * FROM input AS d",
        CHAIN.replace("FROM seed AS p", "FROM @Other AS p") + " SELECT * FROM input AS d",
        CHAIN.replace("FROM seed AS p", "FROM dbo.seed AS p") + " SELECT * FROM input AS d",
        CHAIN.replace("FROM seed AS p", "FROM dbo.expand(seed) AS p") + " SELECT * FROM input AS d",
        format!("{CHAIN} SELECT * FROM (SELECT * FROM input AS x) AS d"),
        format!("{CHAIN} SELECT * FROM input AS d CROSS APPLY dbo.expand(d.id) AS x"),
        format!("{CHAIN} SELECT * FROM input AS d WHERE EXISTS(SELECT 1 FROM @Other)"),
        format!("{CHAIN} SELECT dbo.hidden(d.id) FROM input AS d"),
        format!("{CHAIN} SELECT * FROM input AS d; SELECT * FROM @Other AS o"),
    ] {
        assert!(mssql_relational_references(&sql).is_none(), "{sql}");
    }
}

#[test]
fn both_chain_bodies_use_the_same_closed_direct_projection_grammar() {
    for projection in [
        "id",
        "foreign.id",
        "p.id+1",
        "ABS(p.id)",
        "COUNT_BIG(*)",
        "DISTINCT p.id",
        "TOP(1) p.id",
        "p.id,p.id",
        "p.*",
        "p.id AS name COLLATE Latin1_General_100_CI_AS",
    ] {
        let sql =
            format!("{} SELECT d.id FROM input AS d", CHAIN.replace("SELECT p.id", &format!("SELECT {projection}")));
        assert!(mssql_relational_references(&sql).is_none(), "{sql}");
    }
    for (before, after) in [
        ("FROM @Rows AS r)", "FROM @Rows AS r WHERE r.id>0)"),
        ("FROM seed AS p)", "FROM seed AS p WHERE p.id>0)"),
        ("FROM seed AS p)", "FROM seed AS p GROUP BY p.id)"),
        ("FROM seed AS p)", "FROM seed AS p JOIN @Other AS o ON p.id=o.id)"),
        ("SELECT r.id", "SELECT dbo.hidden(r.id)"),
        ("SELECT p.id", "SELECT (SELECT id FROM @Other)"),
        ("FROM seed AS p", "FROM (SELECT * FROM seed AS x) AS p"),
    ] {
        let sql = format!("{} SELECT d.id FROM input AS d", CHAIN.replace(before, after));
        assert!(mssql_relational_references(&sql).is_none(), "{sql}");
    }
}

#[test]
fn chained_ctes_keep_kind_unknown_and_reuse_existing_statement_and_inventory_bounds() {
    let sql = format!("{} SELECT * FROM input AS d", CHAIN.replace("@Rows", "@Local"));
    assert_eq!(mssql_relational_references(&sql), Some(vec![parameter("@Local", Some("r"))]));
    let bounded = format!("{sql} /*{}*/", "x".repeat(MAX_RELATIONAL_SQL_BYTES - sql.len() - 5));
    assert_eq!(bounded.len(), MAX_RELATIONAL_SQL_BYTES);
    assert!(mssql_relational_references(&bounded).is_some());
    assert!(mssql_relational_references(&(bounded + " ")).is_none());
    let mut sql = format!("{CHAIN} SELECT * FROM input AS d");
    for at in 1..MAX_RELATIONAL_REFERENCES {
        sql.push_str(&format!(" CROSS JOIN @Other{at} AS other{at}"));
    }
    assert_eq!(mssql_relational_references(&sql).unwrap().len(), MAX_RELATIONAL_REFERENCES);
    sql.push_str(" CROSS JOIN @Overflow AS overflow");
    assert!(mssql_relational_references(&sql).is_none());
}
