use super::*;

pub(crate) fn schema() -> MssqlCurrentTypeSchema {
    MssqlCurrentTypeSchema {
        observed_at: "2026-11-19T12:00:00.1234567Z".into(),
        status: MssqlCurrentTypeSchemaStatus::Available,
        module: Some(MssqlCurrentTypeModule {
            identity: MssqlObjectIdentity {
                database_id: 7,
                object_id: 1001,
                created_without_offset: "2026-11-14T12:00:00.003".into(),
            },
            schema: "dbo".into(),
            name: "consume".into(),
            modified_without_offset: "2026-11-19T11:00:00.007".into(),
        }),
        parameters: vec![MssqlCurrentTypeParameter { parameter_id: 1, name: "@Rows".into(), user_type_id: 257 }],
        types: vec![MssqlCurrentTableType {
            schema_id: 1,
            user_type_id: 257,
            schema: "dbo".into(),
            name: "RowsType".into(),
            backing_object: MssqlObjectIdentity {
                database_id: 7,
                object_id: 2001,
                created_without_offset: "2026-11-19T11:00:00.003".into(),
            },
            modified_without_offset: "2026-11-19T11:00:00.007".into(),
            memory_optimized: true,
            columns: vec![MssqlCurrentTypeColumn {
                column_id: 1,
                name: "München".into(),
                user_type_id: 231,
                system_type_id: 231,
                type_schema: "sys".into(),
                type_name: "nvarchar".into(),
                max_length: 128,
                precision: 0,
                scale: 0,
                nullable: false,
                identity: false,
                computed: false,
                collation: Some("Latin1_General_100_BIN2".into()),
            }],
            indexes: vec![MssqlCurrentTypeIndex {
                index_id: 2,
                name: "ix_rows".into(),
                type_code: 7,
                native_type: "NONCLUSTERED HASH".into(),
                unique: false,
                primary_key: false,
                unique_constraint: false,
                disabled: false,
                hypothetical: false,
                columns: vec![MssqlCatalogIndexColumn {
                    index_column_id: 1,
                    column_id: 1,
                    name: "München".into(),
                    key_ordinal: 0,
                    descending: false,
                    included: false,
                }],
            }],
        }],
    }
}

#[test]
fn current_schema_preserves_native_units_names_generations_and_hash_index_ordinals() {
    let current = schema();
    assert!(current.is_valid());
    let encoded = serde_json::to_string(&current).unwrap();
    let decoded: MssqlCurrentTypeSchema = serde_json::from_str(&encoded).unwrap();
    assert_eq!(decoded, current);
    assert_eq!(decoded.types[0].columns[0].max_length, 128, "native bytes remain bytes");
    assert_eq!(decoded.types[0].indexes[0].type_code, 7);
    assert_eq!(decoded.types[0].indexes[0].columns[0].key_ordinal, 0);
    assert!(decoded.types[0].memory_optimized);
    assert!(!encoded.contains("default_value"));
    assert!(!encoded.contains("row_count"));
    let mut equivalent = current;
    equivalent.observed_at = "2026-11-19T12:00:00.1234567+00:00".into();
    assert!(equivalent.is_valid(), "an explicitly supplied equivalent UTC offset remains a valid instant");
}

#[test]
fn current_association_requires_the_original_exact_parameter_and_two_part_type_name() {
    let definition = super::super::retained_tests::retained();
    let current = schema();
    assert!(current.is_valid_for(&definition.context, definition.module_parameters.as_ref()));
    for case in ["missing", "parameter", "case", "one-part", "type", "schema", "database", "module", "sql-token"] {
        let mut original = definition.module_parameters.clone();
        match case {
            "missing" => original = None,
            "parameter" => {
                let original = original.as_mut().unwrap();
                let data_type = original.table_parameters.remove("@Rows").unwrap();
                original.table_parameters.insert("@Other".into(), data_type);
            }
            "case" => {
                let original = original.as_mut().unwrap();
                let data_type = original.table_parameters.remove("@Rows").unwrap();
                original.table_parameters.insert("@rows".into(), data_type);
            }
            "one-part" => {
                original.as_mut().unwrap().table_parameters.get_mut("@Rows").unwrap().remove(0);
            }
            "type" => original.as_mut().unwrap().table_parameters.get_mut("@Rows").unwrap()[1] = "Other".into(),
            "schema" => original.as_mut().unwrap().table_parameters.get_mut("@Rows").unwrap()[0] = "other".into(),
            "database" => original.as_mut().unwrap().database_id = 8,
            "module" => original.as_mut().unwrap().object_id = 1002,
            _ => original.as_mut().unwrap().sql_handle = "0x0301".into(),
        };
        assert!(!current.is_valid_for(&definition.context, original.as_ref()), "{case}");
    }
}

#[test]
fn unavailable_metadata_never_carries_a_successful_empty_or_partial_schema() {
    for status in [
        MssqlCurrentTypeSchemaStatus::Restricted,
        MssqlCurrentTypeSchemaStatus::NotCollected,
        MssqlCurrentTypeSchemaStatus::Oversized,
        MssqlCurrentTypeSchemaStatus::Changed,
    ] {
        let mut unavailable = MssqlCurrentTypeSchema::unavailable(schema().observed_at, status);
        assert!(unavailable.is_valid());
        unavailable.module = schema().module;
        assert!(!unavailable.is_valid());
    }
    let mut empty = schema();
    empty.parameters.clear();
    empty.types.clear();
    assert!(!empty.is_valid());
}

#[test]
fn invalid_current_ids_dangling_keys_duplicates_and_clocks_are_refused() {
    for case in [
        "native-clock",
        "excess-precision",
        "module-date",
        "module-id",
        "database",
        "type-id",
        "backing-id",
        "duplicate-parameter",
        "duplicate-type",
        "duplicate-column",
        "duplicate-column-name",
        "duplicate-index",
        "duplicate-key",
        "duplicate-key-column",
        "dangling-type",
        "dangling-column",
        "column-name",
        "negative-length",
        "unique-flag",
    ] {
        let mut value = schema();
        match case {
            "native-clock" => value.observed_at = "2026-11-19T12:00:00".into(),
            "excess-precision" => value.observed_at = "2026-11-19T12:00:00.1234567891Z".into(),
            "module-date" => value.module.as_mut().unwrap().modified_without_offset.push('Z'),
            "module-id" => value.module.as_mut().unwrap().identity.object_id = 0,
            "database" => value.types[0].backing_object.database_id = 8,
            "type-id" => value.types[0].user_type_id = 0,
            "backing-id" => value.types[0].backing_object.object_id = 0,
            "duplicate-parameter" => value.parameters.push(value.parameters[0].clone()),
            "duplicate-type" => value.types.push(value.types[0].clone()),
            "duplicate-column" => {
                let column = value.types[0].columns[0].clone();
                value.types[0].columns.push(column);
            }
            "duplicate-column-name" => {
                let mut column = value.types[0].columns[0].clone();
                column.column_id = 2;
                value.types[0].columns.push(column);
            }
            "duplicate-index" => {
                let index = value.types[0].indexes[0].clone();
                value.types[0].indexes.push(index);
            }
            "duplicate-key" => {
                let key = value.types[0].indexes[0].columns[0].clone();
                value.types[0].indexes[0].columns.push(key);
            }
            "duplicate-key-column" => {
                let mut column = value.types[0].indexes[0].columns[0].clone();
                column.index_column_id = 2;
                value.types[0].indexes[0].columns.push(column);
            }
            "dangling-type" => value.parameters[0].user_type_id = 258,
            "dangling-column" => value.types[0].indexes[0].columns[0].column_id = 2,
            "column-name" => value.types[0].indexes[0].columns[0].name = "other".into(),
            "negative-length" => value.types[0].columns[0].max_length = -2,
            _ => value.types[0].indexes[0].primary_key = true,
        }
        assert!(!value.is_valid(), "{case}");
    }
}

#[test]
fn every_native_collection_has_a_bounded_deserializer() {
    let base = serde_json::to_value(schema()).unwrap();
    for (pointer, limit) in [
        ("/parameters", MAX_MSSQL_CURRENT_TYPE_PARAMETERS),
        ("/types", MAX_MSSQL_CURRENT_TYPES),
        ("/types/0/columns", MAX_MSSQL_CURRENT_TYPE_COLUMNS),
        ("/types/0/indexes", MAX_MSSQL_CURRENT_TYPE_INDEXES),
        ("/types/0/indexes/0/columns", MAX_MSSQL_CURRENT_TYPE_INDEX_COLUMNS),
    ] {
        let mut value = base.clone();
        let items = value.pointer_mut(pointer).unwrap().as_array_mut().unwrap();
        let first = items[0].clone();
        *items = vec![first; limit + 1];
        assert!(serde_json::from_value::<MssqlCurrentTypeSchema>(value).is_err(), "{pointer}");
    }
}

#[test]
fn a_current_schema_does_not_rewrite_or_supply_original_declarations() {
    let mut definition = super::super::retained_tests::retained();
    let original = definition.module_parameters.clone();
    definition.current_type_schema = Some(schema());
    assert!(definition.is_valid());
    assert_eq!(definition.module_parameters, original);
    definition.module_parameters = None;
    assert!(!definition.is_valid());
    definition.current_type_schema =
        Some(MssqlCurrentTypeSchema::unavailable(schema().observed_at, MssqlCurrentTypeSchemaStatus::NotCollected));
    assert!(definition.is_valid());
}

#[test]
fn plan_facts_preserve_valid_current_schema_and_withdraw_invalid_available_or_partial_context() {
    let facts = crate::PlanFacts { current_type_schema: Some(Box::new(schema())), ..Default::default() };
    let encoded = serde_json::to_value(&facts).unwrap();
    let decoded: crate::PlanFacts = serde_json::from_value(encoded.clone()).unwrap();
    assert_eq!(decoded.current_type_schema, facts.current_type_schema);
    for case in ["empty-types", "partial-unavailable"] {
        let mut invalid = encoded.clone();
        match case {
            "empty-types" => invalid["current_type_schema"]["types"] = serde_json::json!([]),
            _ => invalid["current_type_schema"]["status"] = "restricted".into(),
        }
        let decoded: crate::PlanFacts = serde_json::from_value(invalid).unwrap();
        assert!(decoded.current_type_schema.is_none(), "{case}");
    }
}
