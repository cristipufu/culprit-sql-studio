use super::*;
use culprit_collector::analytical::test_support::{TableTemporalCase, table_temporal_evidence};
use culprit_core::{Snapshot, telemetry::*};
#[test]
fn compact_configuration_keeps_canonical_fields_and_limits_but_withholds_original_receipt_keys() {
    let (_, records) = table_temporal_evidence(TableTemporalCase::MixedAvailability);
    let TelemetryRecord::Operational(row) = &records[0] else { panic!() };
    let OperationalFact::MssqlTable(native) = &row.fact else { panic!() };
    let mut snapshot: Snapshot =
        serde_json::from_str(include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../fixtures/mssql-demo.json")))
            .unwrap();
    snapshot.tables[0].mssql_temporal_configuration = Some(native.temporal_configuration.clone());
    let mut report = culprit_engine::explain_table_temporal(&snapshot, &snapshot.tables[0]).unwrap().unwrap();
    report.entries[0].observation_key = Some("PRIVATE-TEMPORAL-ROW-RECEIPT".into());
    report.entries = vec![report.entries[0].clone(); 9];
    report.native_rows = Some(9);
    report.read_complete = Some(false);
    let output = value(&report);
    assert_eq!(output["entries"].as_array().unwrap().len(), super::super::ENTRY_LIMIT);
    assert_eq!(output["entries_limited"], true);
    assert_eq!(output["read_complete"], false);
    assert_eq!(output["entries"][0]["fields"], json!(report.entries[0].fields));
    assert_eq!(output["qualification"], report.qualification);
    assert_eq!(output["limitations"], json!(report.limitations));
    for private in ["PRIVATE-TEMPORAL-ROW-RECEIPT", "observation_key", "manifest_sha256", "source_sha256"] {
        assert!(!output.to_string().contains(private), "{private}");
    }
}
