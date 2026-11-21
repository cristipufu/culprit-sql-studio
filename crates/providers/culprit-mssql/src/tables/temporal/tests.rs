use super::*;
use crate::{
    resources::test_support::{FakeRead, replace},
    tables::{
        self, Layout,
        tests::{metadata, row},
    },
    value::Val,
};
fn system_versioned() -> Rec {
    replace(
        &row(),
        &[
            ("temporal_type", Val::Int(2)),
            ("temporal_type_desc", Val::Text("SYSTEM_VERSIONED_TEMPORAL_TABLE".into())),
            ("history_table_id", Val::Int(102)),
            ("history_retention_period", Val::Int(6)),
            ("history_retention_period_unit", Val::Int(5)),
            ("history_retention_period_unit_desc", Val::Text("MONTH".into())),
        ],
    )
}
#[tokio::test]
async fn one_scan_dispatch_and_native_decoder_preserve_the_same_six_configuration_values() {
    let point = system_versioned();
    let original = tables::decode(&point, &Layout::new(true)).unwrap();
    assert_eq!(original.temporal_configuration.temporal_type, Some(2));
    assert_eq!(original.temporal_configuration.history_table_id, Some(102));
    assert_eq!(original.temporal_configuration.history_retention_period, Some(6));
    let mut reader = FakeRead::new(vec![Ok(vec![metadata(true)]), Ok(vec![point])]);
    let scan = tables::application(&mut reader, true).await.unwrap();
    assert!(scan.complete_scope);
    assert_eq!(scan.tables[0].mssql_temporal_configuration.as_ref(), Some(&original.temporal_configuration));
    assert_eq!(reader.calls.len(), 2);
    assert!(scan.tables[0].mssql_full_text.is_some());
}
#[test]
fn independent_column_refusal_retains_other_fields_and_native_null_is_not_a_default() {
    for status in [1, 2, 3] {
        let value = decode(&replace(
            &system_versioned(),
            &[
                ("history_retention_period_unit_availability", Val::Int(status)),
                ("history_retention_period_unit", Val::Null),
            ],
        ))
        .unwrap();
        assert_eq!(value.temporal_type, Some(2));
        assert_eq!(value.history_table_id, Some(102));
        assert_eq!(value.history_retention_period, Some(6));
        assert!(value.history_retention_period_unit.is_none());
    }
    let value = decode(&replace(&row(), &[("temporal_type", Val::Null), ("temporal_type_desc", Val::Null)])).unwrap();
    assert!(value.temporal_type.is_none());
    assert_eq!(value.temporal_type_availability, MssqlCatalogAvailability::Available);
    let infinite = decode(&replace(
        &system_versioned(),
        &[
            ("history_retention_period", Val::Int(-1)),
            ("history_retention_period_unit", Val::Int(-1)),
            ("history_retention_period_unit_desc", Val::Text("INFINITE".into())),
        ],
    ))
    .unwrap();
    assert_eq!(infinite.history_retention_period, Some(-1));
}
#[test]
fn missing_alias_malformed_native_scalars_and_unavailable_payloads_refuse_instead_of_defaulting() {
    for field in COLUMNS {
        let mut missing = system_versioned();
        missing.retain_columns(|name| name != field.name);
        assert!(decode(&missing).is_err(), "{}", field.name);
        let bad = replace(&system_versioned(), &[(&format!("{}_availability", field.name), Val::Int(2))]);
        assert!(decode(&bad).is_err(), "{}", field.name);
    }
    for (field, value) in [
        ("temporal_type", Val::Text("2".into())),
        ("temporal_type", Val::Int(3)),
        ("history_table_id", Val::Int(0)),
        ("history_retention_period", Val::Int(i64::from(i32::MAX) + 1)),
        ("history_retention_period_unit", Val::Int(7)),
        ("history_retention_period_unit_desc", Val::Text("DAY".into())),
    ] {
        assert!(decode(&replace(&system_versioned(), &[(field, value)])).is_err(), "{field}");
    }
    assert!(
        tables::decode(&replace(&system_versioned(), &[("history_table_id", Val::Int(100))]), &Layout::new(true))
            .is_err()
    );
}
#[test]
fn optional_native_names_compile_only_inside_one_bounded_table_statement_after_independent_layout_checks() {
    let query = tables::select_sql(&Layout::new(true));
    assert_eq!(query.matches("EXEC /* culprit */ sys.sp_executesql").count(), 1);
    assert!(query.contains("TOP (@P1) DB_ID() AS database_id"));
    assert!(query.contains("@P1=@P1"));
    assert!(query.contains("IF @culprit_full_text_availability=0"));
    for column in COLUMNS {
        assert!(
            query.contains(&format!(
                "HAS_PERMS_BY_NAME(N'sys.tables',N'OBJECT',N'SELECT',N'{}',N'COLUMN')",
                column.name
            ))
        );
        assert!(query.contains(&format!(
            "CASE WHEN {}=0 THEN N't.{}' ELSE N'CAST(NULL AS {})' END",
            column.variable(),
            column.name,
            column.sql_type()
        )));
        assert!(
            query.contains(&format!(
                "system_type_id=TYPE_ID(N'{}') AND max_length={}",
                column.kind, column.native_bytes
            ))
        );
    }
    for forbidden in [
        "sys.periods",
        "sys.internal_tables",
        "sys.dm_fts",
        "sp_spaceused",
        "FOR SYSTEM_TIME",
        "DELETE FROM",
        "ALTER TABLE",
    ] {
        assert!(!query.contains(forbidden), "{forbidden}");
    }
}
