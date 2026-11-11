use super::*;
use crate::{
    resources::test_support::{FakeRead, replace},
    value::Val,
};
pub(crate) fn row() -> Rec {
    Rec::of(&[
        ("database_id", Val::Int(5)),
        ("object_id", Val::Int(200)),
        ("parent_id", Val::Int(100)),
        ("created_without_offset", Val::Text("2026-11-10T09:00:00.003".into())),
        ("modified_without_offset", Val::Text("2026-11-10T10:00:00.007".into())),
        ("parent_created_without_offset", Val::Text("2026-11-10T08:00:00.003".into())),
        ("schema_name", Val::Text("dbo".into())),
        ("name", Val::Text("tenant:audit".into())),
        ("parent_schema", Val::Text("dbo".into())),
        ("parent_name", Val::Text("items".into())),
        ("parent_type", Val::Text("U".into())),
        ("native_type", Val::Text("TR".into())),
        ("is_disabled", Val::Int(0)),
        ("is_instead_of_trigger", Val::Int(0)),
        ("is_not_for_replication", Val::Int(1)),
        ("insert_event", Val::Int(1)),
        ("delete_event", Val::Int(0)),
        ("update_event", Val::Int(1)),
    ])
}
#[test]
fn native_trigger_identity_parent_and_changes_are_independent_of_display_names() {
    let native = decode(&row()).unwrap();
    assert_eq!(native.name, "tenant:audit");
    assert_eq!(event_codes(&native), "IU");
    assert!(native.not_for_replication && !native.disabled && native.event_metadata_complete());
    let record = ObjectDefinitionRecord::new(
        crate::sources::TRIGGERS.name.into(),
        ObjectDefinitionFact::MssqlDmlTrigger(Box::new(native.clone())),
    )
    .unwrap();
    let mut disabled = native;
    disabled.disabled = true;
    let changed =
        ObjectDefinitionRecord::new(record.source.clone(), ObjectDefinitionFact::MssqlDmlTrigger(Box::new(disabled)))
            .unwrap();
    assert_eq!(record.key, changed.key);
    assert_ne!(record.version, changed.version);
    for (column, value) in
        [("parent_id", Val::Int(200)), ("native_type", Val::Text("unknown".into())), ("is_disabled", Val::Null)]
    {
        assert!(decode(&replace(&row(), &[(column, value)])).is_err());
    }
}
#[test]
fn hidden_event_properties_remain_unknown_and_do_not_become_disabled_or_empty_triggers() {
    let hidden = decode(&replace(&row(), &[("insert_event", Val::Null)])).unwrap();
    assert_eq!(hidden.insert_event, None);
    assert!(!hidden.event_metadata_complete());
    assert!(!hidden.disabled);
    assert!(decode(&replace(&row(), &[("insert_event", Val::Int(0)), ("update_event", Val::Int(0))])).is_err());
    assert!(select_sql(Selection::ActiveTables).contains("tr.is_disabled=0 AND parent.type='U'"));
    assert!(!select_sql(Selection::AllDml).contains("tr.is_disabled=0"));
    assert!(select_sql(Selection::AllDml).contains("TOP (@P1)"));
    assert!(select_sql(Selection::AllDml).contains("MAXDOP 1"));
}
#[tokio::test]
async fn application_reads_use_the_same_decoder_and_do_not_retry_an_interrupted_catalog_query() {
    let mut reader = FakeRead::new(vec![Ok(vec![row()])]);
    let native = application(&mut reader).await.unwrap();
    assert_eq!(native, [decode(&row()).unwrap()]);
    assert_eq!(reader.calls.len(), 1);
    assert!(
        matches!(reader.calls[0].parameters.as_slice(),[Parameter::Integer(value)] if *value==(MAX_APPLICATION_TRIGGERS+1) as i64)
    );
    let mut reader = FakeRead::new(vec![Err(ReadError::Stopped(CollectionStop::TimedOut))]);
    assert!(application(&mut reader).await.is_err());
    assert_eq!(reader.calls.len(), 1);
}
