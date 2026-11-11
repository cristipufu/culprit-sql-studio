use super::*;
use crate::{resources::test_support::replace, value::Val};
pub(crate) fn row() -> Rec {
    let definition = "([amount]>=(42))";
    Rec::of(&[
        ("database_id", Val::Int(5)),
        ("object_id", Val::Int(102)),
        ("constraint_schema", Val::Text("dbo".into())),
        ("name", Val::Text("CK_child_amount".into())),
        ("created_without_offset", Val::Text("2026-11-10T07:01:00.123".into())),
        ("modified_without_offset", Val::Text("2026-11-10T07:02:00.123".into())),
        ("parent_id", Val::Int(100)),
        ("parent_schema", Val::Text("dbo".into())),
        ("parent_table", Val::Text("Child".into())),
        ("parent_created_without_offset", Val::Text("2026-11-10T07:00:00.123".into())),
        ("parent_column_id", Val::Int(0)),
        ("is_disabled", Val::Int(1)),
        ("is_not_trusted", Val::Int(1)),
        ("is_not_for_replication", Val::Int(0)),
        ("is_system_named", Val::Int(0)),
        ("uses_database_collation", Val::Int(0)),
        ("definition_bytes", Val::Int(definition.encode_utf16().count() as i64 * 2)),
        ("definition_value", Val::Text(definition.into())),
    ])
}
#[test]
fn native_flags_and_literal_versions_survive_redaction_without_turning_unknown_bodies_into_absence() {
    let native = decode(&row(), MAX_CATALOG_EXPRESSION_BYTES).unwrap();
    assert!(native.disabled && !native.trusted && !native.not_for_replication);
    let first = ObjectDefinitionRecord::new(
        crate::sources::CHECK_CONSTRAINTS.name.into(),
        ObjectDefinitionFact::MssqlCheckConstraint(Box::new(native)),
    )
    .unwrap();
    let DefinitionPayload::Available { text, .. } = first.fact.definition_payloads().next().unwrap().payload else {
        panic!("bounded definition")
    };
    assert!(!text.contains("42"));
    let changed = replace(&row(), &[("definition_value", Val::Text("([amount]>=(43))".into()))]);
    let second = ObjectDefinitionRecord::new(
        crate::sources::CHECK_CONSTRAINTS.name.into(),
        ObjectDefinitionFact::MssqlCheckConstraint(Box::new(decode(&changed, MAX_CATALOG_EXPRESSION_BYTES).unwrap())),
    )
    .unwrap();
    assert_eq!(first.key, second.key);
    assert_ne!(first.version, second.version);
    let hidden = decode(
        &replace(&row(), &[("definition_bytes", Val::Null), ("definition_value", Val::Null)]),
        MAX_CATALOG_EXPRESSION_BYTES,
    )
    .unwrap();
    assert!(matches!(
        hidden.definition.payload,
        DefinitionPayload::Unavailable { reason: PayloadUnavailable::Restricted }
    ));
    let oversized =
        decode(&replace(&row(), &[("definition_bytes", Val::Int(2048)), ("definition_value", Val::Null)]), 1024)
            .unwrap();
    assert!(matches!(
        oversized.definition.payload,
        DefinitionPayload::Unavailable { reason: PayloadUnavailable::Oversized }
    ));
}
#[test]
fn missing_or_malformed_native_flags_never_become_an_enabled_trusted_constraint() {
    for change in [
        ("is_disabled", Val::Null),
        ("is_not_trusted", Val::Int(2)),
        ("parent_column_id", Val::Int(-1)),
        ("definition_bytes", Val::Int(1)),
    ] {
        assert!(decode(&replace(&row(), &[change]), MAX_CATALOG_EXPRESSION_BYTES).is_err());
    }
}
