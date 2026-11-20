use super::*;
use crate::{
    indexes::{Layout, decode},
    resources::test_support::{FakeRead, replace},
    value::Val,
};
use serde_json::json;

fn row() -> Rec {
    crate::indexes::tests::columnstore_row()
}

fn layout() -> Layout {
    Layout { with_dmvs: false, expression_limit: MAX_CATALOG_EXPRESSION_BYTES }
}

#[tokio::test]
async fn the_same_native_projection_reaches_scan_and_snapshot_with_exact_owner_and_configured_units() {
    let raw = decode(&row(), &layout()).unwrap();
    let expected = raw.snapshot.columnstore.as_ref().unwrap().clone();
    assert_eq!(expected.table, raw.snapshot.table);
    assert_eq!(expected.index_id, raw.snapshot.index_id);
    assert_eq!(expected.compression_delay_minutes, Some(15));
    assert_eq!(expected.columns[0].column_id, 3);
    assert_eq!(expected.columns[0].column_store_order_ordinal, Some(1));
    assert_eq!(expected.columns[1].column_store_order_ordinal, Some(0));
    assert_eq!(expected.data_clustering_availability, MssqlCatalogAvailability::Unsupported);
    assert_eq!(raw.project().unwrap().mssql_columnstore, Some(expected.clone()));
    let mut reader = FakeRead::new(vec![Ok(vec![row()])]);
    let (indexes, usage) = crate::indexes::application(&mut reader, false).await.unwrap();
    assert!(!usage);
    assert_eq!(indexes[0].mssql_columnstore, Some(expected));
    assert_eq!(reader.calls.len(), 1, "optional layout and data use the same native dispatch");
}

#[test]
fn automatic_null_zero_and_unavailable_fields_do_not_collapse_into_each_other() {
    for (status, minutes) in [(0, None), (0, Some(0)), (0, Some(15)), (1, None), (2, None), (3, None)] {
        let value = minutes.map(Val::Int).unwrap_or(Val::Null);
        let raw = decode(
            &replace(&row(), &[("compression_availability", Val::Int(status)), ("compression_delay", value)]),
            &layout(),
        )
        .unwrap();
        let configuration = raw.snapshot.columnstore.unwrap();
        assert_eq!(configuration.compression_delay_minutes, minutes.map(|value| value as u32));
        assert_eq!(
            configuration.compression_delay_availability,
            match status {
                0 => MssqlCatalogAvailability::Available,
                1 => MssqlCatalogAvailability::Unsupported,
                2 => MssqlCatalogAvailability::PermissionDenied,
                _ => MssqlCatalogAvailability::Unknown,
            }
        );
    }
}

#[tokio::test]
async fn optional_denied_or_unsupported_configuration_keeps_ordinary_indexes_without_a_retry() {
    for status in [1, 2, 3] {
        let mut value: serde_json::Value =
            serde_json::from_str(&crate::read::text(&crate::indexes::tests::row(), "index_columns").unwrap()).unwrap();
        for column in value.as_array_mut().unwrap() {
            column["column_store_order_ordinal"] = serde_json::Value::Null;
            column["data_clustering_ordinal"] = serde_json::Value::Null;
        }
        let row = replace(
            &crate::indexes::tests::row(),
            &[
                ("compression_availability", Val::Int(status)),
                ("ordering_availability", Val::Int(status)),
                ("clustering_availability", Val::Int(status)),
                ("index_columns", Val::Text(value.to_string())),
            ],
        );
        let mut reader = FakeRead::new(vec![Ok(vec![row])]);
        let (indexes, _) = crate::indexes::application(&mut reader, false).await.unwrap();
        assert_eq!(indexes.len(), 1);
        assert!(indexes[0].mssql_columnstore.is_none());
        assert_eq!(indexes[0].columns, ["id", "owner\u{1f}name DESC"]);
        assert_eq!(reader.calls.len(), 1);
    }
}

#[test]
fn contradictory_statuses_native_numbers_and_missing_nullable_fields_are_refused() {
    for update in [
        ("compression_delay", Val::Int(-1)),
        ("compression_availability", Val::Int(9)),
        ("compression_availability", Val::Int(1)),
        ("clustering_availability", Val::Int(0)),
    ] {
        let field = update.0;
        let changed = replace(&row(), &[update]);
        if field == "clustering_availability" {
            assert!(
                decode(&changed, &layout()).is_ok(),
                "an available native NULL stays unknown, rather than becoming zero"
            );
        } else {
            assert!(decode(&changed, &layout()).is_err());
        }
    }
    for change in
        ["missing-order", "missing-clustering", "negative", "fractional", "too-wide", "unknown-field", "duplicate-id"]
    {
        let mut value: serde_json::Value =
            serde_json::from_str(&crate::read::text(&row(), "index_columns").unwrap()).unwrap();
        match change {
            "missing-order" => {
                value[0].as_object_mut().unwrap().remove("column_store_order_ordinal");
            }
            "missing-clustering" => {
                value[0].as_object_mut().unwrap().remove("data_clustering_ordinal");
            }
            "negative" => value[0]["column_store_order_ordinal"] = json!(-1),
            "fractional" => value[0]["column_store_order_ordinal"] = json!(1.5),
            "too-wide" => value[0]["column_store_order_ordinal"] = json!(256),
            "unknown-field" => value[0]["guessed_role"] = json!(true),
            "duplicate-id" => value[1]["column_id"] = value[0]["column_id"].clone(),
            _ => unreachable!(),
        }
        assert!(
            decode(&replace(&row(), &[("index_columns", Val::Text(value.to_string()))]), &layout()).is_err(),
            "{change}"
        );
    }
}

#[test]
fn optional_catalog_references_compile_only_in_the_selected_typed_native_layout() {
    let sql = crate::indexes::select_sql(&layout());
    assert_eq!(sql.matches("sys.sp_executesql").count(), 1);
    for (field, native_type, minimum) in [
        ("compression_delay", "int", 13),
        ("column_store_order_ordinal", "tinyint", 16),
        ("data_clustering_ordinal", "tinyint", 17),
    ] {
        assert!(sql.contains(&format!("name=N'{field}' AND system_type_id=TYPE_ID(N'{native_type}')")));
        assert!(sql.contains(&format!("@culprit_index_major>={minimum}")));
        assert!(sql.contains(&format!("N'{field}',N'COLUMN'")));
    }
    assert!(sql.contains("SELECT TOP (@P1) DB_ID() AS database_id"));
    assert!(sql.contains("SELECT TOP(1025) ic.index_column_id"));
    assert!(sql.contains("ic.object_id=i.object_id AND ic.index_id=i.index_id"));
    assert!(sql.contains("FOR JSON PATH,INCLUDE_NULL_VALUES"));
    for field in ["i.compression_delay", "ic.column_store_order_ordinal", "ic.data_clustering_ordinal"] {
        assert!(sql.contains(&format!("THEN N'{field}' ELSE N'CAST(NULL AS ")));
    }
    assert!(!sql.contains("column_store_row_group") && !sql.contains("COUNT_BIG(") && !sql.contains("SUM(total_rows"));
}
