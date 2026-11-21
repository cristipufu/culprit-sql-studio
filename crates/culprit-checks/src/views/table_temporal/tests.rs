use super::*;
use serde_json::json;
fn configuration() -> MssqlTableTemporalConfiguration {
    serde_json::from_value(json!({"temporal_type_availability":"available","temporal_type":2,"temporal_type_desc_availability":"available","temporal_type_desc":"SYSTEM_VERSIONED_TEMPORAL_TABLE","history_table_id_availability":"available","history_table_id":102,"history_retention_period_availability":"available","history_retention_period":6,"history_retention_period_unit_availability":"available","history_retention_period_unit":5,"history_retention_period_unit_desc_availability":"available","history_retention_period_unit_desc":"MONTH"})).unwrap()
}
fn snapshot() -> Snapshot {
    let mut snapshot: Snapshot =
        serde_json::from_str(include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../fixtures/mssql-demo.json")))
            .unwrap();
    snapshot.tables.truncate(1);
    snapshot.tables[0].mssql_temporal_configuration = Some(configuration());
    snapshot.sources.retain(|source| source.source != source_id::TABLES);
    snapshot.sources.push(SourceOutcome {
        source: source_id::TABLES.into(),
        state: SourceState::Collected,
        reason: None,
        reason_message: None,
    });
    snapshot
}
#[test]
fn configuration_renders_six_native_fields_and_retains_limits_without_changing_findings() {
    let input = snapshot();
    let report = explain_snapshot(&input).unwrap().unwrap();
    assert_eq!(report.entries[0].fields.len(), 6);
    assert_eq!(report.entries[0].fields[0].text, "2");
    assert_eq!(report.entries[0].fields[2].text, "102");
    assert_eq!(report.entries[0].fields[3].text, "6");
    assert_eq!(report.entries[0].fields[5].text, "MONTH");
    assert_eq!(report.qualification, t!("view.table_temporal.qualification"));
    assert!(report.limitations.contains(&t!("view.table_temporal.limit.history")));
    assert!(report.entries[0].observation_key.is_none());
    assert!(report.observed_from.is_none());
    let mut without = input.clone();
    without.tables[0].mssql_temporal_configuration = None;
    assert_eq!(
        serde_json::to_value(crate::analyze(input).findings).unwrap(),
        serde_json::to_value(crate::analyze(without).findings).unwrap()
    );
}
#[test]
fn native_null_and_each_column_availability_remain_distinct_without_overwriting_other_fields() {
    for (availability, expected) in [
        (MssqlCatalogAvailability::Available, t!("view.table_temporal.native_null")),
        (MssqlCatalogAvailability::Unsupported, t!("view.table_temporal.unsupported")),
        (MssqlCatalogAvailability::PermissionDenied, t!("view.table_temporal.denied")),
        (MssqlCatalogAvailability::Unknown, t!("view.table_temporal.unknown")),
    ] {
        let mut input = snapshot();
        let value = input.tables[0].mssql_temporal_configuration.as_mut().unwrap();
        value.history_retention_period_unit_availability = availability;
        value.history_retention_period_unit = None;
        let report = explain_snapshot(&input).unwrap().unwrap();
        assert_eq!(report.entries[0].fields[4].availability, availability);
        assert_eq!(report.entries[0].fields[4].text, expected);
        assert_eq!(report.entries[0].fields[3].text, "6");
    }
}
#[test]
fn partial_or_unknown_catalog_keeps_qualification_and_unavailable_or_foreign_sources_cannot_supply_it() {
    let mut input = snapshot();
    input.sources.last_mut().unwrap().state = SourceState::Partial;
    let report = explain_snapshot(&input).unwrap().unwrap();
    assert!(report.limitations.contains(&t!("view.table_catalog.partial")));
    input.sources.pop();
    let report = explain_snapshot(&input).unwrap().unwrap();
    assert!(report.limitations.contains(&t!("view.table_catalog.unknown_source")));
    input.server.engine = Engine::Postgres;
    assert!(explain_snapshot(&input).is_err());
    input = snapshot();
    input.sources.last_mut().unwrap().state = SourceState::Unavailable;
    assert!(explain_snapshot(&input).is_err());
    input = snapshot();
    input.tables[0].mssql_temporal_configuration.as_mut().unwrap().temporal_type_desc = Some("HISTORY_TABLE".into());
    assert!(explain_snapshot(&input).is_err());
}
#[test]
fn selected_table_does_not_borrow_another_tables_settings_or_interpret_a_native_reference_as_a_parent() {
    let mut input = snapshot();
    let mut other = input.tables[0].clone();
    other.name = "Independent".into();
    other.mssql_temporal_configuration = None;
    input.tables.push(other);
    assert!(explain_table(&input, &input.tables[1]).unwrap().is_none());
    let native = input.tables[0].mssql_temporal_configuration.as_mut().unwrap();
    native.temporal_type = Some(0);
    native.temporal_type_desc = Some("NON_TEMPORAL_TABLE".into());
    let report = explain_table(&input, &input.tables[0]).unwrap().unwrap();
    assert_eq!(report.entries[0].fields[0].text, "0");
    assert_eq!(report.entries[0].fields[2].text, "102");
    assert_eq!(report.entries.len(), 1);
    assert!(report.limitations.contains(&t!("view.table_temporal.limit.history")));
}
