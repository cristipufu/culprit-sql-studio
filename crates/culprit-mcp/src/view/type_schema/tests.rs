use super::*;
use culprit_core::{Report, telemetry::MssqlCurrentTypeSchema};

fn plan() -> Plan {
    let report: Report = serde_json::from_str(include_str!("../../../../../fixtures/mssql-demo.report.json")).unwrap();
    report.snapshot.queries().iter().find_map(|query| query.plan.clone()).unwrap()
}

fn schema() -> MssqlCurrentTypeSchema {
    serde_json::from_value(json!({
        "observed_at":"2026-11-19T12:00:00Z","status":"available",
        "module":{
            "identity":{"database_id":7,"object_id":1001,"created_without_offset":"2026-11-18T12:00:00"},
            "schema":"dbo","name":"UseRows","modified_without_offset":"2026-11-19T11:00:00"
        },
        "parameters":[{"parameter_id":1,"name":"@Rows","user_type_id":257}],
        "types":[{
            "schema_id":1,"user_type_id":257,"schema":"dbo","name":"RowsType",
            "backing_object":{"database_id":7,"object_id":1002,"created_without_offset":"2026-11-18T12:00:00"},
            "modified_without_offset":"2026-11-18T12:00:00","memory_optimized":false,
            "columns":[{
                "column_id":1,"name":"Value","user_type_id":231,"system_type_id":231,"type_schema":"sys","type_name":"nvarchar",
                "max_length":-1,"precision":0,"scale":0,"nullable":true,"identity":false,"computed":false,
                "collation":"Latin1_General_100_BIN2"
            },{
                "column_id":2,"name":"ValueKey","user_type_id":56,"system_type_id":56,"type_schema":"sys","type_name":"int",
                "max_length":4,"precision":10,"scale":0,"nullable":false,"identity":false,"computed":false,"collation":null
            }],
            "indexes":[{
                "index_id":2,"name":"ix_value","type_code":2,"native_type":"NONCLUSTERED","unique":false,
                "primary_key":false,"unique_constraint":false,"disabled":false,"hypothetical":false,
                "columns":[{"index_column_id":1,"column_id":2,"name":"ValueKey","key_ordinal":1,"descending":true,"included":false}]
            }]
        }]
    })).unwrap()
}

#[test]
fn native_type_units_and_current_clock_survive_the_compact_plan_projection() {
    let mut plan = plan();
    plan.facts_mut().current_type_schema = Some(Box::new(schema()));
    let output = super::super::plan(&plan);
    let context = &output["current_type_schema"];
    assert_eq!(context["observed_at"], "2026-11-19T12:00:00Z");
    assert_eq!(context["types"][0]["columns"][0]["max_length_bytes"], -1);
    assert_eq!(context["types"][0]["columns"][0]["collation"], "Latin1_General_100_BIN2");
    assert_eq!(context["types"][0]["indexes"][0]["columns"][0]["descending"], true);
    assert_eq!(context["limitations"][0], culprit_core::t!("view.plan_type_schema.limit.current"));
    let encoded = context.to_string();
    for private in ["sql_handle", "object_id", "database_id", "default_value", "statement", "sha256"] {
        assert!(!encoded.contains(private), "private or unrelated native field {private}");
    }
}

#[test]
fn compact_columns_are_bounded_and_truncation_does_not_become_complete_metadata() {
    let mut schema = schema();
    let column = schema.types[0].columns[0].clone();
    for id in 3..=MAX_COLUMNS + 1 {
        let mut next = column.clone();
        next.column_id = id as u32;
        next.name = format!("column_{id}");
        schema.types[0].columns.push(next);
    }
    assert!(schema.is_valid());
    let mut plan = plan();
    plan.facts_mut().current_type_schema = Some(Box::new(schema));
    let output = value(&plan).unwrap();
    assert_eq!(output["types"][0]["columns"].as_array().unwrap().len(), MAX_COLUMNS);
    assert_eq!(output["types"][0]["column_count"], MAX_COLUMNS + 1);
    assert_eq!(output["types"][0]["columns_truncated"], true);
    assert!(!output.to_string().contains("column_17"));
}

#[test]
fn unavailable_and_invalid_context_cannot_reveal_partial_catalog_data() {
    let mut plan = plan();
    assert!(value(&plan).is_none());
    plan.facts_mut().current_type_schema = Some(Box::new(MssqlCurrentTypeSchema::unavailable(
        "2026-11-19T12:00:00Z".into(),
        MssqlCurrentTypeSchemaStatus::Restricted,
    )));
    let output = value(&plan).unwrap();
    assert_eq!(output["status"], "restricted");
    assert_eq!(output["types"], json!([]));
    let mut invalid = schema();
    invalid.status = MssqlCurrentTypeSchemaStatus::Restricted;
    plan.facts_mut().current_type_schema = Some(Box::new(invalid));
    assert!(value(&plan).is_none());
}
