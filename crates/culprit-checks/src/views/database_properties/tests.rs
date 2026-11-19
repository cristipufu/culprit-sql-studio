use super::*;
fn read() -> DatabasePropertiesRead {
    let point: DatabasePropertiesObservation = serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../fixtures/database-properties.native.json"
    )))
    .unwrap();
    DatabasePropertiesRead {
        target: culprit_core::DatabaseIdentity::server(
            culprit_core::Engine::Mssql,
            "sample",
            "fixture.invalid",
            1433,
            None,
        )
        .into(),
        context: ObservationReadContext::Collector {
            observed_from: "2020-01-01T12:00:00Z".into(),
            observed_until: "2020-01-01T12:00:01Z".into(),
        },
        source: StateSourceCoverage::collected(DATABASE_PROPERTIES_SOURCE.into(), 1),
        observation: Some(Box::new(point)),
        read_complete: true,
    }
}
fn field<'a>(report: &'a DatabasePropertiesReport, key: &str) -> &'a DatabasePropertyField {
    report.fields.iter().find(|field| field.key == key).unwrap()
}
#[test]
fn one_interpretation_preserves_native_codes_zero_unknown_and_independent_connected_updateability() {
    let mut read = read();
    let point = read.observation.as_mut().unwrap();
    point.updateability = Some("READ_ONLY".into());
    point.catalog_state.code = Some(255);
    point.auto_update_statistics_async = None;
    let report = explain(&read).unwrap();
    assert_eq!(report.fields.len(), 17);
    assert_eq!(field(&report, "catalog_read_only").value.as_deref(), Some(t!("view.database_properties.no").as_str()));
    assert_eq!(
        field(&report, "updateability").value.as_deref(),
        Some(t!("view.database_properties.read_only").as_str())
    );
    assert_eq!(field(&report, "target_recovery_time_seconds").value.as_deref(), Some("0"));
    assert!(field(&report, "catalog_state").value.is_none());
    assert_eq!(field(&report, "catalog_state").native_code.as_deref(), Some("255"));
    assert_eq!(field(&report, "catalog_state").native_description.as_deref(), Some("ONLINE"));
    assert!(field(&report, "auto_update_statistics_async").value.is_none());
    assert!(serde_json::to_value(&report).unwrap().get("fix_sql").is_none());
}
#[test]
fn persisted_raw_properties_reanalyze_to_the_same_report_and_sharing_keeps_an_explicit_gap() {
    let mut snapshot: Snapshot =
        serde_json::from_str(include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../fixtures/mssql-demo.json")))
            .unwrap();
    let mut read = read();
    read.observation.as_mut().unwrap().collation = Some("private-collation-canary".into());
    snapshot.operations.database_properties = Some(Box::new(read));
    let before = explain_snapshot(&snapshot).unwrap().unwrap();
    let restored: Snapshot = serde_json::from_slice(&serde_json::to_vec(&snapshot).unwrap()).unwrap();
    let after = explain_snapshot(&restored).unwrap().unwrap();
    assert_eq!(serde_json::to_value(before).unwrap(), serde_json::to_value(after).unwrap());
    let shared = culprit_core::redact::redact_snapshot(&snapshot);
    assert!(shared.operations.database_properties.is_none());
    assert!(!serde_json::to_string(&shared).unwrap().contains("private-collation-canary"));
    let shared_report = explain_snapshot(&shared).unwrap().unwrap();
    assert_eq!(shared_report.source_state, Some(SourceState::Unavailable));
    assert!(shared_report.fields.is_empty());
}
#[test]
fn body_read_gaps_and_unavailable_or_inapplicable_sources_never_become_measured_empty_properties() {
    let mut read = read();
    read.observation = None;
    read.read_complete = false;
    let report = explain(&read).unwrap();
    assert_eq!(report.source_state, Some(SourceState::Partial));
    assert_eq!(report.native_source.as_ref().unwrap().records, 1);
    assert!(report.fields.is_empty());
    for source in [
        StateSourceCoverage::unavailable(
            DATABASE_PROPERTIES_SOURCE.into(),
            culprit_core::ErrorCode::PermissionDenied,
            None,
        ),
        StateSourceCoverage::not_applicable(DATABASE_PROPERTIES_SOURCE.into()),
    ] {
        read.source = source.clone();
        read.read_complete = true;
        assert_eq!(explain(&read).unwrap().source_state, Some(source.state));
    }
    read.source = StateSourceCoverage::collected(DATABASE_PROPERTIES_SOURCE.into(), 1);
    assert!(explain(&read).is_err(), "read-complete must not hide a missing original body");
}

#[test]
fn last_recorded_checkdb_is_informational_primary_reported_metadata_with_unknown_offset() {
    for (native, expected) in [
        (Some("2019-12-28T23:10:00.003"), Some("2019-12-28T23:10:00.003".to_owned())),
        (Some("1900-01-01T00:00:00.000"), Some(t!("view.database_properties.checkdb_not_recorded"))),
        (None, None),
    ] {
        let mut read = read();
        let point = read.observation.as_mut().unwrap();
        point.last_good_checkdb_without_offset = native.map(str::to_owned);
        point.updateability = Some("READ_ONLY".into());
        let report = explain(&read).unwrap();
        let check = field(&report, "last_good_checkdb_without_offset");
        assert_eq!(check.value, expected);
        assert_eq!(check.label, t!("view.database_properties.field.last_good_checkdb"));
        assert!(report.limitations.contains(&t!("core.engines.mssql.integrity_history")));
        assert_eq!(report.observed_at.as_deref(), Some("2020-01-01T12:00:00.0000001Z"));
        assert_eq!(read.observation.as_ref().unwrap().last_good_checkdb_without_offset.as_deref(), native);
        assert!(!serde_json::to_string(&report).unwrap().contains("age_seconds"));
    }
}
