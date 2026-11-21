use super::*;

fn native_index() -> MssqlIndexSnapshot {
    let table = MssqlObjectIdentity {
        database_id: 7,
        object_id: 100,
        created_without_offset: "2026-11-10T07:00:00.123".into(),
    };
    let modified = "2026-11-10T07:01:00.123".to_string();
    let columns = (1..=2)
        .map(|id| MssqlCatalogIndexColumn {
            index_column_id: id,
            column_id: id,
            name: format!("column_{id}"),
            key_ordinal: 0,
            descending: false,
            included: true,
        })
        .collect::<Vec<_>>();
    let configuration = MssqlColumnstoreConfiguration {
        table: table.clone(),
        index_id: 1,
        schema: "dbo".into(),
        table_name: "events".into(),
        name: "cci_events".into(),
        table_modified_without_offset: modified.clone(),
        compression_delay_availability: MssqlCatalogAvailability::Available,
        compression_delay_minutes: None,
        column_store_order_availability: MssqlCatalogAvailability::Available,
        data_clustering_availability: MssqlCatalogAvailability::Unsupported,
        columns: columns
            .iter()
            .map(|column| MssqlColumnstoreColumn {
                index_column_id: column.index_column_id,
                column_id: column.column_id,
                name: column.name.clone(),
                column_store_order_ordinal: Some(if column.column_id == 1 { 1 } else { 0 }),
                data_clustering_ordinal: None,
            })
            .collect(),
    };
    MssqlIndexSnapshot {
        table,
        schema: "dbo".into(),
        table_name: "events".into(),
        index_id: 1,
        name: "cci_events".into(),
        table_modified_without_offset: modified,
        type_code: 5,
        native_type: "CLUSTERED COLUMNSTORE".into(),
        memory_optimized: false,
        unique: false,
        primary_key: false,
        disabled: false,
        columns,
        configuration: None,
        columnstore: Some(Box::new(configuration)),
        filter: None,
        reserved_bytes: None,
        usage: MssqlIndexUsage::NotCollected,
    }
}

#[test]
fn configured_minutes_native_null_and_zero_ordinals_survive_exact_round_trip() {
    for minutes in [None, Some(0), Some(15)] {
        let mut index = native_index();
        index.columnstore.as_mut().unwrap().compression_delay_minutes = minutes;
        assert!(index.is_valid());
        let decoded: MssqlIndexSnapshot = serde_json::from_slice(&serde_json::to_vec(&index).unwrap()).unwrap();
        assert_eq!(decoded, index);
        let configuration = decoded.columnstore.unwrap();
        assert_eq!(configuration.compression_delay_minutes, minutes);
        assert_eq!(configuration.columns[1].column_store_order_ordinal, Some(0));
        assert_eq!(configuration.data_clustering_availability, MssqlCatalogAvailability::Unsupported);
        assert!(configuration.columns.iter().all(|column| column.data_clustering_ordinal.is_none()));
    }
}

#[test]
fn unsupported_denied_unknown_and_available_null_remain_distinct_without_borrowed_values() {
    for availability in [
        MssqlCatalogAvailability::Unsupported,
        MssqlCatalogAvailability::PermissionDenied,
        MssqlCatalogAvailability::Unknown,
    ] {
        let mut index = native_index();
        let configuration = index.columnstore.as_mut().unwrap();
        configuration.compression_delay_availability = availability;
        configuration.column_store_order_availability = availability;
        configuration.data_clustering_availability = availability;
        for column in &mut configuration.columns {
            column.column_store_order_ordinal = None;
        }
        assert!(index.is_valid());
        assert_eq!(serde_json::from_slice::<MssqlIndexSnapshot>(&serde_json::to_vec(&index).unwrap()).unwrap(), index);
        index.columnstore.as_mut().unwrap().compression_delay_minutes = Some(0);
        assert!(!index.is_valid());
    }
}

#[test]
fn native_owner_index_clock_and_column_mismatches_cannot_attach_configuration() {
    for change in [
        "database",
        "object",
        "created",
        "index",
        "modified",
        "column",
        "position",
        "name",
        "duplicate",
        "missing",
        "rowstore",
    ] {
        let mut index = native_index();
        let configuration = index.columnstore.as_mut().unwrap();
        match change {
            "database" => configuration.table.database_id += 1,
            "object" => configuration.table.object_id += 1,
            "created" => configuration.table.created_without_offset = "2026-11-11T07:00:00.123".into(),
            "index" => configuration.index_id += 1,
            "modified" => configuration.table_modified_without_offset = "2026-11-11T07:01:00.123".into(),
            "column" => configuration.columns[0].column_id = 99,
            "position" => configuration.columns[0].index_column_id = 99,
            "name" => configuration.columns[0].name = "foreign".into(),
            "duplicate" => configuration.columns[1] = configuration.columns[0].clone(),
            "missing" => {
                configuration.columns.pop();
            }
            "rowstore" => index.type_code = 2,
            _ => unreachable!(),
        }
        assert!(!index.is_valid(), "{change}");
    }
}

#[test]
fn positive_ordinals_are_unique_and_unavailable_ordinals_cannot_become_observed_zero() {
    let mut index = native_index();
    index.columnstore.as_mut().unwrap().columns[1].column_store_order_ordinal = Some(1);
    assert!(!index.is_valid());
    let mut index = native_index();
    index.columnstore.as_mut().unwrap().columns[0].data_clustering_ordinal = Some(0);
    assert!(!index.is_valid());
    let mut index = native_index();
    let configuration = index.columnstore.as_mut().unwrap();
    configuration.data_clustering_availability = MssqlCatalogAvailability::Available;
    configuration.columns[0].data_clustering_ordinal = Some(2);
    configuration.columns[1].data_clustering_ordinal = Some(1);
    assert!(index.is_valid());
}

#[test]
fn missing_nullable_fields_and_out_of_native_range_values_are_not_automatic_or_zero() {
    let configuration = native_index().columnstore.unwrap();
    let original = serde_json::to_value(configuration).unwrap();
    for change in [
        "missing-delay",
        "missing-order",
        "missing-clustering",
        "negative-delay",
        "wide-delay",
        "wide-order",
        "fractional-order",
    ] {
        let mut value = original.clone();
        match change {
            "missing-delay" => {
                value.as_object_mut().unwrap().remove("compression_delay_minutes");
            }
            "missing-order" => {
                value["columns"][0].as_object_mut().unwrap().remove("column_store_order_ordinal");
            }
            "missing-clustering" => {
                value["columns"][0].as_object_mut().unwrap().remove("data_clustering_ordinal");
            }
            "negative-delay" => value["compression_delay_minutes"] = serde_json::json!(-1),
            "wide-delay" => value["compression_delay_minutes"] = serde_json::json!(u64::MAX),
            "wide-order" => value["columns"][0]["column_store_order_ordinal"] = serde_json::json!(256),
            "fractional-order" => value["columns"][0]["column_store_order_ordinal"] = serde_json::json!(1.5),
            _ => unreachable!(),
        }
        assert!(serde_json::from_value::<MssqlColumnstoreConfiguration>(value).is_err(), "{change}");
    }
}
