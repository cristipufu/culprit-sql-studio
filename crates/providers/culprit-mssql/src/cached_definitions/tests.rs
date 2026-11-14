use super::*;
use crate::{resources::test_support::replace, value::Val};
pub(crate) const XML: &str = "<ShowPlanXML><BatchSequence><Batch><Statements><StmtSimple StatementText=\"SELECT 'private-value'\" /></Statements></Batch></BatchSequence></ShowPlanXML>";
pub(crate) fn row() -> Rec {
    let sql = "SELECT 'private-value' AS canary";
    let mut fields =
        crate::grants::tests::row().columns().map(|(name, value)| (name.to_owned(), value.clone())).collect::<Vec<_>>();
    fields.extend([
        ("statement_bytes".into(), Val::Int(sql.encode_utf16().count() as i64 * 2)),
        ("statement_text".into(), Val::Text(sql.into())),
        ("plan_bytes".into(), Val::Int(XML.encode_utf16().count() as i64 * 2)),
        ("query_plan".into(), Val::Text(XML.into())),
        ("encrypted".into(), Val::Bool(false)),
        ("module_database_id".into(), Val::Null),
        ("module_object_id".into(), Val::Null),
        ("module_number".into(), Val::Null),
        ("query_plan_hash".into(), Val::Text("0x0000000000000009".into())),
        ("last_start_without_offset".into(), Val::Text("2020-01-01T08:00:00.003".into())),
    ]);
    Rec::of(&fields.iter().map(|(name, value)| (name.as_str(), value.clone())).collect::<Vec<_>>())
}
#[test]
fn cache_definitions_retain_native_identity_and_literal_changes_without_exporting_raw_literals() {
    let limits = Limits::from_budget(&CollectionBudget::default()).unwrap();
    let definition = decode(&row(), &limits).unwrap();
    let encoded = serde_json::to_string(&definition).unwrap();
    assert!(!encoded.contains("private-value"));
    assert!(definition.statement.native_sha256.is_some());
    assert!(definition.estimated_plan.payload.is_available());
    let first = ObjectDefinitionRecord::new(
        crate::sources::PLAN_CACHE.name.into(),
        ObjectDefinitionFact::MssqlCachedDefinition(Box::new(definition)),
    )
    .unwrap();
    let sql = "SELECT 'another-value' AS canary";
    let changed = replace(
        &row(),
        &[("statement_bytes", Val::Int(sql.len() as i64 * 2)), ("statement_text", Val::Text(sql.into()))],
    );
    let second = ObjectDefinitionRecord::new(
        first.source.clone(),
        ObjectDefinitionFact::MssqlCachedDefinition(Box::new(decode(&changed, &limits).unwrap())),
    )
    .unwrap();
    assert_eq!(first.key, second.key);
    assert_ne!(first.version, second.version);
}
#[test]
fn a_single_record_fits_the_page_policy_and_unavailable_bodies_are_not_truncated() {
    let budget = CollectionBudget::default();
    let limits = Limits::from_budget(&budget).unwrap();
    assert!(
        (limits.sql + limits.plan) * JSON_BYTES_PER_UTF16_BYTE + RECORD_METADATA_BYTES <= budget.page_bytes as usize
    );
    let oversized = decode(
        &replace(&row(), &[("statement_bytes", Val::Int((limits.sql + 1) as i64)), ("statement_text", Val::Null)]),
        &limits,
    )
    .unwrap();
    assert!(matches!(
        oversized.statement.payload,
        DefinitionPayload::Unavailable { reason: PayloadUnavailable::Oversized }
    ));
    assert!(decode(&replace(&row(), &[("statement_bytes", Val::Int(2))]), &limits).is_err());
    assert!(Limits::from_budget(&CollectionBudget { page_bytes: 1024, ..budget }).is_err());
    assert!(select_sql(&limits).contains(cached_statements::DATABASE_PREDICATE));
}
