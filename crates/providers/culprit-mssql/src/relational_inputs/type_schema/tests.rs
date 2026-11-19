use super::*;
use crate::{resources::test_support::replace, value::Val};
use serde_json::{Value, json};

pub(crate) fn with_schema(row: &Rec) -> Rec {
    let context = crate::cached_statements::context(row).unwrap();
    let original = crate::relational_inputs::parameters(row, &context).unwrap().unwrap();
    let (parameter, data_type) = original.table_parameters.first_key_value().unwrap();
    let body = json!({
        "module": {
            "identity":{"database_id": context.database_id,"object_id":original.object_id,
                "created_without_offset":"2026-11-14T12:00:00.003"},
            "schema":"dbo","name":"consume","modified_without_offset":"2026-11-19T11:00:00.007"
        },
        "parameters":[{"parameter_id":1,"name":parameter,"user_type_id":257}],
        "types":[{
            "schema_id":1,"user_type_id":257,"schema":data_type[0],"name":data_type[1],
            "backing_object":{"database_id":context.database_id,"object_id":2001,
                "created_without_offset":"2026-11-19T11:00:00.003"},
            "modified_without_offset":"2026-11-19T11:00:00.007","memory_optimized":true,
            "columns":[{"column_id":1,"name":"München","user_type_id":231,"system_type_id":231,
                "type_schema":"sys","type_name":"nvarchar","max_length":128,"precision":0,"scale":0,
                "nullable":false,"identity":false,"computed":false,"collation":"Latin1_General_100_BIN2"}],
            "indexes":[{"index_id":2,"name":"ix_rows","type_code":7,"native_type":"NONCLUSTERED HASH",
                "unique":false,"primary_key":false,"unique_constraint":false,"disabled":false,
                "hypothetical":false,"columns":[{"index_column_id":1,"column_id":1,"name":"München",
                    "key_ordinal":0,"descending":false,"included":false}]}]
        }]
    });
    replace(
        &with_body(row, body),
        &[
            ("current_type_observed_at", Val::Text("2026-11-19T12:00:00.1234567Z".into())),
            ("current_type_visibility", Val::Int(1)),
        ],
    )
}

fn row() -> Rec {
    with_schema(&crate::retained_definitions::tests::tvp_row())
}

fn with_body(row: &Rec, value: Value) -> Rec {
    let body = serde_json::to_string(&value).unwrap();
    replace(
        row,
        &[
            ("current_type_bytes", Val::Int((body.encode_utf16().count() * 2) as i64)),
            ("current_type_schema", Val::Text(body)),
        ],
    )
}

fn body(row: &Rec) -> Value {
    serde_json::from_str(&optional_text(row, "current_type_schema").unwrap().unwrap()).unwrap()
}

fn read(row: &Rec) -> MssqlCurrentTypeSchema {
    let context = crate::cached_statements::context(row).unwrap();
    let original = crate::relational_inputs::parameters(row, &context).unwrap();
    decode(row, &context, original.as_ref()).unwrap().unwrap()
}

#[test]
fn current_schema_keeps_native_type_ids_units_nullable_collation_and_hash_key_zero() {
    let row = row();
    let schema = read(&row);
    assert_eq!(schema.status, MssqlCurrentTypeSchemaStatus::Available);
    assert_eq!(schema.observed_at, "2026-11-19T12:00:00.1234567Z");
    assert_eq!(schema.parameters[0].name, "@Rows");
    assert_eq!(schema.types[0].backing_object.object_id, 2001);
    assert_eq!(schema.types[0].columns[0].max_length, 128);
    assert_eq!(schema.types[0].columns[0].name, "München");
    assert_eq!(schema.types[0].indexes[0].type_code, 7);
    assert_eq!(schema.types[0].indexes[0].columns[0].key_ordinal, 0);
    let mut native = body(&row);
    native["types"][0]["columns"][0]["collation"] = Value::Null;
    native["types"][0]["columns"][0]["max_length"] = (-1).into();
    native["types"][0]["columns"][0]["nullable"] = true.into();
    let schema = read(&with_body(&row, native));
    assert_eq!(schema.types[0].columns[0].collation, None);
    assert_eq!(schema.types[0].columns[0].max_length, -1);
    assert!(schema.types[0].columns[0].nullable);
}

#[test]
fn restricted_uncollected_and_changed_metadata_never_publish_partial_success() {
    let row = row();
    for visible in [Val::Int(0), Val::Null] {
        let schema = read(&replace(&row, &[("current_type_visibility", visible)]));
        assert_eq!(schema.status, MssqlCurrentTypeSchemaStatus::Restricted);
        assert!(schema.types.is_empty());
    }
    let context = crate::cached_statements::context(&row).unwrap();
    assert!(decode(&row, &context, None).unwrap().is_none());
    let mut original = crate::relational_inputs::parameters(&row, &context).unwrap().unwrap();
    let mut scalar_or_local = original.clone();
    scalar_or_local.table_parameters.clear();
    assert!(decode(&row, &context, Some(&scalar_or_local)).unwrap().is_none());
    original.table_parameters.get_mut("@Rows").unwrap().remove(0);
    assert_eq!(
        decode(&row, &context, Some(&original)).unwrap().unwrap().status,
        MssqlCurrentTypeSchemaStatus::NotCollected
    );
    for pointer in ["/module", "/parameters", "/types", "/parameters/0/name", "/types/0/schema", "/types/0/name"] {
        let mut native = body(&row);
        *native.pointer_mut(pointer).unwrap() = match pointer {
            "/module" => Value::Null,
            "/parameters" | "/types" => json!([]),
            "/parameters/0/name" => "@Other".into(),
            _ => "other".into(),
        };
        let schema = read(&with_body(&row, native));
        assert_eq!(schema.status, MssqlCurrentTypeSchemaStatus::Changed, "{pointer}");
        assert!(schema.module.is_none());
        assert!(schema.types.is_empty());
    }
}

#[test]
fn each_server_overflow_witness_withdraws_the_complete_context_without_a_prefix() {
    let row = row();
    for (pointer, limit) in [
        ("/parameters", MAX_MSSQL_CURRENT_TYPE_PARAMETERS),
        ("/types", MAX_MSSQL_CURRENT_TYPES),
        ("/types/0/columns", MAX_MSSQL_CURRENT_TYPE_COLUMNS),
        ("/types/0/indexes", MAX_MSSQL_CURRENT_TYPE_INDEXES),
        ("/types/0/indexes/0/columns", MAX_MSSQL_CURRENT_TYPE_INDEX_COLUMNS),
    ] {
        let mut native = body(&row);
        *native.pointer_mut(pointer).unwrap() = Value::Array(vec![json!({}); limit + 1]);
        let schema = read(&with_body(&row, native));
        assert_eq!(schema.status, MssqlCurrentTypeSchemaStatus::Oversized, "{pointer}");
        assert!(schema.types.is_empty());
    }
    let schema = read(&replace(
        &row,
        &[("current_type_bytes", Val::Int(NATIVE_JSON_BYTES as i64 + 2)), ("current_type_schema", Val::Null)],
    ));
    assert_eq!(schema.status, MssqlCurrentTypeSchemaStatus::Oversized);
}

#[test]
fn malformed_json_length_and_native_identity_do_not_become_available_schema() {
    let row = row();
    let context = crate::cached_statements::context(&row).unwrap();
    let original = crate::relational_inputs::parameters(&row, &context).unwrap();
    for changed in [
        replace(&row, &[("current_type_bytes", Val::Int(-1))]),
        replace(&row, &[("current_type_bytes", Val::Int(1))]),
        replace(&row, &[("current_type_observed_at", Val::Text("2026-11-19T12:00:00".into()))]),
        with_body(&row, json!({"module":{},"parameters":[],"types":[]})),
    ] {
        assert!(decode(&changed, &context, original.as_ref()).is_err());
    }
    let mut native = body(&row);
    native["types"][0]["indexes"][0]["columns"][0]["column_id"] = 2.into();
    assert!(decode(&with_body(&row, native), &context, original.as_ref()).is_err());
}

#[test]
fn catalog_projection_bounds_every_nested_population_before_json_aggregation() {
    let sql = relation("native_text");
    for required in [
        "TOP(17)",
        "TOP(9)",
        "TOP(65)",
        "native_text.dbid=DB_ID()",
        "m.object_id=native_text.objectid",
        "tt.type_table_object_id",
        "p.is_readonly=1",
        "ORDER BY ic.index_column_id",
        "tt.is_memory_optimized",
        "SYSUTCDATETIME()",
    ] {
        assert!(sql.contains(required), "{required}");
    }
    for forbidden in ["COUNT(", "OPENJSON", "default_value", "definition", "dm_db_index_usage_stats", "GROUP BY"] {
        assert!(!sql.contains(forbidden), "{forbidden}");
    }
    assert!(columns().contains(&format!("<={NATIVE_JSON_BYTES}")));
    let planned = crate::cached_statements::detail_sql(true, true);
    assert!(planned.contains(&relation("st")));
    assert!(!crate::cached_statements::detail_sql(false, true).contains("sys.table_types"));
    let cached = crate::cached_definitions::select_sql(&crate::cached_definitions::Limits { sql: 4096, plan: 4096 });
    assert!(cached.contains(&relation("qs")));
}

#[test]
fn current_catalog_context_reuses_shared_cached_and_retained_decoders_without_queries() {
    let row = row();
    let cached = crate::cached_definitions::decode(
        &row,
        &crate::cached_definitions::Limits { sql: 128 * 1024, plan: 128 * 1024 },
        None,
    )
    .unwrap();
    let retained = crate::actual::retained::decode(&row, 128 * 1024, 128 * 1024).unwrap();
    assert_eq!(cached.current_type_schema, retained.current_type_schema);
    assert_eq!(cached.current_type_schema.as_ref().unwrap().status, MssqlCurrentTypeSchemaStatus::Available);
    assert!(cached.statement.payload.is_available());
    assert!(retained.payload.text().is_some());
    let restricted = replace(&row, &[("current_type_visibility", Val::Int(0))]);
    let retained = crate::actual::retained::decode(&restricted, 128 * 1024, 128 * 1024).unwrap();
    assert!(retained.module_parameters.is_some());
    assert!(retained.payload.text().is_some());
    assert_eq!(retained.current_type_schema.unwrap().status, MssqlCurrentTypeSchemaStatus::Restricted);
}
