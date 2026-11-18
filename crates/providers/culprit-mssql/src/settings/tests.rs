use super::*;
use crate::resources::test_support::{FakeRead, replace};
use culprit_core::{Coverage, SourceState, telemetry::SourceShutdown};

pub(crate) fn database() -> Rec {
    crate::database_properties::tests::row("2020-01-01T06:02:00Z")
}
fn target() -> culprit_core::telemetry::CollectionTarget {
    culprit_core::DatabaseIdentity::server(Engine::Mssql, "sample", "fixture.invalid", 1433, None).into()
}
pub(crate) fn scoped() -> Rec {
    Rec::of(&[
        ("name", Val::Text("MAXDOP".into())),
        ("value_text", Val::Text("9007199254740993".into())),
        ("secondary_value_text", Val::Null),
        ("is_value_default", Val::Bool(true)),
    ])
}
fn server() -> Rec {
    Rec::of(&[
        ("name", Val::Text("max degree of parallelism".into())),
        ("value", Val::Text("8".into())),
        ("value_in_use", Val::Text("0".into())),
    ])
}
pub(crate) fn options() -> Rec {
    Rec::of(&[
        ("desired_state", Val::Int(2)),
        ("actual_state", Val::Int(1)),
        ("current_storage_size_mb", Val::Int(10)),
        ("future_usage_counter", Val::Int(4)),
        ("optional", Val::Null),
    ])
}
#[tokio::test]
async fn configured_effective_secondary_and_native_defaults_keep_their_own_meaning() {
    let mut reader = FakeRead::new(vec![Ok(vec![scoped()]), Ok(vec![server()])]);
    let scoped = read(&mut reader, Kind::Scoped, 100).await.unwrap().remove(0);
    assert_eq!(scoped.value.as_deref(), Some("9007199254740993"));
    assert_eq!(scoped.value_for_secondary, None);
    assert_eq!(scoped.native_is_default, Some(true));
    assert_eq!(scoped.default_source, SettingDefaultSource::NativeFlag);
    let server = read(&mut reader, Kind::Server, 100).await.unwrap().remove(0);
    assert_eq!(server.value.as_deref(), Some("8"));
    assert_eq!(server.value_in_use.as_deref(), Some("0"));
    assert_eq!(server.scope, MeasurementScope::Server);
    assert_eq!(server.default_source, SettingDefaultSource::EngineSpec);
    assert_eq!(application(&server).unwrap().value, "0");
    let mut unknown = server;
    unknown.value_in_use = None;
    assert!(application(&unknown).is_none(), "pending configuration cannot stand in for an unknown active value");
}
#[test]
fn query_store_configuration_is_not_confused_with_changing_state_or_storage() {
    let values = query_store_options(&options()).unwrap();
    let get = |name| values.iter().find(|value| value.name == name).unwrap();
    assert_eq!(get("desired_state").nature, SettingNature::Configuration);
    assert_eq!(get("actual_state").nature, SettingNature::State);
    assert_eq!(get("current_storage_size_mb").nature, SettingNature::Measurement);
    assert_eq!(get("future_usage_counter").nature, SettingNature::Unknown);
    assert_eq!(get("optional").value, None);
    assert!(application(get("optional")).is_none());
    assert!(values.iter().all(SettingObservation::is_valid));
}
#[tokio::test]
async fn native_default_flag_is_optional_without_schema_retries_or_null_to_zero_coercion() {
    let old_layout = Rec::of(&[
        ("name", Val::Text("MAXDOP".into())),
        ("value_text", Val::Null),
        ("secondary_value_text", Val::Null),
    ]);
    for (row, expected) in [(old_layout, None), (replace(&scoped(), &[("is_value_default", Val::Int(0))]), Some(false))]
    {
        let mut reader = FakeRead::new(vec![Ok(vec![row])]);
        let values = read(&mut reader, Kind::Scoped, 100).await.unwrap();
        assert_eq!(values[0].native_is_default, expected);
        assert_eq!(values[0].default_source, SettingDefaultSource::Unknown);
        assert_eq!(values[0].default_value, None);
        assert_eq!(reader.calls.len(), 1);
    }
}
#[tokio::test]
async fn malformed_duplicate_or_oversized_native_results_never_become_complete_settings() {
    for (kind, rows, limit) in [
        (Kind::Database, vec![], 100),
        (Kind::Database, vec![database(), database()], 100),
        (Kind::Scoped, vec![scoped(), scoped()], 100),
        (Kind::Scoped, vec![replace(&scoped(), &[("is_value_default", Val::Int(2))])], 100),
        (Kind::Scoped, vec![replace(&scoped(), &[("value_text", Val::Text("x".repeat(16 * 1024 + 1)))])], 100),
        (Kind::Database, vec![database()], 1),
    ] {
        let mut reader = FakeRead::new(vec![Ok(rows)]);
        assert!(read(&mut reader, kind, limit).await.is_err());
        assert_eq!(reader.calls.len(), 1, "no fallback native query");
    }
}
#[tokio::test]
async fn application_reuses_probe_options_and_reports_each_failed_source() {
    let mut reader = FakeRead::new(vec![Ok(vec![database()]), Err(ReadError::Stopped(CollectionStop::TimedOut))]);
    let mut coverage = Coverage::default();
    let values = collect_with(&mut reader, target(), true, Some(&options()), &mut coverage, "test grant").await;
    assert_eq!(reader.calls.len(), 2, "no Query Store options reread or Azure server configuration read");
    assert_eq!(reader.closed, 1);
    assert!(values.settings.iter().any(|value| value.name == "compatibility_level"));
    assert!(values.settings.iter().any(|value| value.name == "query_store_options.actual_state"));
    let properties = values.database_properties.as_ref().unwrap();
    assert!(properties.is_valid());
    assert_eq!(properties.observation.as_ref().unwrap().compatibility_level, Some(160));
    assert!(
        !properties.context.native().unwrap().has_qualified_local_owner(),
        "a selected catalog row is not a process/member proof"
    );
    for (kind, expected) in [
        (Kind::Database, SourceState::Collected),
        (Kind::Scoped, SourceState::Unavailable),
        (Kind::Server, SourceState::NotApplicable),
        (Kind::QueryStore, SourceState::Collected),
    ] {
        assert_eq!(coverage.outcomes().iter().find(|row| row.source == kind.source().name).unwrap().state, expected);
    }
    assert_eq!(
        coverage.outcomes().iter().find(|row| row.source == crate::sources::SETTINGS.name).unwrap().state,
        SourceState::Partial
    );
}
#[tokio::test]
async fn uncertain_cancellation_blocks_further_native_reads_and_cannot_mark_the_family_complete() {
    let mut reader = FakeRead::new(vec![Err(ReadError::Stopped(CollectionStop::CancellationUnconfirmed))]);
    reader.shutdown = SourceShutdown::Unconfirmed;
    let mut coverage = Coverage::default();
    let values = collect_with(&mut reader, target(), false, None, &mut coverage, "test grant").await;
    assert!(values.settings.is_empty());
    assert!(values.database_properties.as_ref().unwrap().observation.is_none());
    assert_eq!(reader.calls.len(), 1);
    assert_eq!(reader.closed, 1);
    assert_eq!(
        coverage.outcomes().iter().find(|row| row.source == crate::sources::SETTINGS.name).unwrap().state,
        SourceState::Unavailable
    );
}

#[tokio::test]
async fn terminal_settings_failures_preserve_the_actual_stop_and_do_not_dispatch_sibling_queries() {
    for stop in
        [CollectionStop::TimedOut, CollectionStop::Cancelled, CollectionStop::ByteLimit, CollectionStop::SourceChanged]
    {
        let mut reader = FakeRead::new(vec![Err(ReadError::Stopped(stop))]);
        let mut coverage = Coverage::default();
        let values = collect_with(&mut reader, target(), false, None, &mut coverage, "test grant").await;
        assert_eq!(reader.calls.len(), 1);
        assert_eq!(reader.closed, 1);
        assert!(values.settings.is_empty());
        let properties = values.database_properties.unwrap();
        assert_eq!(properties.source.stopped, Some(stop));
        assert!(properties.observation.is_none());
        assert!(properties.context.native().is_none());
    }
}

#[tokio::test]
async fn a_foreign_selected_database_cannot_supply_property_values_for_the_configured_namespace() {
    let mut reader =
        FakeRead::new(vec![Ok(vec![replace(&database(), &[("database_name", Val::Text("foreign".into()))])])]);
    let mut coverage = Coverage::default();
    let values = collect_with(&mut reader, target(), false, None, &mut coverage, "test grant").await;
    assert_eq!(reader.calls.len(), 1);
    assert_eq!(reader.closed, 1);
    assert!(values.database_properties.unwrap().observation.is_none());
    assert!(values.settings.is_empty());
}
