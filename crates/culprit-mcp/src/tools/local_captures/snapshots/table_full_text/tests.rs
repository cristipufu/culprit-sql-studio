use super::*;
use culprit_core::{Report, telemetry::*};
fn report(state: MssqlTableFullText) -> TableFullTextReport {
    let mut scan: Report = serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../fixtures/mssql-demo.report.json"
    )))
    .unwrap();
    let table = &mut scan.snapshot.tables[0];
    table.mssql_full_text = Some(state);
    let mut result =
        culprit_engine::explain_table_full_text(&scan.snapshot, &scan.snapshot.tables[0]).unwrap().unwrap();
    result.entries[0].observation_key = Some("PRIVATE-ORIGINAL-ROW-RECEIPT".into());
    result.native_rows = Some(1);
    result.read_complete = Some(true);
    result
}
#[test]
fn compact_catalog_output_retains_canonical_unknowns_and_caps_entries_without_private_receipts() {
    for availability in [
        MssqlFullTextCatalogAvailability::Available,
        MssqlFullTextCatalogAvailability::Unsupported,
        MssqlFullTextCatalogAvailability::PermissionDenied,
        MssqlFullTextCatalogAvailability::Unknown,
    ] {
        let report = report(MssqlTableFullText { availability, index: None });
        let output = value(&report);
        assert_eq!(output["native_rows"], 1);
        assert_eq!(output["observed_rows"], 1);
        assert_eq!(output["entries"][0]["summary"], report.entries[0].summary);
        assert!(output["entries"][0]["metrics"].as_array().unwrap().is_empty());
        for private in [
            "observation_key",
            "PRIVATE-ORIGINAL-ROW-RECEIPT",
            "payload_sha256",
            "manifest_sha256",
            "unique_index_id",
            "catalog_id",
        ] {
            assert!(!output.to_string().contains(private));
        }
    }
    let mut report = report(MssqlTableFullText {
        availability: MssqlFullTextCatalogAvailability::Available,
        index: Some(MssqlFullTextIndex {
            unique_index_id: Some(1),
            catalog_id: Some(3),
            enabled: None,
            change_tracking: None,
            last_crawl_completed: None,
        }),
    });
    report.entries = vec![report.entries[0].clone(); 9];
    let output = value(&report);
    assert_eq!(output["entries"].as_array().unwrap().len(), super::super::ENTRY_LIMIT);
    assert_eq!(output["entries_limited"], true);
    assert!(
        output["entries"][0]["metrics"]
            .as_array()
            .unwrap()
            .iter()
            .all(|metric| metric["text"] == culprit_core::t!("view.database_properties.unknown"))
    );
    assert!(output["entries"][0]["qualification"].as_str().is_some_and(|text| !text.is_empty()));
}
