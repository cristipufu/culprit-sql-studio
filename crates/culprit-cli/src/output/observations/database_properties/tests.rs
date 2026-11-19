use super::*;
use culprit_collector::analytical::test_support::{self, DatabasePropertiesCase};
use culprit_core::telemetry::*;
#[test]
fn text_retains_native_code_unknown_zero_clock_and_original_source_without_configuration_advice() {
    let (source, rows) = test_support::database_properties_evidence(DatabasePropertiesCase::UnknownCodes);
    let rows = rows
        .into_iter()
        .map(|row| {
            let TelemetryRecord::Operational(row) = row else { panic!() };
            row
        })
        .collect();
    let read = DatabasePropertiesRead::from_source(&source, rows, true).unwrap();
    let report = culprit_checks::database_properties::explain(&read).unwrap();
    let rendered = database_properties(&report);
    assert!(rendered.contains("255"));
    assert!(rendered.contains("ONLINE"));
    assert!(rendered.contains(&t!("view.database_properties.unknown")));
    assert!(rendered.contains("2020-01-01T12:00:00Z"));
    assert!(rendered.contains(&t!("view.database_properties.field.last_good_checkdb")));
    assert!(rendered.contains("2019-12-28T23:10:00.003"));
    assert!(rendered.contains(&t!("core.engines.mssql.integrity_history")));
    assert!(!rendered.contains("ALTER DATABASE"));
    assert!(!rendered.contains("fixture.invalid"));
}

#[test]
fn a_native_no_record_sentinel_and_unknown_metadata_keep_distinct_plain_text_values() {
    for (case, expected) in [
        (DatabasePropertiesCase::NoRecordedCheckDb, t!("view.database_properties.checkdb_not_recorded")),
        (DatabasePropertiesCase::UnknownValues, t!("view.database_properties.unknown")),
    ] {
        let (source, rows) = test_support::database_properties_evidence(case);
        let rows = rows
            .into_iter()
            .map(|row| {
                let TelemetryRecord::Operational(row) = row else { panic!() };
                row
            })
            .collect();
        let read = DatabasePropertiesRead::from_source(&source, rows, true).unwrap();
        let report = culprit_checks::database_properties::explain(&read).unwrap();
        let rendered = database_properties(&report);
        assert!(rendered.contains(&t!(
            "view.counter_ratios.cli.metric",
            label = t!("view.database_properties.field.last_good_checkdb"),
            value = expected
        )));
        assert!(!rendered.contains("1900-01-01"));
        assert!(!rendered.contains("DBCC CHECKDB"));
        assert!(
            read.observation
                .as_ref()
                .unwrap()
                .last_good_checkdb_without_offset
                .as_deref()
                .is_none_or(|value| value == "1900-01-01T00:00:00.000")
        );
    }
}
