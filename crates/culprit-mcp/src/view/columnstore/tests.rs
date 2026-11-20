use super::*;
use culprit_core::telemetry::*;

fn fixture() -> Report {
    let mut report: Report = serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../fixtures/mssql-demo.report.json"
    )))
    .unwrap();
    let mut configuration: Box<MssqlColumnstoreConfiguration> = serde_json::from_value(json!({
        "table":{"database_id":7,"object_id":100,"created_without_offset":"2026-11-10T07:00:00.123"},
        "index_id":1,"schema":"dbo","table_name":"events","name":"cci_events",
        "table_modified_without_offset":"2026-11-10T07:01:00.123",
        "compression_delay_availability":"available","compression_delay_minutes":null,
        "column_store_order_availability":"available","data_clustering_availability":"unsupported",
        "columns":[
            {"index_column_id":1,"column_id":1,"name":"id","column_store_order_ordinal":1,"data_clustering_ordinal":null},
            {"index_column_id":2,"column_id":2,"name":"payload","column_store_order_ordinal":0,"data_clustering_ordinal":null}
        ]
    })).unwrap();
    let target = &mut report.snapshot.indexes[0];
    configuration.schema = target.schema.clone();
    configuration.table_name = target.table.clone();
    configuration.name = target.name.clone();
    target.mssql_columnstore = Some(configuration);
    report
}

#[test]
fn selected_index_json_keeps_native_null_zero_availability_and_bounded_exact_counts() {
    let mut report = fixture();
    let index = &mut report.snapshot.indexes[0];
    let metadata = index.mssql_columnstore.as_mut().unwrap();
    for id in 3..=20 {
        metadata.columns.push(MssqlColumnstoreColumn {
            index_column_id: id,
            column_id: id,
            name: format!("column_{id}"),
            column_store_order_ordinal: Some(0),
            data_clustering_ordinal: None,
        });
    }
    let selected = configuration(&report.snapshot, &report.snapshot.indexes[0]).unwrap();
    assert_eq!(selected["compression_delay_availability"], "available");
    assert!(selected["compression_delay_minutes"].is_null());
    assert_eq!(selected["columns"][1]["column_store_order_ordinal"], 0);
    assert_eq!(selected["data_clustering_availability"], "unsupported");
    assert_eq!(selected["columns_total"], 20);
    assert_eq!(selected["columns_shown"], 16);
    assert_eq!(selected["columns"].as_array().unwrap().len(), 16);
    assert_eq!(selected["native_table"]["object_id"], 100);
    assert_eq!(selected["native_index_id"], 1);
    let name = format!("{}.{}", report.snapshot.indexes[0].schema, report.snapshot.indexes[0].table);
    let output = crate::view::table(&report, &name).unwrap();
    let displayed = output["indexes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|index| index["name"] == report.snapshot.indexes[0].name)
        .unwrap();
    assert_eq!(displayed["columnstore_configuration"], selected);
}

#[test]
fn foreign_engine_unavailable_source_changed_index_or_malformed_metadata_cannot_gain_a_disclosure() {
    for case in ["engine", "unavailable", "name", "owner", "ordinal"] {
        let mut report = fixture();
        match case {
            "engine" => report.snapshot.server.engine = culprit_core::Engine::Postgres,
            "unavailable" => {
                report
                    .snapshot
                    .sources
                    .iter_mut()
                    .find(|source| source.source == culprit_core::source_id::INDEXES)
                    .unwrap()
                    .state = culprit_core::SourceState::Unavailable
            }
            "name" => report.snapshot.indexes[0].name = "other-index".into(),
            "owner" => report.snapshot.indexes[0].mssql_columnstore.as_mut().unwrap().table.object_id = 0,
            "ordinal" => {
                report.snapshot.indexes[0].mssql_columnstore.as_mut().unwrap().columns[1].column_store_order_ordinal =
                    Some(1)
            }
            _ => unreachable!(),
        }
        assert!(configuration(&report.snapshot, &report.snapshot.indexes[0]).is_none(), "{case}");
    }
}
