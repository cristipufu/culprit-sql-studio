use super::*;
use culprit_core::{Engine, telemetry::MssqlCurrentTypeSchema};

fn plan() -> Plan {
    crate::waits::snapshot_with(Engine::Mssql).queries().iter().find_map(|query| query.plan.clone()).unwrap()
}

fn schema() -> MssqlCurrentTypeSchema {
    serde_json::from_value(serde_json::json!({
        "observed_at":"2026-11-19T12:00:00Z", "status":"available",
        "module": {
            "identity":{"database_id":7,"object_id":1001,"created_without_offset":"2026-11-18T12:00:00"},
            "schema":"dbo","name":"UseRows","modified_without_offset":"2026-11-19T11:00:00"
        },
        "parameters":[{"parameter_id":1,"name":"@Rows","user_type_id":257}],
        "types":[{
            "schema_id":1,"user_type_id":257,"schema":"dbo","name":"RowsType",
            "backing_object":{"database_id":7,"object_id":1002,"created_without_offset":"2026-11-18T12:00:00"},
            "modified_without_offset":"2026-11-18T12:00:00","memory_optimized":false,
            "columns":[{
                "column_id":1,"name":"Id","user_type_id":56,"system_type_id":56,"type_schema":"sys","type_name":"int",
                "max_length":4,"precision":10,"scale":0,"nullable":false,"identity":false,"computed":false,"collation":null
            }],"indexes":[]
        }]
    })).unwrap()
}

#[test]
fn current_type_context_has_its_own_clock_without_changing_native_plan_advice() {
    let mut plan = plan();
    let before = serde_json::to_value(culprit_core::insights::insights(&plan)).unwrap();
    plan.facts_mut().current_type_schema = Some(Box::new(schema()));
    let view = explain(&plan).unwrap();
    assert_eq!(view.summary, t!("view.plan_type_schema.available", n = 1, count = "1", at = "2026-11-19T12:00:00Z"));
    assert_eq!(view.limitations, [t!("view.plan_type_schema.limit.current")]);
    assert_eq!(serde_json::to_value(culprit_core::insights::insights(&plan)).unwrap(), before);
    assert_eq!(super::super::plan_view(&plan, None).current_type_schema.unwrap().summary, view.summary);
}

#[test]
fn missing_and_invalid_type_context_cannot_produce_an_empty_success_panel() {
    let mut plan = plan();
    assert!(explain(&plan).is_none());
    let mut invalid = schema();
    invalid.types.clear();
    plan.facts_mut().current_type_schema = Some(Box::new(invalid));
    assert!(explain(&plan).is_none());
}

#[test]
fn unavailable_context_preserves_each_reason_without_retaining_partial_schema() {
    let mut plan = plan();
    for (status, expected) in [
        (MssqlCurrentTypeSchemaStatus::Restricted, t!("view.plan_type_schema.restricted")),
        (MssqlCurrentTypeSchemaStatus::NotCollected, t!("view.plan_type_schema.not_collected")),
        (MssqlCurrentTypeSchemaStatus::Oversized, t!("view.plan_type_schema.oversized")),
        (MssqlCurrentTypeSchemaStatus::Changed, t!("view.plan_type_schema.changed")),
    ] {
        plan.facts_mut().current_type_schema =
            Some(Box::new(MssqlCurrentTypeSchema::unavailable("2026-11-19T12:00:00Z".into(), status)));
        assert_eq!(explain(&plan).unwrap().summary, expected);
        let mut invalid = schema();
        invalid.status = status;
        plan.facts_mut().current_type_schema = Some(Box::new(invalid));
        assert!(explain(&plan).is_none(), "partial context must not escape an unavailable status");
    }
}
