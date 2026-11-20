use super::*;
use crate::settings::{Kind, query};

#[test]
fn authority_is_part_of_the_catalog_projection_instead_of_a_second_native_result_set() {
    for kind in Kind::ALL {
        let sql = query(kind, COLUMNS);
        assert!(
            sql.contains(&format!("SELECT /* culprit */ {COLUMNS}")) || sql.starts_with(&format!("SELECT {COLUMNS}")),
            "{kind:?}"
        );
        assert_eq!(
            sql.matches("AS culprit_settings_frame").count(),
            if matches!(kind, Kind::Database) { 2 } else { 1 },
            "{kind:?}"
        );
        assert!(!FRAME.contains("SELECT"));
        assert!(FRAME.starts_with("DECLARE /* culprit */"));
        for variable in
            ["@culprit_settings_database_id", "@culprit_settings_database_name", "@culprit_settings_updateability"]
        {
            assert_eq!(FRAME.matches(variable).count(), 1, "one fixed batch-start assignment");
            assert_eq!(COLUMNS.matches(variable).count(), 1, "reuse the captured value per catalog row");
        }
    }
}

#[test]
fn ordinary_query_store_options_keep_the_exact_existing_native_projection() {
    assert_eq!(query(Kind::QueryStore, ""), crate::query_store::catalog_sql::OPTIONS);
    assert_eq!(query(Kind::Database, ""), crate::database_properties::query());
    assert!(query(Kind::Scoped, "").starts_with("SELECT c.*"));
    assert!(query(Kind::Server, "").starts_with("SELECT name,"));
    assert!(!query(Kind::QueryStore, "").contains(MARKER));
}

#[test]
fn dynamic_database_properties_receive_the_original_batch_variables_and_keep_a_qualified_error_row() {
    let sql = query(Kind::Database, COLUMNS);
    assert!(sql.contains(&format!("sys.sp_executesql @culprit_database_properties{EXECUTION_PARAMETERS};")));
    let fallback = sql.split("BEGIN CATCH").nth(1).unwrap();
    assert!(fallback.contains(&format!("SELECT /* culprit */ {COLUMNS}")));
    let ordinary = query(Kind::Database, "");
    assert!(ordinary.contains("sys.sp_executesql @culprit_database_properties;"));
    assert!(!ordinary.contains("@culprit_settings_database_id"));
}
