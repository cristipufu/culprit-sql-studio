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

#[test]
fn file_summary_cli_reuses_the_canonical_census_before_entry_limits_without_reformatting_native_values_as_floats() {
    for case in [
        StorageCase::FileCensus,
        StorageCase::FileCensusUnknownUsage,
        StorageCase::FileCensusPartial,
        StorageCase::FileCensusDenied,
        StorageCase::FileCensusLogDenied,
        StorageCase::FileCensusLarge,
    ] {
        let (source, rows) = test_support::storage_evidence(case);
        let report = culprit_checks::storage::explain(
            &StorageRead::from_source(
                &source,
                rows.into_iter()
                    .map(|row| {
                        let TelemetryRecord::Operational(row) = row else { unreachable!() };
                        row
                    })
                    .collect(),
                true,
            )
            .unwrap(),
        )
        .unwrap();
        test_support::assert_storage_file_summary(&report, case);
        let output = storage(&report);
        let summary = report.file_summary.as_ref().unwrap();
        assert!(output.contains(&summary.title) && output.contains(&summary.summary));
        for field in &summary.fields {
            assert!(
                output.contains(&field.value.clone().unwrap_or_else(|| t!("ui.memory_failures.not_reported"))),
                "{case:?}: {}",
                field.key
            );
        }
        assert!(!output.contains("fixture.invalid") && !output.contains("ALTER DATABASE"));
        if case == StorageCase::FileCensusLarge {
            assert!(output.contains("5277655810867200"));
        }
    }
}
#[test]
fn native_original_database_label_is_escaped_only_for_human_output_without_changing_canonical_json() {
    let (mut source, mut rows) = test_support::storage_evidence(StorageCase::FileCensus);
    let label = "original\n\u{1b}😀";
    let CollectionTarget::Database(database) = &mut source.target else { unreachable!() };
    database.database = label.into();
    for row in &mut rows {
        let TelemetryRecord::Operational(row) = row else { unreachable!() };
        match &mut row.fact {
            OperationalFact::Space(space) => space.database = Some(label.into()),
            OperationalFact::MssqlDatabaseFile(file) => file.database = label.into(),
            _ => unreachable!(),
        }
    }
    let report = culprit_checks::storage::explain(
        &StorageRead::from_source(
            &source,
            rows.into_iter()
                .map(|row| {
                    let TelemetryRecord::Operational(row) = row else { unreachable!() };
                    row
                })
                .collect(),
            true,
        )
        .unwrap(),
    )
    .unwrap();
    let original = serde_json::to_value(&report).unwrap();
    let output = storage(&report);
    assert!(output.contains("original\\n\\u{1b}😀"));
    assert!(!output.contains(label));
    assert_eq!(serde_json::to_value(&report).unwrap(), original);
}
