use super::*;
use std::collections::BTreeMap;

fn definition() -> QueryDefinition {
    let native = |id: &str| NativeQueryIdentity {
        query_id: id.into(),
        qualifiers: BTreeMap::from([("epoch".into(), "initial".into())]),
    };
    let identity = native("3");
    let parent = native("1");
    QueryDefinition {
        id: digest(b"definition"),
        origin: StatementOrigin::Application,
        monitoring_template_id: None,
        identity: identity.clone(),
        variant_family: Some(crate::QueryVariantFamily {
            variant: identity,
            parent: parent.clone(),
            dispatcher: NativePlanIdentity { plan_id: "2".into(), query: parent, qualifiers: BTreeMap::new() },
        }),
        context: BTreeMap::new(),
        context_available: false,
        source_text_bytes: None,
        sql: DefinitionPayload::Unavailable { reason: PayloadUnavailable::Restricted },
    }
}

#[test]
fn family_metadata_remains_valid_when_the_native_sql_payload_is_restricted() {
    let original = definition();
    assert!(original.is_valid());
    let decoded: QueryDefinition = serde_json::from_value(serde_json::to_value(&original).unwrap()).unwrap();
    assert_eq!(decoded, original);
    assert_eq!(decoded.variant_family.as_ref().unwrap().variant, decoded.identity);
}

#[test]
fn another_child_or_recycled_child_lifetime_cannot_label_this_definition() {
    let mut different = definition();
    different.variant_family.as_mut().unwrap().variant.query_id = "4".into();
    assert!(different.variant_family.as_ref().unwrap().is_valid());
    assert!(!different.is_valid());
    let mut recycled = definition();
    recycled.variant_family.as_mut().unwrap().variant.qualifiers.insert("epoch".into(), "recycled".into());
    assert!(!recycled.is_valid());
}

#[test]
fn unknown_family_metadata_is_omitted_without_creating_a_parent_or_dispatcher() {
    let mut unknown = definition();
    unknown.variant_family = None;
    assert!(unknown.is_valid());
    assert!(serde_json::to_value(&unknown).unwrap().get("variant_family").is_none());
}
