use super::*;
use culprit_collector::analytical::test_support::{self, StorageCase};
use culprit_core::telemetry::*;
#[test]
fn exact_native_bytes_and_unknown_used_space_keep_the_same_shared_interpretation_without_sql_or_disk_claims() {
    let (source, rows) = test_support::storage_evidence(StorageCase::Unknown);
    let rows = rows
        .into_iter()
        .map(|row| {
            let TelemetryRecord::Operational(row) = row else { panic!() };
            row
        })
        .collect();
    let report = culprit_checks::storage::explain(&StorageRead::from_source(&source, rows, true).unwrap()).unwrap();
    let text = storage(&report);
    assert!(text.contains("9007199254740993"));
    assert!(text.contains(&t!("ui.memory_failures.not_reported")));
    assert!(text.contains("255"));
    assert!(!text.contains("fixture.invalid"));
    assert!(!text.contains("ALTER DATABASE"));
}
