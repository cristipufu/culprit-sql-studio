use super::*;

#[test]
fn original_current_binding_is_separate_from_fixed_provider_catalog_aliases() {
    assert_eq!(BINDING.matches("DB_ID()").count(), 1);
    assert_eq!(BINDING.matches("DB_NAME()").count(), 1);
    assert_eq!(predicate("d"), "d.name=@culprit_catalog_current_name");
    assert_eq!(predicate(""), "name=@culprit_catalog_current_name");
    let sql = bind("SELECT database_id FROM sys.databases WHERE name=@culprit_catalog_current_name");
    assert!(sql.starts_with(BINDING));
    assert!(!sql.contains("database_id=DB_ID()"));
    assert!(!sql.contains("COALESCE") && !sql.contains("TOP"));
}
