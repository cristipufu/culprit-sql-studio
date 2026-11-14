use super::*;
use crate::{
    resources::test_support::{FakeRead, replace},
    value::Val,
};
fn row(hash: &str) -> Rec {
    let sql = "SELECT 1";
    let context = replace(
        &crate::grants::tests::row(),
        &[("query_hash", Val::Text(hash.into())), ("plan_handle", Val::Text(format!("0x01{}", &hash[2..])))],
    );
    let mut fields = context.columns().map(|(name, value)| (name.to_owned(), value.clone())).collect::<Vec<_>>();
    fields.extend([
        ("payload_included".into(), Val::Bool(true)),
        ("statement_bytes".into(), Val::Int((sql.len() * 2) as i64)),
        ("statement_text".into(), Val::Text(sql.into())),
        ("plan_bytes".into(), Val::Int(2)),
        ("query_plan".into(), Val::Text("x".into())),
        ("object_name".into(), Val::Null),
        ("encrypted".into(), Val::Int(0)),
        ("module_database_id".into(), Val::Null),
        ("module_object_id".into(), Val::Null),
        ("module_number".into(), Val::Null),
    ]);
    Rec::of(&fields.iter().map(|(name, value)| (name.as_str(), value.clone())).collect::<Vec<_>>())
}
fn binding(row: &Rec) -> Rec {
    crate::relational_inputs::tests::binding_row(&crate::cached_statements::context(row).unwrap())
}
#[tokio::test]
async fn only_requested_plans_are_read_and_hashes_never_change_native_sql_templates() {
    let hashes: Vec<String> = vec!["0x0123456789ABCDEF".into(), "0x0123456789ABCDE0".into()];
    let mut reader =
        FakeRead::new(vec![Ok(vec![row(&hashes[0])]), Ok(vec![row(&hashes[1])]), Ok(vec![binding(&row(&hashes[0]))])]);
    let budget = crate::read::ReadSession::application_budget();
    let values = read(&mut reader, &hashes, 1, &budget).await.unwrap();
    assert!(values[&hashes[0]].plan.text().is_some());
    assert!(matches!(values[&hashes[1]].plan, Payload::Unavailable(PayloadUnavailable::NotCollected)));
    assert_eq!(reader.calls.len(), 3);
    assert!(reader.calls[0].sql.contains("sys.dm_exec_text_query_plan"));
    assert!(!reader.calls[1].sql.contains("sys.dm_exec_text_query_plan"));
    for (call, hash) in reader.calls.iter().zip(&hashes) {
        assert!(!call.sql.contains(hash));
        assert!(call.sql.contains(DATABASE_PREDICATE));
        assert!(call.sql.contains("ORDER BY qs.last_execution_time DESC"));
        let Parameter::Text(xml) = &call.parameters[0] else { panic!("bound selectors") };
        assert!(xml.contains(hash));
    }
    assert!(reader.calls[2].sql.contains("sys.dm_exec_plan_attributes(selected.plan_handle)"));
    assert!(!reader.calls[2].sql.contains("sys.dm_exec_query_stats"));
}
#[tokio::test]
async fn malformed_or_duplicate_selectors_and_empty_requests_send_no_sql() {
    let mut reader = FakeRead::new(vec![]);
    let budget = crate::read::ReadSession::application_budget();
    assert!(read(&mut reader, &[], 0, &budget).await.unwrap().is_empty());
    assert!(read(&mut reader, &["0x0123456789ABCDEF".into(), "bad".into()], 1, &budget).await.is_err());
    assert!(read(&mut reader, &["0x0123456789ABCDEF".into(), "0x0123456789abcdef".into()], 1, &budget).await.is_err());
    assert!(reader.calls.is_empty());
}
#[tokio::test]
async fn oversized_or_evicted_payloads_are_explicit_and_late_errors_never_return_complete_details() {
    let budget = crate::read::ReadSession::application_budget();
    let hash = "0x0123456789ABCDEF".to_string();
    let oversized = replace(
        &row(&hash),
        &[("plan_bytes", Val::Int(i64::from(budget.max_plan_bytes) + 2)), ("query_plan", Val::Null)],
    );
    let mut reader = FakeRead::new(vec![Ok(vec![oversized])]);
    let details = read(&mut reader, std::slice::from_ref(&hash), 1, &budget).await.unwrap();
    assert!(matches!(details[&hash].plan, Payload::Unavailable(PayloadUnavailable::Oversized)));
    let mut reader = FakeRead::new(vec![Ok(vec![])]);
    assert!(read(&mut reader, std::slice::from_ref(&hash), 1, &budget).await.unwrap().is_empty());
    let mut reader = FakeRead::new(vec![Err(ReadError::Stopped(CollectionStop::CancellationUnconfirmed))]);
    let error = read(&mut reader, &[hash], 1, &budget).await.err().unwrap();
    assert_eq!(CollectionStop::from_error(&error), CollectionStop::CancellationUnconfirmed);
    assert_eq!(reader.calls.len(), 1);
}

#[tokio::test]
async fn many_small_plans_share_one_native_cache_read() {
    let hashes = (1..=25).map(|value| format!("0x{value:016X}")).collect::<Vec<_>>();
    let rows: Vec<_> = hashes.iter().map(|hash| row(hash)).collect();
    let mut reader = FakeRead::new(vec![Ok(rows.clone()), Ok(rows.iter().map(binding).collect())]);
    let result = read(&mut reader, &hashes, 25, &crate::read::ReadSession::application_budget()).await.unwrap();
    assert_eq!(result.len(), 25);
    assert_eq!(reader.calls.len(), 2, "one payload cache read and one batched independent binding check");
    assert_eq!(reader.calls.iter().filter(|call| call.sql.contains("sys.dm_exec_query_stats")).count(), 1);
}
#[tokio::test]
async fn payload_deferral_reselects_only_the_remaining_members_and_must_make_progress() {
    let hashes = vec!["0x0123456789ABCDEF".to_string(), "0x0123456789ABCDE0".to_string()];
    let deferred = replace(
        &row(&hashes[1]),
        &[("payload_included", Val::Bool(false)), ("statement_text", Val::Null), ("query_plan", Val::Null)],
    );
    let mut reader = FakeRead::new(vec![
        Ok(vec![row(&hashes[0]), deferred.clone()]),
        Ok(vec![row(&hashes[1])]),
        Ok(hashes.iter().map(|hash| binding(&row(hash))).collect()),
    ]);
    let result = read(&mut reader, &hashes, 2, &crate::read::ReadSession::application_budget()).await.unwrap();
    assert_eq!(result.len(), 2);
    assert_eq!(reader.calls.len(), 3);
    let Parameter::Text(second) = &reader.calls[1].parameters[0] else { panic!("bound selectors") };
    assert!(!second.contains(&hashes[0]));
    let mut reader = FakeRead::new(vec![Ok(vec![deferred])]);
    let error =
        read(&mut reader, &hashes[1..], 1, &crate::read::ReadSession::application_budget()).await.err().unwrap();
    assert_eq!(CollectionStop::from_error(&error), CollectionStop::ByteLimit);
    assert_eq!(reader.calls.len(), 1);
}

fn procedure_row(hash: &str) -> Rec {
    let header = "CREATE PROC dbo.consume @rows dbo.RowsType READONLY AS SELECT id FROM @rows";
    let statement = "SELECT id FROM @rows";
    replace(
        &row(hash),
        &[
            ("source_text_bytes", Val::Int(header.encode_utf16().count() as i64 * 2)),
            ("text_prefix", Val::Text(header.into())),
            ("statement_bytes", Val::Int(statement.encode_utf16().count() as i64 * 2)),
            ("statement_text", Val::Text(statement.into())),
            ("object_name", Val::Text("dbo.consume".into())),
            ("module_database_id", Val::Int(5)),
            ("module_object_id", Val::Int(1001)),
            ("module_number", Val::Int(1)),
        ],
    )
}

#[tokio::test]
async fn original_header_metadata_is_admitted_only_after_an_independent_native_binding_check() {
    let hash = "0x0123456789ABCDEF".to_string();
    let native = procedure_row(&hash);
    let mut reader = FakeRead::new(vec![Ok(vec![native.clone()]), Ok(vec![binding(&native)])]);
    let details = read(&mut reader, std::slice::from_ref(&hash), 1, &crate::read::ReadSession::application_budget())
        .await
        .unwrap();
    let detail = &details[&hash];
    assert!(detail.module_parameters.as_ref().unwrap().input(&detail.context, "@rows").is_some());
    assert_eq!(detail.statement.text(), Some("SELECT id FROM @rows"));
    assert!(detail.plan.text().is_some());
    assert!(reader.calls[0].sql.contains("sys.dm_exec_sql_text(r.sql_handle)"));
    assert!(!reader.calls[1].sql.contains("sys.dm_exec_sql_text"), "no second header read");
}

#[tokio::test]
async fn changed_missing_or_denied_binding_withholds_enrichment_but_preserves_the_original_statement() {
    let hash = "0x0123456789ABCDEF".to_string();
    let native = procedure_row(&hash);
    for reply in [
        Ok(vec![replace(&binding(&native), &[("sql_handle", Val::Text("0x9999".into()))])]),
        Ok(vec![replace(&binding(&native), &[("sql_handle", Val::Null)])]),
        Ok(vec![]),
        Err(ReadError::Source(culprit_core::Error::new(
            culprit_core::ErrorCode::PermissionDenied,
            "native permission denied",
        ))),
    ] {
        let mut reader = FakeRead::new(vec![Ok(vec![native.clone()]), reply]);
        let details =
            read(&mut reader, std::slice::from_ref(&hash), 1, &crate::read::ReadSession::application_budget())
                .await
                .unwrap();
        let detail = &details[&hash];
        assert_eq!(detail.statement.text(), Some("SELECT id FROM @rows"));
        assert!(matches!(detail.plan, Payload::Unavailable(PayloadUnavailable::SourceUnavailable)));
        assert!(detail.module_parameters.is_none());
        assert!(detail.object_name.is_none());
        assert_eq!(reader.calls.len(), 2);
    }
}

#[tokio::test]
async fn unconfirmed_binding_cancellation_cannot_publish_a_successful_detail_or_issue_followup_sql() {
    let hash = "0x0123456789ABCDEF".to_string();
    let mut reader = FakeRead::new(vec![
        Ok(vec![procedure_row(&hash)]),
        Err(ReadError::Stopped(CollectionStop::CancellationUnconfirmed)),
    ]);
    let error = read(&mut reader, &[hash], 1, &crate::read::ReadSession::application_budget()).await.err().unwrap();
    assert_eq!(CollectionStop::from_error(&error), CollectionStop::CancellationUnconfirmed);
    assert_eq!(reader.calls.len(), 2);
}

#[tokio::test]
async fn unsupported_system_modules_and_zero_native_tokens_preserve_sql_without_false_module_proof() {
    let hash = "0x0123456789ABCDEF".to_string();
    let system = replace(&procedure_row(&hash), &[("module_object_id", Val::Int(-1001))]);
    let mut reader = FakeRead::new(vec![Ok(vec![system.clone()]), Ok(vec![binding(&system)])]);
    let budget = crate::read::ReadSession::application_budget();
    let details = read(&mut reader, std::slice::from_ref(&hash), 1, &budget).await.unwrap();
    assert_eq!(details[&hash].statement.text(), Some("SELECT id FROM @rows"));
    assert!(details[&hash].module_parameters.is_none());
    assert!(
        details[&hash].plan.text().is_some(),
        "a negative module ID does not invalidate otherwise verified plan text"
    );
    for (column, zero) in [("plan_handle", "0x0000"), ("sql_handle", "0x0000")] {
        let native = replace(&procedure_row(&hash), &[(column, Val::Text(zero.into()))]);
        let mut reader = FakeRead::new(vec![Ok(vec![native])]);
        let details = read(&mut reader, std::slice::from_ref(&hash), 1, &budget).await.unwrap();
        assert_eq!(details[&hash].statement.text(), Some("SELECT id FROM @rows"));
        assert!(matches!(details[&hash].plan, Payload::Unavailable(PayloadUnavailable::NotCollected)));
        assert!(details[&hash].module_parameters.is_none());
        assert_eq!(reader.calls.len(), 1, "unqualified tokens must not trigger a native plan-attribute lookup");
    }
}

#[tokio::test]
async fn conflicting_payload_versions_keep_all_statements_but_withdraw_enrichment_without_further_sql() {
    let hashes: Vec<String> =
        vec!["0x0123456789ABCDEF".into(), "0x0123456789ABCDE0".into(), "0x0123456789ABCDE1".into()];
    let first = replace(&procedure_row(&hashes[0]), &[("plan_handle", Val::Text("0x0200".into()))]);
    let second = replace(
        &procedure_row(&hashes[1]),
        &[("plan_handle", Val::Text("0x0200".into())), ("sql_handle", Val::Text("0x0301".into()))],
    );
    let mut reader = FakeRead::new(vec![Ok(vec![first, second]), Ok(vec![procedure_row(&hashes[2])])]);
    let details = read(&mut reader, &hashes, 2, &crate::read::ReadSession::application_budget()).await.unwrap();
    for hash in &hashes {
        let detail = &details[hash];
        assert_eq!(detail.statement.text(), Some("SELECT id FROM @rows"));
        assert!(detail.module_parameters.is_none());
        assert!(detail.object_name.is_none());
        assert!(detail.plan.text().is_none());
    }
    assert!(matches!(details[&hashes[2]].plan, Payload::Unavailable(PayloadUnavailable::NotCollected)));
    assert_eq!(reader.calls.len(), 2, "a local contradiction must not run the later attribute recheck");
}
