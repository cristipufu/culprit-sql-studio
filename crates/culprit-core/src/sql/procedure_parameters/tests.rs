use super::*;

#[test]
fn only_native_procedure_header_readonly_parameters_establish_table_parameters() {
    for text in [
        "CREATE PROCEDURE dbo.consume @rows dbo.RowsType READONLY AS SELECT * FROM @rows",
        "ALTER PROC [dbo].[consume] @n int=1,@rows AS [dbo].[RowsType] READONLY AS BEGIN SELECT * FROM @rows END",
        "CREATE OR ALTER PROC dbo.consume @rows dbo.RowsType READONLY, @limit decimal(10,2)=1.25 AS SELECT 'body may be truncated",
    ] {
        let parameters = mssql_procedure_table_parameters(text).unwrap_or_else(|| panic!("complete header: {text}"));
        assert_eq!(parameters.len(), 1);
        assert_eq!(parameters.get("@rows").unwrap(), &["dbo", "RowsType"]);
    }
}

#[test]
fn typed_locals_body_strings_dynamic_sql_and_incomplete_headers_do_not_create_parameter_evidence() {
    for text in [
        "DECLARE @rows dbo.RowsType",
        "SELECT 'CREATE PROC dbo.consume @rows dbo.RowsType READONLY AS SELECT 1'",
        "EXEC(N'CREATE PROC dbo.consume @rows dbo.RowsType READONLY AS SELECT 1')",
        "CREATE PROC dbo.consume @rows dbo.RowsType READONLY",
        "CREATE PROC dbo.consume @rows dbo.RowsType READONLY, AS SELECT 1",
        "CREATE PROC dbo.consume @rows dbo.RowsType READONLY WITH RECOMPILE AS SELECT 1",
        "CREATE FUNCTION dbo.consume(@rows dbo.RowsType READONLY) RETURNS TABLE AS RETURN SELECT 1",
        "CREATE PROC dbo.consume @rows dbo.RowsType READONLY WITH EXECUTE AS OWNER AS SELECT 1",
    ] {
        assert_eq!(mssql_procedure_table_parameters(text), None, "{text}");
    }
    let local = "CREATE PROC dbo.consume @n int AS DECLARE @rows dbo.RowsType; SELECT * FROM @rows";
    assert_eq!(mssql_procedure_table_parameters(local), Some(BTreeMap::new()));
}

#[test]
fn comments_and_literals_cannot_supply_a_fake_header_boundary_or_readonly_parameter() {
    let parameters = mssql_procedure_table_parameters(
        "CREATE PROC dbo.consume @message nvarchar(100)='AS READONLY, @fake dbo.T', /* AS */ @rows dbo.RowsType READONLY AS SELECT 1",
    ).unwrap();
    assert_eq!(parameters.keys().map(String::as_str).collect::<Vec<_>>(), vec!["@rows"]);
    assert_eq!(mssql_procedure_table_parameters("CREATE PROC dbo.consume @rows dbo.RowsType /* AS */ READONLY"), None);
}

#[test]
fn quoted_type_names_and_case_sensitive_parameter_names_keep_their_native_identity() {
    let parameters = mssql_procedure_table_parameters(
        "CREATE PROC [dbo].[consume] @Rows [dbo].[Rows]]Type] READONLY,@rows [dbo].[other] READONLY AS SELECT 1",
    )
    .unwrap();
    assert_eq!(parameters["@Rows"], vec!["dbo", "Rows]Type"]);
    assert_eq!(parameters["@rows"], vec!["dbo", "other"]);
    assert!(!parameters.contains_key("@ROWS"));
}
