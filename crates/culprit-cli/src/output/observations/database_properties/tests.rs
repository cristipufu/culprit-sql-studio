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
    assert!(!rendered.contains("ALTER DATABASE"));
    assert!(!rendered.contains("fixture.invalid"));
}
