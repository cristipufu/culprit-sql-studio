use super::*;
use crate::{
    resources::test_support::{FakeRead, replace},
    value::Val,
};
pub(crate) fn row() -> Rec {
    Rec::of(&[
        ("database_id", Val::Int(5)),
        ("object_id", Val::Int(100)),
        ("schema_name", Val::Text("dbo".into())),
        ("name", Val::Text("orders".into())),
        ("native_type", Val::Text("U".into())),
        ("created_without_offset", Val::Text("2020-01-01T08:00:00.003".into())),
        ("modified_without_offset", Val::Text("2020-01-01T09:00:00.007".into())),
        ("parent_object_id", Val::Int(0)),
        ("parent_schema", Val::Null),
        ("parent_name", Val::Null),
        ("created", Val::Text("2020-01-01T06:00:00Z".into())),
        ("modified", Val::Text("2020-01-01T07:00:00Z".into())),
    ])
}
#[test]
fn catalog_dates_remain_native_local_facts_with_object_generation() {
    let object = decode(&row()).unwrap();
    assert_eq!(object.identity.created_without_offset, "2020-01-01T08:00:00.0030000");
    assert_eq!(object.modified_without_offset, "2020-01-01T09:00:00.0070000");
    assert!(object.parent_object_id.is_none());
    for (column, value) in [
        ("object_id", Val::Int(0)),
        ("native_type", Val::Text("unknown".into())),
        ("created_without_offset", Val::Text("2020-01-01T08:00:00Z".into())),
    ] {
        assert!(decode(&replace(&row(), &[(column, value)])).is_err());
    }
}
#[tokio::test]
async fn interactive_changes_share_decoding_and_keep_since_values_out_of_sql() {
    let mut reader = FakeRead::new(vec![Ok(vec![row()])]);
    let events = changes(&mut reader, "2020-01-01T06:30:00Z").await.unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].title, title("U", EventAction::Changed, "orders", "dbo.orders", None).render());
    assert!(!reader.calls[0].sql.contains("2020-01-01"));
    assert_eq!(reader.calls[0].limit, 301);
    assert!(select_sql(false).contains("created_without_offset"));
    assert!(!select_sql(false).contains("GETUTCDATE"));
    let mut reader = FakeRead::new(vec![]);
    assert!(changes(&mut reader, "invalid").await.unwrap().is_empty());
    assert!(reader.calls.is_empty());
}
