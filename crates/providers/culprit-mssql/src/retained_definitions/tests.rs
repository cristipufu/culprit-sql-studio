use super::*;
use crate::{
    read::ReadError,
    resources::test_support::{FakeRead, replace},
    value::Val,
};
pub(crate) fn feature(visible: i64, present: i64) -> Rec {
    Rec::of(&[
        ("metadata_visibility", Val::Int(visible)),
        ("last_actual", Val::Int(present)),
        ("live_xml", Val::Int(1)),
        ("profiles", Val::Int(1)),
    ])
}
#[tokio::test]
async fn unsupported_hidden_and_failed_feature_reads_cannot_masquerade_as_an_empty_plan_cache() {
    let mut absent = FakeRead::new(vec![Ok(vec![feature(1, 0)])]);
    assert!(layout(&mut absent, &CollectionBudget::default()).await.unwrap().is_none());
    assert_eq!(absent.calls.len(), 1);
    let mut hidden = FakeRead::new(vec![Ok(vec![feature(0, 0)])]);
    assert!(layout(&mut hidden, &CollectionBudget::default()).await.is_err());
    assert_eq!(hidden.calls.len(), 1);
    let mut failed = FakeRead::new(vec![Ok(vec![feature(1, 1)]), Err(ReadError::Stopped(CollectionStop::TimedOut))]);
    assert!(layout(&mut failed, &CollectionBudget::default()).await.is_err());
    assert_eq!(failed.calls.len(), 2);
}
#[test]
fn retained_exports_share_native_decoding_and_do_not_claim_a_batch_execution_count_or_utc_clock() {
    let layout = Layout { maximum_bytes: 4096, database_setting: Some(false) };
    let row =
        replace(&crate::cached_definitions::tests::row(), &[("execution_count", Val::Int(9_007_199_254_740_993))]);
    let native = decode(&row, &layout).unwrap();
    assert!(native.batch_plan.payload.is_available());
    assert_eq!(native.database_last_query_plan_stats, Some(false));
    assert_eq!(native.statement_executions.as_deref(), Some("9007199254740993"));
    assert_eq!(native.last_start_without_offset.as_deref(), Some("2020-01-01T08:00:00.0030000"));
    assert!(!serde_json::to_string(&native).unwrap().contains("private-value"));
    let unsupported = decode(&replace(&row, &[("plan_handle", Val::Bytes(vec![0; 64]))]), &layout).unwrap();
    assert!(unsupported.statement_executions.is_none());
    assert!(select_sql(&layout).contains(cached_statements::DATABASE_PREDICATE));
}
