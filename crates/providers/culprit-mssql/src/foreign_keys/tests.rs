use super::*;
use crate::{
    resources::test_support::{FakeRead, replace},
    value::Val,
};
pub(crate) fn row() -> Rec {
    Rec::of(&[
        ("database_id", Val::Int(5)), ("object_id", Val::Int(101)), ("name", Val::Text("FK_child_parent".into())),
        ("constraint_schema",Val::Text("archive".into())),
        ("created_without_offset",Val::Text("2026-11-10T07:01:00.123".into())),
        ("modified_without_offset",Val::Text("2026-11-10T07:02:00.123".into())),
        ("parent_id",Val::Int(100)), ("parent_schema",Val::Text("dbo".into())), ("parent_table",Val::Text("Child".into())),
        ("parent_created_without_offset",Val::Text("2026-11-10T07:00:00.123".into())),
        ("referenced_id",Val::Int(99)), ("referenced_schema",Val::Text("dbo".into())), ("referenced_table",Val::Text("Parent".into())),
        ("referenced_created_without_offset",Val::Text("2026-11-10T06:59:00.123".into())),
        ("delete_action",Val::Int(1)), ("update_action",Val::Int(0)),
        ("is_disabled",Val::Int(1)), ("is_not_trusted",Val::Int(1)),
        ("is_not_for_replication",Val::Int(0)), ("is_system_named",Val::Int(0)),
        ("column_pairs",Val::Text(r#"[{"ordinal":1,"parent_column_id":2,"parent_column":"parent\u001fkey","referenced_column_id":1,"referenced_column":"first"},{"ordinal":2,"parent_column_id":1,"parent_column":"second","referenced_column_id":2,"referenced_column":"other"}]"#.into())),
    ])
}
#[tokio::test]
async fn native_column_pairs_preserve_names_and_order_through_app_and_export_projections() {
    let native = decode(&row()).unwrap();
    assert_eq!(native.columns[0].parent_column, "parent\u{1f}key");
    assert_eq!(native.columns[0].parent_column_id, 2);
    assert_eq!(native.columns[1].parent_column_id, 1);
    assert!(native.disabled && !native.trusted);
    assert_eq!(native.schema, "archive");
    let definition =
        ObjectDefinitionRecord::new("foreign_keys".into(), ObjectDefinitionFact::MssqlForeignKey(Box::new(native)))
            .unwrap();
    assert!(definition.is_valid());
    let mut reader = FakeRead::new(vec![Ok(vec![row()])]);
    let app = application(&mut reader).await.unwrap().pop().unwrap();
    assert_eq!(app.schema, "dbo");
    assert_eq!(app.columns, vec!["parent\u{1f}key", "second"]);
    assert_eq!(app.ref_columns, vec!["first", "other"]);
    assert_eq!(app.delete_rule.as_deref(), Some("CASCADE"));
    assert_eq!(reader.calls.len(), 1);
    assert!(reader.calls[0].sql.contains("FOR JSON PATH"));
}
#[test]
fn unknown_actions_missing_flags_and_invalid_column_membership_never_become_defaults() {
    for change in [
        ("delete_action", Val::Int(99)),
        ("is_disabled", Val::Null),
        ("column_pairs", Val::Text("[]".into())),
        ("column_pairs", Val::Text("null".into())),
        ("created_without_offset", Val::Text("2026-11-10T07:00:00Z".into())),
    ] {
        assert!(decode(&replace(&row(), &[change])).is_err());
    }
    let mut native = decode(&row()).unwrap();
    native.columns[1].parent_column_id = native.columns[0].parent_column_id;
    assert!(!native.is_valid());
    native.columns[1].parent_column_id = 1;
    native.columns[1].ordinal = 1;
    assert!(!native.is_valid());
}
#[test]
fn definitions_keep_object_identity_while_versions_follow_native_properties() {
    let native = decode(&row()).unwrap();
    let first = ObjectDefinitionRecord::new(
        "foreign_keys".into(),
        ObjectDefinitionFact::MssqlForeignKey(Box::new(native.clone())),
    )
    .unwrap();
    let mut changed = native;
    changed.disabled = false;
    let second =
        ObjectDefinitionRecord::new("foreign_keys".into(), ObjectDefinitionFact::MssqlForeignKey(Box::new(changed)))
            .unwrap();
    assert_eq!(first.key, second.key);
    assert_ne!(first.version, second.version);
    let mut corrupt = first;
    corrupt.version = second.version;
    assert!(!corrupt.is_valid());
}

#[test]
fn definition_replay_keeps_ids_but_conflicting_versions_cannot_be_two_objects_in_one_capture() {
    let source = CollectionDescription {
        target: culprit_core::DatabaseIdentity::server(
            culprit_core::Engine::Mssql,
            "db",
            "fixture.invalid",
            1433,
            None,
        )
        .into(),
        source: "foreign-key-state".into(),
        continuity: SourceContinuity::Unverified,
        observation: SourceObservation::Snapshot(StateSnapshotDescription {
            dataset: SnapshotDataset::ForeignKeys,
            observed_from: "2026-11-10T08:00:00Z".into(),
            observed_until: "2026-11-10T08:00:01Z".into(),
            records: 1,
            sources: vec![StateSourceCoverage::collected("foreign_keys".into(), 1)],
        }),
    };
    let fact = decode(&row()).unwrap();
    let definition = ObjectDefinitionRecord::new(
        "foreign_keys".into(),
        ObjectDefinitionFact::MssqlForeignKey(Box::new(fact.clone())),
    )
    .unwrap();
    let record = TelemetryRecord::ObjectDefinition(definition);
    let first = ExportedRecord::new(
        &source,
        &CaptureIdentity { observation_id: "first".into(), revision: None },
        record.clone(),
    )
    .unwrap();
    let replay = ExportedRecord::new(
        &source,
        &CaptureIdentity { observation_id: "second".into(), revision: None },
        record.clone(),
    )
    .unwrap();
    assert_eq!(first.record_id, replay.record_id);
    let mut changed = fact;
    changed.disabled = false;
    let version = TelemetryRecord::ObjectDefinition(
        ObjectDefinitionRecord::new("foreign_keys".into(), ObjectDefinitionFact::MssqlForeignKey(Box::new(changed)))
            .unwrap(),
    );
    assert_eq!(
        record.native_identity(),
        version.native_identity(),
        "one object has one current definition per observation"
    );
    let changed =
        ExportedRecord::new(&source, &CaptureIdentity { observation_id: "second".into(), revision: None }, version)
            .unwrap();
    assert_ne!(first.record_id, changed.record_id);
}
