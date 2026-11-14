use super::*;
use crate::{resources::test_support::FakeRead, value::Val};

pub(crate) fn row(id: i64, role: i64, name: Option<&str>) -> Rec {
    Rec::of(&[
        ("replica_group_id", Val::Int(id)),
        ("role_type", Val::Int(role)),
        ("replica_name", name.map_or(Val::Null, |name| Val::Text(name.into()))),
    ])
}

#[test]
fn native_role_history_and_unknown_roles_do_not_become_a_current_member_selector() {
    let catalog =
        Catalog::decode(&[row(3, 2, Some("native-member")), row(3, 1, Some("native-member")), row(7, 42, None)])
            .unwrap();
    assert_eq!(catalog.ids(), ["3", "7"]);
    let catalog = catalog.population_catalog().unwrap();
    let metadata: serde_json::Value =
        serde_json::from_str(&catalog.populations[0].native_properties["role_history"]).unwrap();
    assert_eq!(metadata[0]["role_type"], 1);
    assert_eq!(metadata[1]["role_type"], 2);
    let unknown: serde_json::Value =
        serde_json::from_str(&catalog.populations[1].native_properties["role_history"]).unwrap();
    assert_eq!(unknown[0]["role_type"], 42);
    assert!(unknown[0]["replica_name"].is_null());
    assert_eq!(metadata[0]["replica_name"], "native-member");
}

#[test]
fn catalog_order_is_stable_and_cannot_hide_duplicate_or_incomplete_native_metadata() {
    let first = row(12, 1, None);
    let second = row(3, 5, Some("named-reader"));
    let forward = Catalog::decode(&[first.clone(), second.clone()]).unwrap();
    let reverse = Catalog::decode(&[second, first.clone()]).unwrap();
    assert_eq!(forward, reverse);
    assert_eq!(forward.population_catalog().unwrap(), reverse.population_catalog().unwrap());
    for rows in [
        vec![],
        vec![first.clone(), first],
        vec![row(-1, 1, None)],
        vec![row(1, -1, None)],
        vec![row(1, 256, None)],
        vec![Rec::of(&[("replica_group_id", Val::Int(1))])],
        vec![row(1, 1, Some(&"x".repeat(MAX_REPLICA_NAME_BYTES + 1)))],
        (0..=MAX_REPLICA_GROUP_ROWS as i64).map(|id| row(id, 1, None)).collect(),
    ] {
        assert!(Catalog::decode(&rows).is_err());
    }
}

#[tokio::test]
async fn catalog_uses_one_bounded_native_read_and_preserves_nullable_names() {
    let mut reader = FakeRead::new(vec![Ok(vec![row(1, 1, None), row(5, 5, Some("named-reader"))])]);
    let catalog = read(&mut reader).await.unwrap();
    assert_eq!(catalog.ids(), ["1", "5"]);
    assert_eq!(reader.calls.len(), 1);
    assert_eq!(reader.calls[0].limit, MAX_REPLICA_GROUP_ROWS + 1);
    assert!(reader.calls[0].sql.contains("role_type,replica_name"));
    assert!(reader.calls[0].sql.contains("/* culprit */"));
    assert!(reader.calls[0].parameters.is_empty());
}

#[test]
fn selection_never_pools_groups_or_guesses_from_role_history_and_unknown_roles() {
    let catalog = Catalog::decode(&[row(3, 2, Some("native-member")), row(7, 42, None)]).unwrap();
    assert_eq!(catalog.select(Some(&NativeIdentity::new("3"))).unwrap(), Population::ReplicaGroup(3));
    assert_eq!(catalog.select(Some(&NativeIdentity::new("7"))).unwrap(), Population::ReplicaGroup(7));
    assert!(catalog.select(None).is_err());
    assert!(catalog.select(Some(&NativeIdentity::new("1"))).is_err());
    for id in ["", "-1", "03", "+3", "3;SELECT 1", "9223372036854775808"] {
        assert!(catalog.select(Some(&NativeIdentity::new(id))).is_err(), "{id}");
    }
    assert!(catalog.select(Some(&NativeIdentity::new("3").qualified("foreign-scope", "unknown"))).is_err());
    let historical = Catalog::decode(&[row(3, 1, Some("native-member")), row(3, 2, Some("native-member"))]).unwrap();
    assert_eq!(historical.select(None).unwrap(), Population::ReplicaGroup(3));
    assert_eq!(historical.ids(), ["3"], "one group may retain multiple historical role facts");
    assert_eq!(Population::ReplicaGroup(7).predicate("rs"), "rs.replica_group_id=7");
    assert_eq!(Population::ReplicaGroup(7).predicate("ws"), "ws.replica_group_id=7");
    assert_eq!(Population::Unpartitioned.predicate("rs"), "1=1");
}
