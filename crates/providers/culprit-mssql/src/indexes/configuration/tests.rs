use super::*;
use crate::{
    resources::test_support::{FakeRead, replace},
    value::Val,
};
use serde_json::json;
fn partitions(index: u32, count: usize) -> String {
    let rows=(1..=count).map(|number| json!({
        "object_id":100,"index_id":index,"partition_id":(9_007_199_254_740_993_u64+number as u64).to_string(),
        "partition_number":number,"hobt_id":(9_007_199_254_741_993_u64+number as u64).to_string(),
        "data_compression":if number==1{0}else{2},"data_compression_description":if number==1{"NONE"}else{"PAGE"},
        "xml_compression":false,"xml_compression_description":"OFF"
    })).collect::<Vec<_>>();
    serde_json::to_string(&rows).unwrap()
}
fn row() -> Rec {
    replace(&crate::indexes::tests::row(), &[("configuration_partitions", Val::Text(partitions(2, 2)))])
}
fn decoded(row: &Rec) -> Result<crate::indexes::NativeIndex> {
    crate::indexes::decode(
        row,
        &crate::indexes::Layout { with_dmvs: true, expression_limit: MAX_CATALOG_EXPRESSION_BYTES },
    )
}
#[tokio::test]
async fn one_existing_index_query_retains_native_options_and_each_exact_partition() {
    let original = decoded(&row()).unwrap().snapshot;
    let configuration = original.configuration.as_ref().unwrap();
    assert_eq!(configuration.options.as_ref().unwrap().fill_factor, Some(0));
    assert_eq!(configuration.partitions.len(), 2);
    assert!(configuration.partitions_complete);
    assert_eq!(configuration.partitions[0].data_compression, Some(0));
    assert_eq!(configuration.partitions[1].data_compression, Some(2));
    assert_eq!(configuration.partitions[0].partition_id, "9007199254740994");
    let mut reader = FakeRead::new(vec![Ok(vec![row()])]);
    let indexes = crate::indexes::application(&mut reader, true).await.unwrap().table_indexes;
    assert_eq!(reader.calls.len(), 1);
    assert_eq!(indexes[0].mssql_configuration.as_ref(), Some(configuration));
    let query = &reader.calls[0].sql;
    assert_eq!(query.matches("EXEC /* culprit */ sys.sp_executesql").count(), 1);
    assert!(query.contains("SELECT TOP (@P1) DB_ID() AS database_id"));
    assert!(query.contains("CAST("));
    assert!(query.contains("AS nvarchar(max)"));
    assert!(query.contains(&format!("SELECT TOP({}) cp.object_id", MAX_MSSQL_CONFIGURATION_PARTITIONS + 1)));
    assert!(query.contains("cp.object_id=o.object_id AND cp.index_id=i.index_id"));
    for field in OPTIONS.into_iter().chain(COMPRESSION) {
        assert!(
            query.contains(&format!("name=N''{}''", field.name)) || query.contains(&format!("name=N'{}'", field.name))
        );
        assert!(query.contains(&format!("{}={}", field.variable(), field.variable())));
    }
    for forbidden in
        ["DBCC", "sp_spaceused", "dm_db_index_physical_stats", "UPDATE STATISTICS", "ALTER INDEX", "SELECT *"]
    {
        assert!(!query.contains(forbidden));
    }
}
#[test]
fn nullable_optional_column_refusals_preserve_independent_native_fields_and_old_index_usage() {
    for status in [1, 2, 3] {
        let changed = replace(
            &row(),
            &[("configuration_fill_factor_availability", Val::Int(status)), ("configuration_fill_factor", Val::Null)],
        );
        let result = decoded(&changed).unwrap().snapshot;
        let options = result.configuration.unwrap().options.clone().unwrap();
        assert!(options.fill_factor.is_none());
        assert_eq!(options.allow_row_locks, Some(true));
        assert!(matches!(result.usage, MssqlIndexUsage::Observed(_)));
    }
    let native_null = decoded(&replace(&row(), &[("configuration_fill_factor", Val::Null)])).unwrap().snapshot;
    assert_eq!(native_null.configuration.unwrap().options.unwrap().fill_factor, None);
    let invalid = replace(&row(), &[("configuration_fill_factor_availability", Val::Int(2))]);
    assert!(decoded(&invalid).is_err());
}
#[test]
fn a_partition_prefix_keeps_its_limit_and_checks_the_unretained_lookahead_owner() {
    let count = MAX_MSSQL_CONFIGURATION_PARTITIONS + 1;
    let bounded = replace(&row(), &[("configuration_partitions", Val::Text(partitions(2, count)))]);
    let observed = decoded(&bounded).unwrap().snapshot.configuration.unwrap();
    assert_eq!(observed.partitions.len(), MAX_MSSQL_CONFIGURATION_PARTITIONS);
    assert!(!observed.partitions_complete);
    let mut raw: serde_json::Value = serde_json::from_str(&partitions(2, count)).unwrap();
    raw[count - 1]["index_id"] = json!(3);
    assert!(decoded(&replace(&row(), &[("configuration_partitions", Val::Text(raw.to_string()))])).is_err());
    assert!(decoded(&replace(&row(), &[("configuration_partitions", Val::Text(partitions(2, count + 1)))])).is_err());
}
#[test]
fn malformed_missing_or_wrong_owner_rows_never_become_a_default_or_an_index_wide_state() {
    for field in
        ["configuration_fill_factor", "configuration_partitions", "configuration_data_compression_availability"]
    {
        let mut missing = row();
        missing.retain_columns(|column| column != field);
        assert!(decoded(&missing).is_err(), "{field}");
    }
    for cause in ["object", "index", "duplicate", "zero", "description", "xml", "native-boolean"] {
        let mut raw: serde_json::Value = serde_json::from_str(&partitions(2, 2)).unwrap();
        match cause {
            "object" => raw[1]["object_id"] = json!(101),
            "index" => raw[1]["index_id"] = json!(0),
            "duplicate" => raw[1]["partition_id"] = raw[0]["partition_id"].clone(),
            "zero" => raw[0]["partition_id"] = json!("0"),
            "description" => raw[0]["data_compression_description"] = json!("PAGE"),
            "xml" => raw[0]["xml_compression_description"] = json!("ON"),
            _ => raw[0]["xml_compression"] = json!(2),
        }
        assert!(
            decoded(&replace(&row(), &[("configuration_partitions", Val::Text(raw.to_string()))])).is_err(),
            "{cause}"
        );
    }
}
#[test]
fn heap_partition_configuration_stays_on_the_existing_table_projection_without_index_population_expansion() {
    let heap = replace(
        &crate::tables::tests::row(),
        &[("is_heap", Val::Int(1)), ("configuration_partitions", Val::Text(partitions(0, 2)))],
    );
    let decoded = crate::tables::decode(&heap, &crate::tables::Layout::new(true)).unwrap();
    let configuration = decoded.heap_configuration.unwrap();
    assert_eq!(configuration.index_id, 0);
    assert!(configuration.name.is_none() && configuration.options.is_none());
    assert_eq!(configuration.partitions.len(), 2);
    let query = crate::tables::select_sql(&crate::tables::Layout::new(true));
    assert!(query.contains("cp.index_id=0"));
    assert!(query.contains("AS nvarchar(max)"), "preserve Root's intermediate-concatenation fix");
    assert_eq!(query.matches("EXEC /* culprit */ sys.sp_executesql").count(), 1);
    assert!(
        crate::tables::decode(&replace(&heap, &[("is_heap", Val::Int(0))]), &crate::tables::Layout::new(true)).is_err()
    );
}

#[test]
fn missing_partition_layout_withholds_only_its_companion_not_the_original_index_or_usage() {
    for status in [1, 2, 3] {
        let changed = replace(
            &row(),
            &[
                ("configuration_partition_identity_availability", Val::Int(status)),
                ("configuration_partitions", Val::Null),
            ],
        );
        let native = decoded(&changed).unwrap().snapshot;
        assert!(matches!(native.usage, MssqlIndexUsage::Observed(_)));
        assert_eq!(native.name, "ix_child");
        let configuration = native.configuration.unwrap();
        assert!(configuration.partitions.is_empty() && !configuration.partitions_complete);
        assert_eq!(configuration.options.as_ref().unwrap().allow_row_locks, Some(true));
    }
}
