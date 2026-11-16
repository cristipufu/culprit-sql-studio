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
