use super::*;

fn parameter(name: &str, alias: Option<&str>) -> MssqlRelationReference {
    MssqlRelationReference { name: MssqlRelationName::Parameter(name.into()), alias: alias.map(str::to_owned) }
}

#[test]
fn direct_inputs_keep_exact_names_and_literal_or_comment_text_is_not_a_reference() {
    for text in ["SELECT * FROM @Rows", "SELECT N'dbo.f(@Other)' AS label FROM @Rows -- @Other"] {
        assert_eq!(mssql_relational_references(text), Some(vec![parameter("@Rows", None)]), "{text}");
    }
    assert_eq!(
        mssql_relational_references("SELECT r.id,r.value FROM @Rows AS r WHERE r.id > 10;"),
        Some(vec![parameter("@Rows", Some("r"))])
    );
}

#[test]
fn explicit_join_forms_preserve_every_relation_and_its_original_spelling() {
    for join in ["JOIN", "INNER JOIN", "LEFT JOIN", "LEFT OUTER JOIN", "RIGHT JOIN", "FULL OUTER JOIN"] {
        let text = format!("SELECT r.id FROM @Rows AS r {join} [Sales].[Line Items] AS [L] ON r.id=L.id");
        assert_eq!(
            mssql_relational_references(&text),
            Some(vec![
                parameter("@Rows", Some("r")),
                MssqlRelationReference {
                    name: MssqlRelationName::Named(vec!["Sales".into(), "Line Items".into()]),
                    alias: Some("L".into()),
                },
            ]),
            "{text}"
        );
    }
    assert_eq!(
        mssql_relational_references("SELECT r.id FROM @Rows r CROSS JOIN @Local l"),
        Some(vec![parameter("@Rows", Some("r")), parameter("@Local", Some("l"))])
    );
}

#[test]
fn bracketed_dots_escaped_brackets_and_explicit_database_names_stay_distinct() {
    let text = "SELECT r.id FROM @Rows r JOIN [Other.Db].[s]]chema].[Ta.ble] AS [a]]lias] ON r.id=[a]]lias].id";
    assert_eq!(
        mssql_relational_references(text).unwrap()[1],
        MssqlRelationReference {
            name: MssqlRelationName::Named(vec!["Other.Db".into(), "s]chema".into(), "Ta.ble".into()]),
            alias: Some("a]lias".into()),
        }
    );
    assert_eq!(
        mssql_relational_references("SELECT * FROM [@Rows]").unwrap()[0].name,
        MssqlRelationName::Named(vec!["@Rows".into()]),
        "a quoted table name is not an original parameter token"
    );
}

#[test]
fn lexical_mode_disagreement_never_resolves_a_relation_or_alias() {
    for text in [
        "SELECT * FROM \"@Rows\"",
        "SELECT * FROM @Rows \"r\"",
        "SELECT r.id FROM @Rows r JOIN \"dbo\".\"Items\" i ON r.id=i.id",
    ] {
        assert!(mssql_relational_references(text).is_none(), "{text}");
    }
}

#[test]
fn unsupported_expansion_scopes_and_ambiguous_joins_are_not_partial_reference_lists() {
    for text in [
        "SELECT * FROM dbo.expand(@Rows)",
        "SELECT r.id,dbo.expand(r.id) FROM @Rows AS r",
        "SELECT r.id FROM @Rows r CROSS APPLY dbo.expand(r.id) f",
        "SELECT r.id FROM @Rows r OUTER APPLY dbo.expand(r.id) f",
        "WITH rows AS (SELECT * FROM @Rows) SELECT * FROM rows",
        "SELECT * FROM (SELECT * FROM @Rows) r",
        "SELECT * FROM @Rows WHERE id IN(SELECT id FROM dbo.Items)",
        "SELECT * FROM @Rows UNION ALL SELECT * FROM dbo.Items",
        "SELECT * FROM @Rows r, dbo.Items i",
        "SELECT * FROM @Rows JOIN dbo.Items i ON @Rows.id=i.id",
        "SELECT * FROM @Rows r JOIN dbo.Items ON r.id=Items.id",
        "SELECT * FROM @Rows r JOIN dbo.Items R ON r.id=R.id",
        "SELECT * FROM @Rows r JOIN dbo.Items i",
        "SELECT * FROM @Rows r JOIN dbo.Items i ON",
        "SELECT * FROM @Rows r CROSS JOIN dbo.Items i ON r.id=i.id",
        "SELECT * FROM @Rows r JOIN server.db.dbo.Items i ON r.id=i.id",
        "SELECT * FROM @Rows r JOIN dbo..Items i ON r.id=i.id",
        "SELECT * FROM @Rows r JOIN dbo.Items i WITH (NOLOCK) ON r.id=i.id",
        "SELECT * INTO #copy FROM @Rows",
        "EXEC(N'SELECT * FROM @Rows')",
        "SELECT * FROM @Rows;SELECT * FROM @Other",
        "SELECT * FROM @Rows; /* unclosed",
    ] {
        assert!(mssql_relational_references(text).is_none(), "{text}");
    }
}

#[test]
fn relation_inventory_is_bounded_without_dropping_extra_inputs() {
    let mut text = String::from("SELECT * FROM @Rows r");
    for at in 1..MAX_RELATIONAL_REFERENCES {
        text.push_str(&format!(" CROSS JOIN @Rows{at} r{at}"));
    }
    assert_eq!(mssql_relational_references(&text).unwrap().len(), MAX_RELATIONAL_REFERENCES);
    text.push_str(" CROSS JOIN @Overflow overflow");
    assert!(mssql_relational_references(&text).is_none());
}

#[test]
fn aggregate_and_scalar_expressions_preserve_the_closed_native_relation_inventory() {
    for expression in [
        "COUNT_BIG(*)",
        "COUNT(DISTINCT r.id)",
        "SUM(ABS(r.id))",
        "AVG(NULLIF(r.id,0))",
        "MIN(r.id), MAX(r.id)",
        "COALESCE(r.id,1)",
        "ISNULL(r.id,0)",
        "(r.id + 1) * (r.id - 1)",
        "CASE WHEN (r.id > 0) THEN ABS(r.id) ELSE 0 END",
    ] {
        let sql = format!("SELECT {expression} FROM @Rows AS r WHERE (r.id > 1 AND (r.id IN (2,3) OR r.id < 0))");
        assert_eq!(mssql_relational_references(&sql), Some(vec![parameter("@Rows", Some("r"))]), "{sql}");
    }
    let sql = "SELECT COUNT_BIG(*),SUM(ABS(r.id)) FROM @Rows r JOIN dbo.Items i ON (COALESCE(r.id,0)=i.id AND (i.id > 0)) WHERE (r.id > 0) GROUP BY (r.id) HAVING COUNT_BIG(*) > 1 ORDER BY ABS(r.id)";
    assert_eq!(
        mssql_relational_references(sql).unwrap(),
        vec![
            parameter("@Rows", Some("r")),
            MssqlRelationReference {
                name: MssqlRelationName::Named(vec!["dbo".into(), "Items".into()]),
                alias: Some("i".into())
            },
        ]
    );
}

#[test]
fn recognized_builtin_spelling_does_not_admit_unknown_functions_hidden_relations_or_lexical_disagreement() {
    for sql in [
        "SELECT dbo.COUNT_BIG(r.id) FROM @Rows r",
        "SELECT [COUNT_BIG](r.id) FROM @Rows r",
        "SELECT unknown_function(r.id) FROM @Rows r",
        "SELECT SUM(dbo.hidden(r.id)) FROM @Rows r",
        "SELECT COUNT_BIG(*) FROM dbo.expand(@Rows)",
        "SELECT (SELECT COUNT_BIG(*) FROM @Other) FROM @Rows r",
        "SELECT COUNT_BIG(*) FROM @Rows r WHERE r.id IN (SELECT id FROM dbo.Items)",
        "SELECT COUNT_BIG(*) FROM @Rows r WHERE EXISTS (SELECT 1 FROM @Other)",
        "SELECT COUNT_BIG(*) FROM (@Rows) r",
        "SELECT COUNT_BIG(*) FROM @Rows r CROSS APPLY dbo.expand(r.id) f",
        "SELECT COUNT_BIG(*) FROM @Rows r JOIN dbo.Items i ON (r.id=i.id), dbo.More m",
        "SELECT COUNT_BIG(*) FROM @Rows r WITH (NOLOCK)",
        "SELECT COUNT_BIG(*) FROM @Rows r OPTION (RECOMPILE)",
        "SELECT COUNT_BIG(*) FROM @Rows r UNION ALL SELECT COUNT_BIG(*) FROM @Other",
        "SELECT COUNT_BIG(*) FROM @Rows r; SELECT COUNT_BIG(*) FROM @Other",
        "SELECT ABS((r.id) FROM @Rows r",
        "SELECT ABS(r.id)) FROM @Rows r",
        "SELECT \"COUNT_BIG\"(*) FROM @Rows r",
        "SELECT COUNT_BIG(*) FROM \"@Rows\" r",
    ] {
        assert!(mssql_relational_references(sql).is_none(), "{sql}");
    }
}

#[test]
fn expression_limits_refuse_the_whole_inventory_instead_of_retaining_a_prefix() {
    let depth = expressions::MAX_EXPRESSION_DEPTH;
    let sql = format!("SELECT {}r.id{} FROM @Rows r", "(".repeat(depth), ")".repeat(depth));
    assert_eq!(mssql_relational_references(&sql), Some(vec![parameter("@Rows", Some("r"))]));
    let excessive = format!("SELECT ({}) FROM @Rows r", &sql[7..sql.find(" FROM").unwrap()]);
    assert!(mssql_relational_references(&excessive).is_none());
    let sql = "SELECT COUNT_BIG(*) FROM @Rows r";
    let bounded = format!("{sql} /*{}*/", "x".repeat(MAX_RELATIONAL_SQL_BYTES - sql.len() - 5));
    assert_eq!(bounded.len(), MAX_RELATIONAL_SQL_BYTES);
    assert!(mssql_relational_references(&bounded).is_some());
    assert!(mssql_relational_references(&(bounded + " ")).is_none());
}
