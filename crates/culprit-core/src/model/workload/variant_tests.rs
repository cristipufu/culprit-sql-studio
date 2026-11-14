use super::*;
use std::collections::BTreeMap;

fn family() -> QueryVariantFamily {
    let query = |id: &str| NativeQueryIdentity {
        query_id: id.into(),
        qualifiers: BTreeMap::from([("initial_compile_at".into(), "2020-01-01T06:00:00Z".into())]),
    };
    let parent = query("1");
    QueryVariantFamily {
        variant: query("3"),
        parent: parent.clone(),
        dispatcher: NativePlanIdentity { plan_id: "2".into(), query: parent, qualifiers: BTreeMap::new() },
    }
}

#[test]
fn variant_relationship_requires_distinct_child_and_the_same_native_parent_lifetime() {
    let original = family();
    assert!(original.is_valid());
    let mut changed = original.clone();
    changed.parent = changed.variant.clone();
    changed.dispatcher.query = changed.parent.clone();
    assert!(!changed.is_valid(), "a query cannot be its own child");
    let mut recycled = original.clone();
    recycled.dispatcher.query.qualifiers.insert("initial_compile_at".into(), "2020-01-02T06:00:00Z".into());
    assert!(!recycled.is_valid(), "equal numeric IDs do not establish the same native lifetime");
    let mut missing = original;
    missing.dispatcher.plan_id.clear();
    assert!(!missing.is_valid());
}
