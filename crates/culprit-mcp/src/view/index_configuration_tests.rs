use super::*;
use culprit_collector::analytical::test_support::{IndexConfigurationCase, index_configuration_evidence};
use culprit_core::{SourceState, telemetry::*};
fn scan(case: IndexConfigurationCase) -> Report {
    let mut report: Report = serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../fixtures/mssql-demo.report.json"
    )))
    .unwrap();
    let (_, records) = index_configuration_evidence(case);
    let TelemetryRecord::Operational(row) = &records[0] else { panic!() };
    match &row.fact {
        OperationalFact::MssqlIndex(native) => {
            let mut index = report.snapshot.indexes[0].clone();
            index.schema = native.parent_schema.clone();
            index.table = native.parent_name.clone();
            index.name = native.name.clone();
            index.mssql_configuration = native.configuration.clone();
            index.mssql_columnstore = None;
            report.snapshot.tables[0].schema = native.parent_schema.clone();
            report.snapshot.tables[0].name = native.parent_name.clone();
            report.snapshot.indexes = vec![index];
        }
        OperationalFact::MssqlTable(native) => {
            report.snapshot.tables[0].schema = native.schema.clone();
            report.snapshot.tables[0].name = native.name.clone();
            report.snapshot.tables[0].is_heap = Some(true);
            report.snapshot.tables[0].mssql_heap_configuration = native.heap_configuration.clone();
            report.snapshot.indexes.clear();
        }
        _ => panic!(),
    }
    report.snapshot.tables.truncate(1);
    report.snapshot.tables[0].mssql_full_text = None;
    report.snapshot.tables[0].mssql_temporal_configuration = None;
    report
}
#[test]
fn selected_index_and_heap_share_canonical_values_without_borrowing_saved_prose() {
    for case in [
        IndexConfigurationCase::Mixed,
        IndexConfigurationCase::NativeNulls,
        IndexConfigurationCase::UnavailableColumn,
        IndexConfigurationCase::Heap,
    ] {
        let mut report = scan(case);
        report.index_configuration.clear();
        let selected = &report.snapshot.tables[0];
        let name = format!("{}.{}", selected.schema, selected.name);
        let output = super::table(&report, &name).unwrap();
        let (actual, expected) = if case == IndexConfigurationCase::Heap {
            (
                &output["heap_configuration"],
                culprit_engine::explain_heap_configuration(&report.snapshot, selected).unwrap().unwrap(),
            )
        } else {
            (
                &output["indexes"][0]["persisted_configuration"],
                culprit_engine::explain_index_configuration(&report.snapshot, &report.snapshot.indexes[0])
                    .unwrap()
                    .unwrap(),
            )
        };
        assert_eq!(actual["entries"][0]["partitions"], json!(expected.entries[0].partitions));
        assert_eq!(actual["qualification"], expected.qualification);
        assert_eq!(actual["limitations"], json!(expected.limitations));
    }
}
#[test]
fn selected_configuration_cannot_promote_unavailable_foreign_or_changed_native_parent() {
    for cause in ["unavailable", "engine", "parent"] {
        let mut report = scan(IndexConfigurationCase::Mixed);
        let selected = &report.snapshot.tables[0];
        let name = format!("{}.{}", selected.schema, selected.name);
        match cause {
            "unavailable" => {
                report
                    .snapshot
                    .sources
                    .iter_mut()
                    .find(|source| source.source == culprit_core::source_id::INDEXES)
                    .unwrap()
                    .state = SourceState::Unavailable
            }
            "engine" => report.snapshot.server.engine = culprit_core::Engine::Postgres,
            _ => report.snapshot.indexes[0].mssql_configuration.as_mut().unwrap().parent_name = "Different".into(),
        }
        assert!(super::table(&report, &name).is_err(), "{cause}");
    }
}
