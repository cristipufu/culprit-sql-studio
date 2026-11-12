use super::*;
use crate::{
    resources::test_support::{FakeRead, replace},
    value::Val,
};
fn row(hash: &str) -> Rec {
    let sql = "SELECT 1";
    Rec::of(&[
        ("query_hash", Val::Text(hash.into())),
        ("payload_included", Val::Bool(true)),
        ("statement_bytes", Val::Int((sql.len() * 2) as i64)),
        ("statement_text", Val::Text(sql.into())),
        ("plan_bytes", Val::Int(2)),
        ("query_plan", Val::Text("x".into())),
        ("object_name", Val::Null),
        ("encrypted", Val::Int(0)),
    ])
}
#[tokio::test]
async fn only_requested_plans_are_read_and_hashes_never_change_native_sql_templates() {
    let hashes: Vec<String> = vec!["0x0123456789ABCDEF".into(), "0x0123456789ABCDE0".into()];
    let mut reader = FakeRead::new(vec![Ok(vec![row(&hashes[0])]), Ok(vec![row(&hashes[1])])]);
    let budget = crate::read::ReadSession::application_budget();
    let values = read(&mut reader, &hashes, 1, &budget).await.unwrap();
    assert!(values[&hashes[0]].plan.text().is_some());
    assert!(matches!(values[&hashes[1]].plan, Payload::Unavailable(PayloadUnavailable::NotCollected)));
    assert_eq!(reader.calls.len(), 2);
    assert!(reader.calls[0].sql.contains("sys.dm_exec_text_query_plan"));
    assert!(!reader.calls[1].sql.contains("sys.dm_exec_text_query_plan"));
    for (call, hash) in reader.calls.iter().zip(&hashes) {
        assert!(!call.sql.contains(hash));
        assert!(call.sql.contains(DATABASE_PREDICATE));
        assert!(call.sql.contains("ORDER BY qs.last_execution_time DESC"));
        let Parameter::Text(xml) = &call.parameters[0] else { panic!("bound selectors") };
        assert!(xml.contains(hash));
    }
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
    let mut reader = FakeRead::new(vec![Ok(hashes.iter().map(|hash| row(hash)).collect())]);
    let result = read(&mut reader, &hashes, 25, &crate::read::ReadSession::application_budget()).await.unwrap();
    assert_eq!(result.len(), 25);
    assert_eq!(reader.calls.len(), 1, "small payloads must not force one DMV scan per requested plan");
}
#[tokio::test]
async fn payload_deferral_reselects_only_the_remaining_members_and_must_make_progress() {
    let hashes = vec!["0x0123456789ABCDEF".to_string(), "0x0123456789ABCDE0".to_string()];
    let deferred = replace(
        &row(&hashes[1]),
        &[("payload_included", Val::Bool(false)), ("statement_text", Val::Null), ("query_plan", Val::Null)],
    );
    let mut reader = FakeRead::new(vec![Ok(vec![row(&hashes[0]), deferred.clone()]), Ok(vec![row(&hashes[1])])]);
    let result = read(&mut reader, &hashes, 2, &crate::read::ReadSession::application_budget()).await.unwrap();
    assert_eq!(result.len(), 2);
    assert_eq!(reader.calls.len(), 2);
    let Parameter::Text(second) = &reader.calls[1].parameters[0] else { panic!("bound selectors") };
    assert!(!second.contains(&hashes[0]));
    let mut reader = FakeRead::new(vec![Ok(vec![deferred])]);
    let error =
        read(&mut reader, &hashes[1..], 1, &crate::read::ReadSession::application_budget()).await.err().unwrap();
    assert_eq!(CollectionStop::from_error(&error), CollectionStop::ByteLimit);
    assert_eq!(reader.calls.len(), 1);
}
