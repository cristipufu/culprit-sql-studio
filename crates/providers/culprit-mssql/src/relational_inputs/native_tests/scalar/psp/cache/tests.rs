use super::*;
use crate::{
    cached_definitions,
    resources::test_support::{FakeRead, replace},
    value::{Rec, Val},
};

const STATEMENT_HANDLE: &str = "0x09000000000000000000000000000001";
const QUERY_HASH: &str = "0x0123456789ABCDEF";
const PLAN_HASH: &str = "0x0000000000000001";
fn actual() -> Vec<String> {
    vec![format!(
        r#"<ShowPlanXML><BatchSequence><Batch><Statements><StmtSimple StatementText="SELECT id AS native_payload_length FROM dbo.t WHERE bucket=@bucket" QueryHash="{QUERY_HASH}" QueryPlanHash="{PLAN_HASH}" StatementSqlHandle="{STATEMENT_HANDLE}" DatabaseContextSettingsId="17"><QueryPlan QueryVariantID="1"><RelOp NodeId="0" PhysicalOp="Index Seek"><RunTimeInformation><RunTimeCountersPerThread Thread="0" ActualRows="1" ActualExecutions="1"/></RunTimeInformation></RelOp></QueryPlan></StmtSimple></Statements></Batch></BatchSequence></ShowPlanXML>"#
    )]
}
fn point() -> Rec {
    let base = replace(
        &cached_definitions::tests::row(),
        &[
            ("query_hash", Val::Text(QUERY_HASH.into())),
            ("query_plan_hash", Val::Text(PLAN_HASH.into())),
            // A native prepared cache ID deliberately differs from the owning stored procedure ID.
            ("module_object_id", Val::Int(987654321)),
        ],
    );
    let mut fields = base.columns().map(|(name, value)| (name, value.clone())).collect::<Vec<_>>();
    fields
        .extend([("statement_sql_handle", Val::Text(STATEMENT_HANDLE.into())), ("statement_context_id", Val::Int(17))]);
    Rec::of(&fields)
}
fn database() -> u32 {
    crate::cached_statements::context(&point()).unwrap().database_id
}
fn stopped(error: &culprit_core::Error, reason: CollectionStop) {
    assert_eq!(error.detail, Some(culprit_core::ErrorDetail::CollectionStopped(reason)));
}

#[tokio::test]
async fn actual_statement_handle_and_context_select_prepared_native_identity_without_a_module_id_guess() {
    let mut reader = FakeRead::new(vec![Ok(vec![point()])]);
    let selected = lookup(&mut reader, database(), &actual(), None).await.unwrap();
    assert_eq!(
        selected.identity.key().unwrap(),
        crate::cached_statements::context(&point()).unwrap().identity.key().unwrap()
    );
    assert_eq!(reader.calls.len(), 1);
    assert_eq!(reader.calls[0].limit, LOOKUP_ROWS);
    assert!(reader.calls[0].sql.contains("qs.statement_sql_handle=CONVERT(varbinary(64),@P3,1)"));
    assert!(reader.calls[0].sql.contains("qs.statement_context_id=@P4"));
    assert!(reader.calls[0].sql.contains("qs.query_hash=CONVERT(binary(8),@P5,1)"));
    assert!(reader.calls[0].sql.contains("qs.query_plan_hash=CONVERT(binary(8),@P6,1)"));
    assert!(!reader.calls[0].sql.contains("module_object_id="));
}

#[tokio::test]
async fn missing_or_invalid_actual_native_identity_refuses_before_any_lookup_sql() {
    let original = actual().remove(0);
    let mut cases = vec![vec![original.clone(), original.clone()]];
    for (before, after) in [
        (format!("StatementSqlHandle=\"{STATEMENT_HANDLE}\""), String::new()),
        ("DatabaseContextSettingsId=\"17\"".into(), String::new()),
        (STATEMENT_HANDLE.into(), "0x00000000000000000000000000000000".into()),
        (STATEMENT_HANDLE.into(), "0xXYZ".into()),
        ("DatabaseContextSettingsId=\"17\"".into(), "DatabaseContextSettingsId=\"-1\"".into()),
        ("QueryVariantID=\"1\"".into(), String::new()),
    ] {
        cases.push(vec![original.replace(&before, &after)]);
    }
    for xml in cases {
        let mut reader = FakeRead::new(vec![]);
        let error = lookup(&mut reader, database(), &xml, None).await.unwrap_err();
        stopped(&error, CollectionStop::SourceUnavailable);
        assert!(reader.calls.is_empty());
    }
}

#[tokio::test]
async fn missing_or_duplicate_native_lookup_rows_are_unavailable_or_invalid_never_first_row() {
    for (rows, reason) in
        [(vec![], CollectionStop::SourceUnavailable), (vec![point(), point()], CollectionStop::InvalidPage)]
    {
        let mut reader = FakeRead::new(vec![Ok(rows)]);
        let error = lookup(&mut reader, database(), &actual(), None).await.unwrap_err();
        stopped(&error, reason);
        assert_eq!(reader.calls.len(), 1);
    }
}

#[tokio::test]
async fn returned_foreign_database_context_handle_or_plan_cannot_override_actual_native_guards() {
    for (column, value) in [
        ("database_id", Val::Int(i64::from(database()) + 1)),
        ("statement_context_id", Val::Int(18)),
        ("statement_sql_handle", Val::Text("0x09000000000000000000000000000002".into())),
        ("query_hash", Val::Text("0xFEDCBA9876543210".into())),
        ("query_plan_hash", Val::Text("0x0000000000000002".into())),
        ("plan_handle", Val::Bytes(vec![0; 64])),
        ("sql_handle", Val::Bytes(vec![0; 64])),
    ] {
        let mut reader = FakeRead::new(vec![Ok(vec![replace(&point(), &[(column, value)])])]);
        let error = lookup(&mut reader, database(), &actual(), None).await.unwrap_err();
        stopped(&error, CollectionStop::SourceChanged);
        assert_eq!(reader.calls.len(), 1);
    }
}

#[tokio::test]
async fn changed_native_cache_generation_or_sql_token_never_replaces_the_original() {
    let original = crate::cached_statements::context(&point()).unwrap();
    for (column, value) in [
        ("plan_generation_num", Val::Int(9)),
        ("sql_handle", Val::Bytes(vec![9; 64])),
        ("creation_time", Val::Text("2026-01-01T00:00:00.000".into())),
    ] {
        let mut reader = FakeRead::new(vec![Ok(vec![replace(&point(), &[(column, value)])])]);
        let error = lookup(&mut reader, database(), &actual(), Some(&original)).await.unwrap_err();
        stopped(&error, CollectionStop::SourceChanged);
    }
}

#[tokio::test]
async fn cancellation_and_native_lifecycle_errors_never_start_the_payload_projection() {
    let cancellation = CollectionCancellation::default();
    cancellation.cancel();
    let mut reader = FakeRead::new(vec![]);
    let error = definitions(&mut reader, database(), &actual(), None, &cancellation).await.err().unwrap();
    stopped(&error, CollectionStop::Cancelled);
    assert!(reader.calls.is_empty());
    let mut reader = FakeRead::new(vec![Err(ReadError::Stopped(CollectionStop::TimedOut))]);
    let error = lookup(&mut reader, database(), &actual(), None).await.unwrap_err();
    stopped(&error, CollectionStop::TimedOut);
    assert_eq!(reader.calls.len(), 1);
}
