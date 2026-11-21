use super::*;
use crate::{
    resources::test_support::{FakeRead, replace},
    value::Val,
};
pub(crate) fn layout_row() -> Rec {
    Rec::of(&[
        ("edition", Val::Int(5)),
        ("database_edition", Val::Text("Hyperscale".into())),
        ("updateability", Val::Text("READ_WRITE".into())),
        ("database_id", Val::Int(5)),
        ("performance_visibility", Val::Int(1)),
        ("supported_columns", Val::Int(10)),
    ])
}
pub(crate) fn native_row() -> Rec {
    Rec::of(&[
        ("database_id", Val::Int(5)),
        ("replica_id", Val::Null),
        ("file_id", Val::Int(1)),
        ("component_id", Val::Text("00000001-2222-3333-4444-555555555555".into())),
        ("role", Val::Int(1)),
        ("role_desc", Val::Null),
        ("current_max_log_rate_bps", Val::Int(9_007_199_254_740_993)),
        ("catchup_rate_bps", Val::Int(0)),
        ("catchup_distance_bytes", Val::Int(i64::MAX)),
        ("last_reported_time_without_offset", Val::Null),
    ])
}
#[tokio::test]
async fn exact_supported_layout_requires_positive_same_read_native_profile_and_column_contract() {
    for (field, value) in [
        ("edition", Val::Int(3)),
        ("database_edition", Val::Null),
        ("database_edition", Val::Text("GeneralPurpose".into())),
        ("updateability", Val::Text("READ_ONLY".into())),
        ("updateability", Val::Null),
        ("performance_visibility", Val::Int(0)),
        ("performance_visibility", Val::Null),
        ("supported_columns", Val::Int(9)),
        ("database_id", Val::Int(0)),
    ] {
        let mut reader = FakeRead::new(vec![Ok(vec![replace(&layout_row(), &[(field, value)])])]);
        assert!(Layout::read(&mut reader).await.is_err(), "{field}");
        assert_eq!(reader.calls.len(), 1);
        assert!(!reader.calls[0].sql.contains("FROM sys.dm_hs_database_log_rate("));
    }
    let mut reader = FakeRead::new(vec![Ok(vec![layout_row()])]);
    assert_eq!(Layout::read(&mut reader).await.unwrap().database_id, 5);
}
#[test]
fn source_query_is_fixed_database_filtered_and_does_not_guess_documentation_aliases() {
    assert!(COMPONENTS.contains("TOP (@P1)"));
    assert!(COMPONENTS.contains("sys.dm_hs_database_log_rate(DB_ID())"));
    assert!(COMPONENTS.contains("WHERE database_id=DB_ID()"));
    assert!(!COMPONENTS.contains("(NULL)"));
    assert!(!COMPONENTS.contains("SELECT *"));
    assert!(LAYOUT.contains("c.system_type_id=48"));
    let budget = bounded_budget(&CollectionBudget::upper_limits());
    assert_eq!(budget.max_queries, SOURCE_READ_QUERY_COUNT + crate::read::ReadSession::SESSION_SETUP_QUERIES);
    assert_eq!(STANDALONE_QUERY_COUNT, 8);
    assert_eq!(budget.max_snapshot_rows, 1024);
    let strict =
        bounded_budget(&CollectionBudget { max_queries: 3, max_snapshot_rows: 1, ..CollectionBudget::default() });
    assert_eq!(strict.max_queries, 3);
    assert_eq!(strict.max_snapshot_rows, 1);
}
