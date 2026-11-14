use super::*;
use crate::telemetry::{CachedStatementIdentity, StatementOrigin};

fn context() -> CachedStatementContext {
    CachedStatementContext {
        identity: CachedStatementIdentity {
            plan_handle: "0x0200".into(),
            sql_handle: "0x0300".into(),
            statement_start_bytes: 0,
            statement_end_bytes: -1,
            created_without_offset: "2026-11-14T12:00:00".into(),
            plan_generation: "1".into(),
        },
        database_id: 7,
        query_hash: None,
        origin: StatementOrigin::Application,
        monitoring_template_id: None,
        source_text_bytes: None,
        source_text_complete: false,
    }
}

fn parameters() -> MssqlModuleParameters {
    MssqlModuleParameters {
        database_id: 7,
        object_id: 1001,
        module_number: 1,
        sql_handle: "0x0300".into(),
        table_parameters: BTreeMap::from([("@Rows".into(), vec!["dbo".into(), "RowsType".into()])]),
    }
}

#[test]
fn original_sql_handle_and_database_bind_kind_without_claiming_a_counter_generation() {
    let context = context();
    let metadata = parameters();
    assert!(metadata.is_valid_for(&context));
    assert!(metadata.input(&context, "@Rows").is_some());
    assert!(metadata.input(&context, "@rows").is_none(), "case-sensitive native spelling remains exact");
    assert!(metadata.input(&context, "@local").is_none(), "unlisted variables do not become local variables");
    let mut changed = context.clone();
    changed.database_id = 8;
    assert!(!metadata.is_valid_for(&changed));
    changed = context.clone();
    changed.identity.sql_handle = "0x0301".into();
    assert!(!metadata.is_valid_for(&changed));
    changed = context.clone();
    changed.identity.plan_handle = "0x0000".into();
    assert!(!metadata.is_valid_for(&changed), "native placeholder handles cannot qualify plan/header metadata");
    let mut placeholder_metadata = metadata.clone();
    placeholder_metadata.sql_handle = "0x0000".into();
    changed = context.clone();
    changed.identity.sql_handle = placeholder_metadata.sql_handle.clone();
    assert!(!placeholder_metadata.is_valid_for(&changed), "matching zero text handles are not original-module proof");
    changed = context;
    changed.identity.plan_generation = "2".into();
    changed.identity.created_without_offset = "2026-11-14T13:00:00".into();
    assert!(
        metadata.is_valid_for(&changed),
        "same module declaration can survive recompile; it is not a runtime delta proof"
    );
}

#[test]
fn malformed_metadata_and_duplicate_native_parameters_are_rejected() {
    let context = context();
    for change in 0..5 {
        let mut value = parameters();
        match change {
            0 => value.object_id = 0,
            1 => value.module_number = 2,
            2 => {
                value.table_parameters.insert("@bad".into(), vec![]);
            }
            3 => {
                value.table_parameters.insert("@bad".into(), vec!["x".repeat(129)]);
            }
            _ => {
                value.table_parameters.insert("@bad".into(), vec!["x\nbody".into()]);
            }
        }
        assert!(!value.is_valid_for(&context));
    }
    let duplicate = r#"{"database_id":7,"object_id":1001,"module_number":1,"sql_handle":"0x0300","table_parameters":{"@Rows":["dbo","RowsType"],"@Rows":["dbo","Other"]}}"#;
    assert!(serde_json::from_str::<MssqlModuleParameters>(duplicate).is_err());
    let mut empty = parameters();
    empty.table_parameters.clear();
    assert!(empty.is_valid_for(&context), "complete declared header may contain no TVPs; it does not classify locals");
}
