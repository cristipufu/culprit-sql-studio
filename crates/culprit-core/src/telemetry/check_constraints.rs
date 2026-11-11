//! CHECK definitions retain native trust and disablement flags, independent of rendered findings.
use super::*;
#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[ts(export)]
pub struct MssqlCheckConstraint {
    pub identity: MssqlObjectIdentity,
    pub schema: String,
    pub name: String,
    pub modified_without_offset: String,
    pub parent: MssqlObjectIdentity,
    pub parent_schema: String,
    pub parent_table: String,
    /// Zero denotes a table constraint; a positive ID denotes a column constraint.
    pub parent_column_id: u32,
    pub definition: CatalogExpression,
    pub disabled: bool,
    pub trusted: bool,
    pub not_for_replication: bool,
    pub system_named: bool,
    pub uses_database_collation: bool,
}
impl MssqlCheckConstraint {
    pub fn is_valid(&self) -> bool {
        self.identity.is_valid()
            && self.parent.is_valid()
            && self.identity.database_id == self.parent.database_id
            && [&self.schema, &self.name, &self.parent_schema, &self.parent_table]
                .into_iter()
                .all(|name| !name.is_empty() && name.len() <= 2048)
            && super::catalog::local_time(&self.modified_without_offset)
            && self.definition.is_valid()
    }
}
