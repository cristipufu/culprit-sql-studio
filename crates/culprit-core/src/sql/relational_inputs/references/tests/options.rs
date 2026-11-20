use super::*;

#[test]
fn terminal_execution_hints_preserve_named_and_parameter_relation_spelling() {
    let tail = "OPTION (HASH GROUP, MAXDOP 1, MIN_GRANT_PERCENT=70, MAX_GRANT_PERCENT=70)";
    assert_eq!(
        mssql_relational_references(&format!("SELECT COUNT_BIG(r.id) FROM @Rows AS r {tail}")),
        Some(vec![parameter("@Rows", Some("r"))])
    );
    assert_eq!(
        mssql_relational_references(&format!(
            "SELECT COUNT_BIG(r.id) FROM [dbo].[Rows] AS r WHERE r.bucket=@bucket {tail}"
        )),
        Some(vec![MssqlRelationReference {
            name: MssqlRelationName::Named(vec!["dbo".into(), "Rows".into()]),
            alias: Some("r".into())
        }])
    );
}
#[test]
fn admitted_hints_are_independent_of_order_case_and_non_numeric_comment_positions() {
    for tail in [
        "OPTION(MAXDOP 1)",
        "option(hash group)",
        "OPTION (MAX_GRANT_PERCENT=100.0,MIN_GRANT_PERCENT=0,HASH GROUP,MAXDOP 0)",
        "OPTION(/*native*/ HASH/*native*/GROUP,/*native*/MAXDOP 4)",
    ] {
        let sql = format!("; SELECT COUNT_BIG(r.id),N'OPTION(MAXDOP 7)' FROM @Rows AS r {tail}; -- original comment");
        assert_eq!(mssql_relational_references(&sql), Some(vec![parameter("@Rows", Some("r"))]), "{tail}");
    }
}
#[test]
fn documented_integer_and_decimal_bounds_do_not_use_float_rounding() {
    for value in ["0", "1", "32767", "+1", "0001"] {
        assert!(
            mssql_relational_references(&format!("SELECT * FROM @Rows r OPTION(MAXDOP {value})")).is_some(),
            "{value}"
        );
    }
    for value in ["0", ".5", "70.125", "100", "100.0000", "+0.5", "001.00", "0.0000000000000000000000000000000000001"] {
        for hint in ["MIN_GRANT_PERCENT", "MAX_GRANT_PERCENT"] {
            assert!(
                mssql_relational_references(&format!("SELECT * FROM @Rows r OPTION({hint}={value})")).is_some(),
                "{hint}:{value}"
            );
        }
    }
    for tail in [
        "MAXDOP -1",
        "MAXDOP 32768",
        "MAXDOP 1.0",
        "MAXDOP =1",
        "MAX_GRANT_PERCENT=-1",
        "MIN_GRANT_PERCENT=100.0000000000000000000000001",
        "MAX_GRANT_PERCENT=101",
        "MAX_GRANT_PERCENT=1e1",
        "MIN_GRANT_PERCENT=0.00000000000000000000000000000000000001",
    ] {
        assert!(mssql_relational_references(&format!("SELECT * FROM @Rows r OPTION({tail})")).is_none(), "{tail}");
    }
}
#[test]
fn malformed_duplicate_unknown_nested_or_nonterminal_options_never_supply_partial_relations() {
    for tail in [
        "OPTION()",
        "OPTION(HASH)",
        "OPTION(HASH GROUP,HASH GROUP)",
        "OPTION(MAXDOP 1,MAXDOP 2)",
        "OPTION(MIN_GRANT_PERCENT=0,MIN_GRANT_PERCENT=1)",
        "OPTION(MAXDOP 1,)",
        "OPTION(,MAXDOP 1)",
        "OPTION(MAXDOP 1 MAX_GRANT_PERCENT=70)",
        "OPTION(MAXDOP 1) OPTION(HASH GROUP)",
        "OPTION(OPTION(MAXDOP 1))",
        "OPTION(MAXDOP (1))",
        "OPTION(RECOMPILE)",
        "OPTION(OPTIMIZE FOR(@p=1))",
        "OPTION(USE PLAN N'<plan/>')",
        "OPTION(MAXDOP 1) WHERE r.id=1",
        "OPTION(MAXDOP 1); SELECT * FROM @Other",
        "OPTION(MAXDOP 1) JOIN dbo.Other o ON r.id=o.id",
        "OPTION(MAXDOP 1",
        "OPTION(MAXDOP 1))",
    ] {
        assert!(mssql_relational_references(&format!("SELECT * FROM @Rows AS r {tail}")).is_none(), "{tail}");
    }
}
#[test]
fn omitted_literals_or_split_numeric_tokens_cannot_forge_a_valid_hint_operand() {
    for tail in [
        "OPTION(MAXDOP '1')",
        "OPTION(MAXDOP 1 'omitted')",
        "OPTION(MAXDOP 1.1)",
        "OPTION(MAXDOP 1 0)",
        "OPTION(MIN_GRANT_PERCENT=7 0)",
        "OPTION(MIN_GRANT_PERCENT=70 .5)",
        "OPTION(MIN_GRANT_PERCENT=7'ignored'0)",
        "OPTION(HASH 'ignored' GROUP)",
        "OPTION([MAXDOP] 1)",
        "OPTION(\"MAXDOP\" 1)",
        "OPTION(MIN_GRANT_PERCENT=@p)",
        "OPTION(MAXDOP unknown(1))",
    ] {
        assert!(mssql_relational_references(&format!("SELECT * FROM @Rows r {tail}")).is_none(), "{tail}");
    }
}
#[test]
fn execution_hints_do_not_repair_ambiguous_aliases_or_nested_population_changing_scopes() {
    for body in [
        "SELECT * FROM @Rows r JOIN dbo.Items R ON r.id=R.id",
        "SELECT * FROM @Rows r CROSS APPLY dbo.expand(r.id) x",
        "SELECT * FROM (SELECT * FROM @Rows r WHERE r.id>0) AS d",
        "SELECT (SELECT id FROM @Other) FROM @Rows r",
        "SELECT * FROM @Rows r WHERE id IN(SELECT id FROM @Other)",
        "SELECT * FROM @Rows r UNION SELECT * FROM @Other o",
        "SELECT * FROM @Rows r WITH(NOLOCK)",
    ] {
        assert!(mssql_relational_references(&format!("{body} OPTION(MAXDOP 1)")).is_none(), "{body}");
    }
    assert!(
        mssql_relational_references("WITH p AS (SELECT * FROM @Rows AS r OPTION(MAXDOP 1)) SELECT * FROM p AS q")
            .is_none()
    );
}
#[test]
fn terminal_hints_preserve_delivered_closed_scope_alias_ownership() {
    for body in [
        "SELECT COUNT_BIG(d.id) FROM (SELECT r.id FROM @Rows AS r) AS d",
        "WITH p AS (SELECT r.id FROM @Rows AS r) SELECT COUNT_BIG(q.id) FROM p AS q",
    ] {
        assert_eq!(
            mssql_relational_references(&format!("{body} OPTION(HASH GROUP,MAXDOP 1)")),
            Some(vec![parameter("@Rows", Some("r"))]),
            "{body}"
        );
    }
}
