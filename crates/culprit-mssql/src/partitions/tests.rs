use super::*;
use crate::{
    resources::test_support::{FakeRead, replace},
    value::Val,
};
pub(crate) fn row() -> Rec {
    let display = "N'2026-11-10T00:00:00.1234567'";
    Rec::of(&[
        ("database_id", Val::Int(5)),
        ("object_id", Val::Int(100)),
        ("schema_name", Val::Text("dbo".into())),
        ("table_name", Val::Text("events".into())),
        ("created_without_offset", Val::Text("2026-11-01T08:00:00.003".into())),
        ("index_id", Val::Int(0)),
        ("index_name", Val::Null),
        ("index_disabled", Val::Int(0)),
        ("index_hypothetical", Val::Int(0)),
        ("scheme_id", Val::Int(65_537)),
        ("scheme_name", Val::Text("ps_events".into())),
        ("function_id", Val::Int(65_536)),
        ("function_name", Val::Text("pf_events".into())),
        ("function_created_without_offset", Val::Text("2026-11-01T07:00:00.003".into())),
        ("function_modified_without_offset", Val::Text("2026-11-01T09:00:00.007".into())),
        ("fanout", Val::Int(2)),
        ("boundary_value_on_right", Val::Int(1)),
        ("parameter_id", Val::Int(1)),
        ("parameter_type_schema", Val::Text("sys".into())),
        ("parameter_type", Val::Text("datetime2".into())),
        ("system_type_id", Val::Int(42)),
        ("user_type_id", Val::Int(42)),
        ("max_length", Val::Int(8)),
        ("precision", Val::Int(27)),
        ("scale", Val::Int(7)),
        ("collation_name", Val::Null),
        ("boundary_id", Val::Int(1)),
        ("value_is_null", Val::Int(0)),
        ("native_value_type", Val::Text("datetime2".into())),
        ("native_value_sha256", Val::Text("A".repeat(64))),
        ("boundary_bytes", Val::Int(display.encode_utf16().count() as i64 * 2)),
        ("boundary_value", Val::Text(display.into())),
    ])
}
#[test]
fn native_layout_direction_fingerprints_and_dates_survive_export_but_tenant_values_do_not() {
    let native = decode(&row(), MAX_CATALOG_EXPRESSION_BYTES).unwrap();
    assert!(native.definition.boundary_on_right);
    assert_eq!(native.definition.native_value_sha256, Some("a".repeat(64)));
    let record = ObjectDefinitionRecord::new(
        crate::sources::PARTITIONS.name.into(),
        ObjectDefinitionFact::MssqlPartitionBoundary(Box::new(native.definition.clone())),
    )
    .unwrap();
    let mut changed = native.definition;
    changed.native_value_sha256 = Some("b".repeat(64));
    let revised = ObjectDefinitionRecord::new(
        record.source.clone(),
        ObjectDefinitionFact::MssqlPartitionBoundary(Box::new(changed)),
    )
    .unwrap();
    assert_eq!(record.key, revised.key);
    assert_ne!(record.version, revised.version);
    let DefinitionPayload::Available { text, .. } = record.fact.definition_payloads().next().unwrap().payload else {
        panic!("date bound")
    };
    assert!(text.contains("2026-11-10T00:00:00.1234567"));
    let raw = "N'tenant-secret'";
    let private = decode(
        &replace(
            &row(),
            &[("boundary_bytes", Val::Int(raw.len() as i64 * 2)), ("boundary_value", Val::Text(raw.into()))],
        ),
        MAX_CATALOG_EXPRESSION_BYTES,
    )
    .unwrap();
    assert_eq!(private.raw_display_value.as_deref(), Some(raw));
    assert!(!serde_json::to_string(&private.definition).unwrap().contains("tenant-secret"));
    assert!(application_bound(&private).unwrap().contains("tenant-secret"));
}
#[test]
fn native_null_is_a_boundary_and_does_not_mean_a_function_has_no_boundaries() {
    let null = decode(
        &replace(
            &row(),
            &[
                ("value_is_null", Val::Int(1)),
                ("native_value_type", Val::Null),
                ("native_value_sha256", Val::Null),
                ("boundary_bytes", Val::Int(8)),
                ("boundary_value", Val::Text("NULL".into())),
            ],
        ),
        MAX_CATALOG_EXPRESSION_BYTES,
    )
    .unwrap();
    assert_eq!(null.definition.boundary_id, Some(1));
    assert_eq!(null.definition.value_is_null, Some(true));
    let empty = decode(
        &replace(
            &row(),
            &[
                ("fanout", Val::Int(1)),
                ("boundary_id", Val::Null),
                ("value_is_null", Val::Null),
                ("native_value_type", Val::Null),
                ("native_value_sha256", Val::Null),
                ("boundary_bytes", Val::Null),
                ("boundary_value", Val::Null),
            ],
        ),
        MAX_CATALOG_EXPRESSION_BYTES,
    )
    .unwrap();
    assert!(empty.definition.boundary_id.is_none());
    assert!(empty.definition.display_value.is_none());
    assert!(application_bound(&empty).is_none());
    let left =
        decode(&replace(&row(), &[("boundary_value_on_right", Val::Int(0))]), MAX_CATALOG_EXPRESSION_BYTES).unwrap();
    assert!(application_bound(&left).unwrap().contains("LESS THAN OR EQUAL"));
}
#[tokio::test]
async fn missing_or_oversized_values_remain_explicit_and_native_errors_never_retry() {
    let oversized =
        decode(&replace(&row(), &[("boundary_bytes", Val::Int(100)), ("boundary_value", Val::Null)]), 50).unwrap();
    assert!(matches!(
        oversized.definition.display_value.unwrap().payload,
        DefinitionPayload::Unavailable { reason: PayloadUnavailable::Oversized }
    ));
    let mut reader = FakeRead::new(vec![Err(ReadError::Stopped(CollectionStop::TimedOut))]);
    assert!(application(&mut reader).await.is_err());
    assert_eq!(reader.calls.len(), 1);
    assert!(select_sql(Selection::AllIndexes, 1024).contains("LEFT JOIN sys.partition_range_values"));
    assert!(!select_sql(Selection::AllIndexes, 1024).contains("index_id IN(0,1)"));
    assert!(select_sql(Selection::BaseStorage, 1024).contains("MAXDOP 1"));
    assert!(decode(&replace(&row(), &[("boundary_id", Val::Int(2))]), MAX_CATALOG_EXPRESSION_BYTES).is_err());
}
