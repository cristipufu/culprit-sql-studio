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
        availability: MssqlFullTextCatalogAvailability::Available,
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
