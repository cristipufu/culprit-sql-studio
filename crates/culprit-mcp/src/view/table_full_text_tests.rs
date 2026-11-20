use super::*;
use culprit_core::telemetry::*;

#[test]
fn single_table_schema_projects_original_native_catalog_values_and_never_a_stale_derived_summary() {
    let mut report: Report = serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../fixtures/mssql-demo.report.json"
    )))
    .unwrap();
    let table = &mut report.snapshot.tables[0];
    table.mssql_full_text = Some(MssqlTableFullText {
        availability: MssqlCatalogAvailability::Available,
        index: Some(MssqlFullTextIndex {
            unique_index_id: Some(1),
            catalog_id: Some(2),
            enabled: Some(false),
            change_tracking: None,
            last_crawl_completed: Some(true),
        }),
    });
    let name = format!("{}.{}", table.schema, table.name);
    let retained = table.mssql_full_text.clone();
    let expected =
        culprit_engine::explain_table_full_text(&report.snapshot, &report.snapshot.tables[0]).unwrap().unwrap();
    report.table_full_text = Some(Box::new(expected.clone()));
    report.table_full_text.as_mut().unwrap().entries[0].summary = "STALE_FULL_TEXT_MUST_NOT_RENDER".into();
    let output = super::table(&report, &name).unwrap();
    assert_eq!(output["full_text"]["summary"], expected.entries[0].summary);
    assert_eq!(output["full_text"]["qualification"], expected.entries[0].qualification);
    assert!(!output.to_string().contains("STALE_FULL_TEXT_MUST_NOT_RENDER"));
    report.snapshot.tables[0].mssql_full_text = None;
    assert!(super::table(&report, &name).unwrap().get("full_text").is_none());
    report.snapshot.tables[0].mssql_full_text = retained;
    report.snapshot.server.engine = culprit_core::Engine::Postgres;
    assert!(super::table(&report, &name).is_err());
    report.snapshot.server.engine = culprit_core::Engine::Mssql;
    report.snapshot.sources.iter_mut().find(|source| source.source == culprit_core::source_id::TABLES).unwrap().state =
        culprit_core::SourceState::Unavailable;
    assert!(super::table(&report, &name).is_err());
    report.snapshot.sources.iter_mut().find(|source| source.source == culprit_core::source_id::TABLES).unwrap().state =
        culprit_core::SourceState::Partial;
    let partial = super::table(&report, &name).unwrap();
    assert_eq!(partial["full_text"]["source_state"], "partial");
    assert!(
        partial["full_text"]["limitations"]
            .as_array()
            .unwrap()
            .contains(&json!(culprit_core::t!("view.table_full_text.limit.partial")))
    );
}

#[test]
fn selected_table_storage_configuration_preserves_native_nulls_scope_and_unavailable_refusal() {
    let mut report: Report = serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../fixtures/mssql-demo.report.json"
    )))
    .unwrap();
    for (memory_optimized, durability, description) in [
        (Some(false), Some(0), Some("SCHEMA_AND_DATA")),
        (Some(true), Some(1), Some("SCHEMA_ONLY")),
        (None, None, None),
    ] {
        let selected = &mut report.snapshot.tables[0];
        selected.mssql_storage_configuration = Some(MssqlTableStorageConfiguration {
            memory_optimized,
            durability,
            durability_desc: description.map(str::to_owned),
        });
        let name = selected.qualified();
        let value = super::table(&report, &name).unwrap();
        assert_eq!(value["storage_configuration"]["memory_optimized"], json!(memory_optimized));
        assert_eq!(value["storage_configuration"]["durability"], json!(durability));
        assert_eq!(value["storage_configuration"]["durability_desc"], json!(description));
        assert_eq!(
            value["storage_configuration"]["qualification"],
            culprit_core::t!("view.table_storage.qualification")
        );
    }
    report.snapshot.tables[0].mssql_full_text = None;
    let name = report.snapshot.tables[0].qualified();
    let source =
        report.snapshot.sources.iter_mut().find(|source| source.source == culprit_core::source_id::TABLES).unwrap();
    source.state = culprit_core::SourceState::Partial;
    let partial = super::table(&report, &name).unwrap();
    assert_eq!(partial["storage_configuration"]["source_state"], "partial");
    assert_eq!(
        partial["storage_configuration"]["source_qualification"],
        culprit_core::t!("view.table_catalog.partial")
    );
    report.snapshot.sources.iter_mut().find(|source| source.source == culprit_core::source_id::TABLES).unwrap().state =
        culprit_core::SourceState::Unavailable;
    assert!(super::table(&report, &name).unwrap().get("storage_configuration").is_none());
    report.snapshot.sources.retain(|source| source.source != culprit_core::source_id::TABLES);
    let unknown = super::table(&report, &name).unwrap();
    assert_eq!(unknown["storage_configuration"]["source_state"], Value::Null);
    assert_eq!(
        unknown["storage_configuration"]["source_qualification"],
        culprit_core::t!("view.table_catalog.unknown_source")
    );
    report.snapshot.server.engine = culprit_core::Engine::Postgres;
    assert!(super::table(&report, &name).unwrap().get("storage_configuration").is_none());
}

#[test]
fn storage_configuration_is_selected_by_its_original_table_and_invalid_native_pairs_are_withheld() {
    let mut report: Report = serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../fixtures/mssql-demo.report.json"
    )))
    .unwrap();
    let table = &mut report.snapshot.tables[0];
    let name = table.qualified();
    table.mssql_storage_configuration = Some(MssqlTableStorageConfiguration {
        memory_optimized: Some(true),
        durability: Some(1),
        durability_desc: Some("SCHEMA_ONLY".into()),
    });
    let mut other = table.clone();
    other.name = "IndependentTable".into();
    other.mssql_full_text = None;
    other.mssql_storage_configuration = None;
    let other_name = other.qualified();
    report.snapshot.tables.push(other);
    assert!(super::table(&report, &name).unwrap().get("storage_configuration").is_some());
    assert!(super::table(&report, &other_name).unwrap().get("storage_configuration").is_none());
    report.snapshot.tables[0].mssql_storage_configuration.as_mut().unwrap().durability_desc =
        Some("SCHEMA_AND_DATA".into());
    assert!(super::table(&report, &name).unwrap().get("storage_configuration").is_none());
}

#[test]
fn selected_table_cdc_enrollment_keeps_unknown_and_partial_catalogs_distinct_from_job_health() {
    let mut report: Report = serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../fixtures/mssql-demo.report.json"
    )))
    .unwrap();
    let name = report.snapshot.tables[0].qualified();
    for tracked in [Some(true), Some(false), None] {
        report.snapshot.tables[0].mssql_tracked_by_cdc = tracked;
        let output = super::table(&report, &name).unwrap();
        assert_eq!(output["change_data_capture"]["tracked_by_cdc"], json!(tracked));
        assert_eq!(output["change_data_capture"]["qualification"], culprit_core::t!("view.table_cdc.qualification"));
    }
    report.snapshot.sources.iter_mut().find(|source| source.source == culprit_core::source_id::TABLES).unwrap().state =
        culprit_core::SourceState::Partial;
    let partial = super::table(&report, &name).unwrap();
    assert_eq!(partial["change_data_capture"]["source_state"], "partial");
    report.snapshot.sources.iter_mut().find(|source| source.source == culprit_core::source_id::TABLES).unwrap().state =
        culprit_core::SourceState::Unavailable;
    assert!(super::table(&report, &name).unwrap().get("change_data_capture").is_none());
    report.snapshot.sources.retain(|source| source.source != culprit_core::source_id::TABLES);
    assert_eq!(super::table(&report, &name).unwrap()["change_data_capture"]["source_state"], Value::Null);
    report.snapshot.server.engine = culprit_core::Engine::Postgres;
    assert!(super::table(&report, &name).unwrap().get("change_data_capture").is_none());
}
