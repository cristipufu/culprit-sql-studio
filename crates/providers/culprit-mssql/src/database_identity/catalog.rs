//! Select the catalog row by this batch's original current name; catalog IDs never become DMV IDs.
//! This locator does not establish a database incarnation. Existing native member/GUID/final guards do.
pub(crate) const BINDING: &str = include_str!("catalog_binding.sql");
pub(crate) const CURRENT_DATABASE_ID: &str = "@culprit_catalog_current_id";
pub(crate) const CURRENT_DATABASE_NAME: &str = "@culprit_catalog_current_name";

pub(crate) fn bind(sql: &str) -> String {
    format!("{BINDING}{sql}")
}

/// Alias is a fixed provider-owned SQL identifier, never a database name or native/user value.
pub(crate) fn predicate(alias: &str) -> String {
    if alias.is_empty() {
        format!("name={CURRENT_DATABASE_NAME}")
    } else {
        format!("{alias}.name={CURRENT_DATABASE_NAME}")
    }
}

#[cfg(test)]
mod tests;
