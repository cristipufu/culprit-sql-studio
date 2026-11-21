use super::*;
use serde_json::json;
fn configuration() -> MssqlIndexConfiguration {
    serde_json::from_value(json!({
        "table":{"database_id":5,"object_id":100,"created_without_offset":"2026-11-10T07:00:00.123"},
        "schema":"dbo","table_name":"Orders","table_modified_without_offset":"2026-11-10T07:01:00.123",
        "index_id":2,"name":"ix_orders","type_code":2,
        "options":{
            "fill_factor_availability":"available","fill_factor":0,
            "padded_availability":"available","padded":false,
            "ignore_duplicate_key_availability":"available","ignore_duplicate_key":false,
            "allow_row_locks_availability":"available","allow_row_locks":true,
            "allow_page_locks_availability":"available","allow_page_locks":true
        },
        "partition_identity_availability":"available","data_compression_availability":"available",
        "data_compression_description_availability":"available","xml_compression_availability":"unsupported",
        "xml_compression_description_availability":"unsupported","partitions_complete":true,
        "partitions":[
            {"object_id":100,"index_id":2,"partition_id":"9007199254740993","partition_number":1,"hobt_id":"9007199254740995","data_compression":0,"data_compression_description":"NONE","xml_compression":null,"xml_compression_description":null},
            {"object_id":100,"index_id":2,"partition_id":"9007199254740994","partition_number":2,"hobt_id":"9007199254740996","data_compression":2,"data_compression_description":"PAGE","xml_compression":null,"xml_compression_description":null}
        ]
    })).unwrap()
}
fn index(configuration: MssqlIndexConfiguration) -> MssqlIndexSnapshot {
    MssqlIndexSnapshot {
        table: configuration.table.clone(),
        schema: configuration.schema.clone(),
        table_name: configuration.table_name.clone(),
        index_id: 2,
        name: "ix_orders".into(),
        table_modified_without_offset: configuration.table_modified_without_offset.clone(),
        type_code: 2,
        native_type: "NONCLUSTERED".into(),
        memory_optimized: false,
        unique: false,
        primary_key: false,
        disabled: false,
        columns: vec![],
        columnstore: None,
        configuration: Some(Box::new(configuration)),
        filter: None,
        reserved_bytes: None,
        usage: MssqlIndexUsage::NotCollected,
    }
}
#[test]
fn options_keep_native_zero_null_and_independent_column_refusals() {
    let original = configuration();
    assert!(original.is_valid());
    assert_eq!(original.options.as_ref().unwrap().fill_factor, Some(0));
    for state in ["available", "unsupported", "permission-denied", "unknown"] {
        let mut raw = serde_json::to_value(&original).unwrap();
        raw["options"]["fill_factor_availability"] = json!(state);
        raw["options"]["fill_factor"] = json!(null);
        let restored: MssqlIndexConfiguration = serde_json::from_value(raw.clone()).unwrap();
        assert!(restored.is_valid());
        assert_eq!(restored.options.unwrap().allow_row_locks, Some(true));
        raw["options"].as_object_mut().unwrap().remove("fill_factor");
        assert!(serde_json::from_value::<MssqlIndexConfiguration>(raw).is_err());
    }
    let mut changed = original;
    changed.options.as_mut().unwrap().fill_factor = Some(101);
    assert!(!changed.is_valid());
}
#[test]
fn mixed_partition_states_and_exact_bigint_ids_survive_without_becoming_a_whole_index_setting() {
    let original = configuration();
    assert!(original.is_valid());
    let restored: MssqlIndexConfiguration = serde_json::from_slice(&serde_json::to_vec(&original).unwrap()).unwrap();
    assert_eq!(restored, original);
    assert_eq!(restored.partitions[0].data_compression, Some(0));
    assert_eq!(restored.partitions[1].data_compression, Some(2));
    assert_eq!(restored.partitions[0].partition_id, "9007199254740993");
    let mut bounded = restored;
    bounded.partitions_complete = false;
    assert!(bounded.is_valid());
}
#[test]
fn wrong_owner_generation_or_native_partition_contract_never_borrows_original_configuration() {
    let original = index(configuration());
    assert!(original.is_valid());
    for cause in ["database", "object", "created", "modified", "index", "name", "type"] {
        let mut changed = original.clone();
        match cause {
            "database" => changed.table.database_id = 6,
            "object" => changed.table.object_id = 101,
            "created" => changed.table.created_without_offset = "2026-11-10T07:00:01.123".into(),
            "modified" => changed.table_modified_without_offset = "2026-11-10T07:02:00.123".into(),
            "index" => changed.index_id = 3,
            "name" => changed.name = "another".into(),
            _ => changed.type_code = 1,
        }
        assert!(!changed.is_valid(), "{cause}");
    }
    for cause in [
        "foreign-object",
        "foreign-index",
        "duplicate",
        "unordered",
        "zero-number",
        "padded-id",
        "too-wide",
        "description",
        "unavailable-value",
    ] {
        let mut changed = configuration();
        match cause {
            "foreign-object" => changed.partitions[1].object_id = 101,
            "foreign-index" => changed.partitions[1].index_id = 1,
            "duplicate" => changed.partitions[1].partition_id = changed.partitions[0].partition_id.clone(),
            "unordered" => changed.partitions.swap(0, 1),
            "zero-number" => changed.partitions[0].partition_number = 0,
            "padded-id" => changed.partitions[0].partition_id = "01".into(),
            "too-wide" => changed.partitions[0].partition_id = "9223372036854775808".into(),
            "description" => changed.partitions[0].data_compression_description = Some("PAGE".into()),
            _ => changed.xml_compression_availability = MssqlCatalogAvailability::Unsupported,
        }
        if cause == "unavailable-value" {
            changed.partitions[0].xml_compression = Some(false);
        }
        assert!(!changed.is_valid(), "{cause}");
    }
}
#[test]
fn every_lookahead_row_remains_subject_to_the_same_identity_and_order_policy() {
    let mut original = configuration();
    original.partitions = (1..=MAX_MSSQL_CONFIGURATION_PARTITIONS + 1)
        .map(|n| {
            let mut row = original.partitions[0].clone();
            row.partition_number = n as u32;
            row.partition_id = (9_007_199_254_740_000_u64 + n as u64).to_string();
            row
        })
        .collect();
    assert!(!original.is_valid());
    assert!(original.partition_rows_are_valid());
    original.partitions.last_mut().unwrap().object_id = 101;
    assert!(!original.partition_rows_are_valid(), "unretained lookahead cannot smuggle a foreign target");
}

#[test]
fn future_native_codes_remain_raw_and_native_null_does_not_become_an_encoding_default() {
    let mut native = configuration();
    native.partitions[0].data_compression = Some(255);
    native.partitions[0].data_compression_description = Some("FUTURE_NATIVE_ENCODING".into());
    assert!(native.is_valid());
    native.partitions[0].data_compression = None;
    native.partitions[0].data_compression_description = None;
    assert!(native.is_valid());
    assert_eq!(native.partitions[0].data_compression, None);
    native.data_compression_availability = MssqlCatalogAvailability::Unknown;
    native.partitions[1].data_compression = None;
    assert!(native.is_valid());
}
