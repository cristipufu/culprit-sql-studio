use super::*;
use culprit_collector::analytical::test_support::{TableTemporalCase, table_temporal_evidence};
use culprit_core::telemetry::*;
fn scan(case: TableTemporalCase) -> Report {
    let mut report: Report = serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../fixtures/mssql-demo.report.json"
    )))
    .unwrap();
    let (_, records) = table_temporal_evidence(case);
    let TelemetryRecord::Operational(row) = &records[0] else { panic!() };
    let OperationalFact::MssqlTable(native) = &row.fact else { panic!() };
    report.snapshot.tables[0].mssql_temporal_configuration = Some(native.temporal_configuration.clone());
    report.snapshot.tables[0].mssql_full_text = None;
    report
}
#[test]
fn selected_table_exposes_only_its_canonical_configuration_and_keeps_native_null_refusal_and_source_limits() {
    for case in [
        TableTemporalCase::SystemVersioned,
        TableTemporalCase::Infinite,
        TableTemporalCase::NativeNulls,
        TableTemporalCase::MixedAvailability,
    ] {
        let mut report = scan(case);
        let name = format!("{}.{}", report.snapshot.tables[0].schema, report.snapshot.tables[0].name);
        let expected =
            culprit_engine::explain_table_temporal(&report.snapshot, &report.snapshot.tables[0]).unwrap().unwrap();
        let output = super::table(&report, &name).unwrap();
        assert_eq!(output["temporal_configuration"]["fields"], json!(expected.entries[0].fields));
        assert_eq!(output["temporal_configuration"]["qualification"], expected.qualification);
        assert_eq!(output["temporal_configuration"]["limitations"], json!(expected.limitations));
        report
            .snapshot
            .sources
            .iter_mut()
            .find(|source| source.source == culprit_core::source_id::TABLES)
            .unwrap()
            .state = culprit_core::SourceState::Partial;
        assert_eq!(super::table(&report, &name).unwrap()["temporal_configuration"]["source_state"], "partial");
        report.snapshot.server.engine = culprit_core::Engine::Postgres;
        assert!(super::table(&report, &name).is_err());
    }
}
#[test]
fn unavailable_source_and_invalid_retention_cannot_become_a_healthy_or_synthetic_table_configuration() {
    let mut report = scan(TableTemporalCase::SystemVersioned);
    let name = format!("{}.{}", report.snapshot.tables[0].schema, report.snapshot.tables[0].name);
    report.snapshot.tables[0].mssql_temporal_configuration.as_mut().unwrap().history_retention_period_unit = Some(7);
    assert!(super::table(&report, &name).is_err());
    report = scan(TableTemporalCase::SystemVersioned);
    report.snapshot.sources.iter_mut().find(|source| source.source == culprit_core::source_id::TABLES).unwrap().state =
        culprit_core::SourceState::Unavailable;
    assert!(super::table(&report, &name).is_err());
    report = scan(TableTemporalCase::SystemVersioned);
    let mut other = report.snapshot.tables[0].clone();
    other.name = "Independent".into();
    other.mssql_temporal_configuration = None;
    report.snapshot.tables.push(other);
    let other_name = format!("{}.Independent", report.snapshot.tables[0].schema);
    assert!(super::table(&report, &other_name).unwrap().get("temporal_configuration").is_none());
}
