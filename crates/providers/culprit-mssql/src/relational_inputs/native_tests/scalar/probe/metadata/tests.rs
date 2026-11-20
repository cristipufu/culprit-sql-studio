use super::*;
use crate::{
    relational_inputs::tests::binding_row,
    resources::test_support::{FakeRead, replace},
    value::{Rec, Val},
};

const OBJECT: i64 = 1001;
const STATEMENT: &str = "SELECT COUNT_BIG(pad) FROM dbo.scalar_rows WHERE bucket=@bucket";
fn row() -> Rec {
    let prefix = "CREATE PROCEDURE dbo.scalar_probe @bucket int AS ";
    let header = format!("{prefix}{STATEMENT}");
    replace(
        &crate::cached_definitions::tests::module_row(STATEMENT, crate::cached_definitions::tests::XML),
        &[
            ("text_prefix", Val::Text(header.clone())),
            ("source_text_bytes", Val::Int((header.encode_utf16().count() * 2) as i64)),
            ("statement_start_offset", Val::Int((prefix.encode_utf16().count() * 2) as i64)),
        ],
    )
}

#[tokio::test]
async fn one_exact_module_header_and_existing_independent_binding_replace_all_database_plan_payloads() {
    let row = row();
    let original = cached_statements::context(&row).unwrap();
    let mut reader = FakeRead::new(vec![Ok(vec![row]), Ok(vec![binding_row(&original)])]);
    let selected = read(&mut reader, OBJECT, Some(&original)).await.unwrap();
    assert_eq!(selected.context, original);
    assert_eq!(selected.statement, STATEMENT);
    assert_eq!(selected.module_parameters.object_id, OBJECT as i32);
    assert!(selected.module_parameters.table_parameters.is_empty());
    assert!(selected.module_parameters.is_valid_for(&selected.context));
    assert_eq!(reader.calls.len(), 2);
    let call = &reader.calls[0];
    assert_eq!(call.limit, SELECTED_ROWS);
    assert_eq!(call.parameters.len(), 8);
    assert!(call.sql.contains("TOP(@P1)") && call.sql.contains("st.objectid=@P2 AND st.number=1"));
    assert!(call.sql.contains(cached_statements::DATABASE_PREDICATE));
    assert!(call.sql.contains("st.dbid=DB_ID()"));
    for field in [
        "plan_handle=CONVERT",
        "sql_handle=CONVERT",
        "statement_start_offset=@P5",
        "statement_end_offset=@P6",
        "plan_generation_num=@P7",
    ] {
        assert!(call.sql.contains(field), "{field}");
    }
    for forbidden in [
        "sys.dm_exec_text_query_plan",
        "sys.dm_exec_query_plan_stats",
        "sys.parameters",
        "current_type_schema",
        "sys.columns",
        "scalar_rows",
    ] {
        assert!(!call.sql.contains(forbidden), "{forbidden}");
    }
    assert!(reader.calls[1].sql.contains("sys.dm_exec_plan_attributes(selected.plan_handle)"));
}

#[tokio::test]
async fn absent_or_ambiguous_native_points_and_unverified_bindings_have_no_representative_fallback() {
    let row = row();
    let original = cached_statements::context(&row).unwrap();
    for rows in [vec![], vec![row.clone(), row.clone()]] {
        let mut reader = FakeRead::new(vec![Ok(rows)]);
        assert!(read(&mut reader, OBJECT, Some(&original)).await.is_err());
        assert_eq!(reader.calls.len(), 1);
    }
    let changed = replace(&binding_row(&original), &[("sql_handle", Val::Text("0x9999".into()))]);
    let mut reader = FakeRead::new(vec![Ok(vec![row]), Ok(vec![changed])]);
    let error = read(&mut reader, OBJECT, Some(&original)).await.err().unwrap();
    assert_eq!(CollectionStop::from_error(&error), CollectionStop::SourceChanged);
    assert_eq!(reader.calls.len(), 2);
}

#[tokio::test]
async fn full_creation_generation_handles_statement_database_and_module_ownership_are_rechecked() {
    let original_row = row();
    let original = cached_statements::context(&original_row).unwrap();
    for (field, value) in [
        ("creation_time", Val::Text("2026-11-21T12:00:00.003".into())),
        ("plan_generation_num", Val::Int(9)),
        ("plan_handle", Val::Text("0x9999".into())),
        ("sql_handle", Val::Text("0x9999".into())),
        ("statement_start_offset", Val::Int(0)),
        ("statement_end_offset", Val::Int(i64::from(original.identity.statement_start_bytes) + 2)),
        ("database_id", Val::Int(99)),
        ("query_hash", Val::Text("0x9999999999999999".into())),
        ("module_object_id", Val::Int(99)),
    ] {
        let mut reader = FakeRead::new(vec![Ok(vec![replace(&original_row, &[(field, value)])])]);
        let error = read(&mut reader, OBJECT, Some(&original)).await.err().unwrap();
        assert_eq!(CollectionStop::from_error(&error), CollectionStop::SourceChanged, "{field}");
        assert_eq!(reader.calls.len(), 1, "{field}");
    }
}

#[tokio::test]
async fn missing_partial_encrypted_or_foreign_native_headers_cannot_supply_module_or_statement_context() {
    for (field, value) in [
        ("text_complete", Val::Int(0)),
        ("text_prefix", Val::Null),
        ("encrypted", Val::Bool(true)),
        ("module_database_id", Val::Int(99)),
        ("module_number", Val::Int(2)),
    ] {
        let mut reader = FakeRead::new(vec![Ok(vec![replace(&row(), &[(field, value)])])]);
        assert!(read(&mut reader, OBJECT, None).await.is_err(), "{field}");
        assert_eq!(reader.calls.len(), 1, "{field}");
    }
}

#[tokio::test]
async fn cancellation_timeout_and_unconfirmed_native_reads_never_start_an_independent_binding_query() {
    for stop in [CollectionStop::Cancelled, CollectionStop::TimedOut, CollectionStop::CancellationUnconfirmed] {
        let mut reader = FakeRead::new(vec![Err(ReadError::Stopped(stop))]);
        let error = read(&mut reader, OBJECT, None).await.err().unwrap();
        assert_eq!(CollectionStop::from_error(&error), stop);
        assert_eq!(reader.calls.len(), 1);
    }
}
