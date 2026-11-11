use super::*;
fn decode(row: &Rec) -> Result<NativeColumn> {
    super::decode(row, MAX_CATALOG_EXPRESSION_BYTES)
}
use crate::{
    resources::test_support::{FakeRead, replace},
    value::Val,
};

pub(crate) fn row() -> Rec {
    Rec::of(&[
        ("database_id", Val::Int(5)),
        ("object_id", Val::Int(100)),
        ("created_without_offset", Val::Text("2026-11-10T07:00:00.123".into())),
        ("modified_without_offset", Val::Text("2026-11-10T07:01:00.123".into())),
        ("schema_name", Val::Text("dbo".into())),
        ("table_name", Val::Text("Child".into())),
        ("column_name", Val::Text("owner\u{1f}name".into())),
        ("column_id", Val::Int(3)),
        ("user_type_id", Val::Int(231)),
        ("system_type_id", Val::Int(231)),
        ("type_schema", Val::Text("sys".into())),
        ("type_name", Val::Text("nvarchar".into())),
        ("max_length", Val::Int(80)),
        ("precision", Val::Int(0)),
        ("scale", Val::Int(0)),
        ("is_nullable", Val::Int(0)),
        ("is_identity", Val::Int(0)),
        ("is_computed", Val::Int(0)),
        ("collation_name", Val::Text("Latin1_General_100_CI_AS".into())),
        ("default_object_id", Val::Int(101)),
        ("default_bytes", Val::Int(36)),
        ("default_value", Val::Text("(N'private-owner')".into())),
        ("computed_bytes", Val::Null),
        ("computed_value", Val::Null),
    ])
}

#[tokio::test]
async fn app_preserves_native_default_and_export_redacts_it_without_losing_its_version() {
    let mut reader = FakeRead::new(vec![Ok(vec![row()])]);
    let app = application(&mut reader).await.unwrap().pop().unwrap();
    assert_eq!(app.name, "owner\u{1f}name");
    assert_eq!(app.default.as_deref(), Some("(N'private-owner')"));
    assert_eq!(app.data_type, "nvarchar(40)");
    assert!(!app.nullable && !app.generated);
    let native = decode(&row()).unwrap().definition;
    let first =
        ObjectDefinitionRecord::new("columns".into(), ObjectDefinitionFact::MssqlColumn(Box::new(native))).unwrap();
    let serialized = serde_json::to_string(&first).unwrap();
    assert!(!serialized.contains("private-owner"));
    let changed =
        replace(&row(), &[("default_value", Val::Text("(N'another-owner')".into())), ("default_bytes", Val::Int(36))]);
    let second = ObjectDefinitionRecord::new(
        "columns".into(),
        ObjectDefinitionFact::MssqlColumn(Box::new(decode(&changed).unwrap().definition)),
    )
    .unwrap();
    assert_eq!(first.key, second.key);
    assert_ne!(first.version, second.version, "native literal changes are a different definition even after redaction");
}

#[test]
fn unknown_flags_or_malformed_native_metadata_are_not_defaulted() {
    for change in [
        ("is_nullable", Val::Null),
        ("is_identity", Val::Int(2)),
        ("column_id", Val::Int(0)),
        ("max_length", Val::Int(-2)),
        ("precision", Val::Int(256)),
        ("default_bytes", Val::Int(-1)),
        ("default_bytes", Val::Int(35)),
        ("computed_value", Val::Text("(1)".into())),
    ] {
        assert!(decode(&replace(&row(), &[change])).is_err());
    }
}

#[test]
fn unavailable_expression_is_distinct_from_absence_and_never_invents_an_app_default() {
    for change in [
        vec![("default_bytes", Val::Int(MAX_CATALOG_EXPRESSION_BYTES as i64 + 1)), ("default_value", Val::Null)],
        vec![("default_bytes", Val::Null), ("default_value", Val::Null)],
    ] {
        let native = decode(&replace(&row(), &change)).unwrap();
        assert!(!native.definition.default.as_ref().unwrap().payload.is_available());
        assert!(native.project().is_err());
    }
    let absent = decode(&replace(
        &row(),
        &[("default_object_id", Val::Int(0)), ("default_bytes", Val::Null), ("default_value", Val::Null)],
    ))
    .unwrap();
    assert!(absent.definition.default.is_none());
    assert!(absent.project().unwrap().default.is_none());
}

#[test]
fn native_byte_lengths_and_precision_drive_the_same_app_type_rendering() {
    for (name, length, precision, scale, expected) in [
        ("nvarchar", -1, 0, 0, "nvarchar(max)"),
        ("varchar", 20, 0, 0, "varchar(20)"),
        ("decimal", 9, 12, 2, "decimal(12,2)"),
        ("datetime2", 8, 27, 3, "datetime2(3)"),
    ] {
        let native = decode(&replace(
            &row(),
            &[
                ("type_name", Val::Text(name.into())),
                ("max_length", Val::Int(length)),
                ("precision", Val::Int(precision)),
                ("scale", Val::Int(scale)),
            ],
        ))
        .unwrap();
        assert_eq!(native.project().unwrap().data_type, expected);
    }
}
