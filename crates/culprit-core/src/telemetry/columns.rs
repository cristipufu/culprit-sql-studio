//! Column identities belong to a native table generation; expressions never carry unredacted literals.
use super::*;

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[ts(export)]
pub struct MssqlColumnDefinition {
    pub table: MssqlObjectIdentity,
    pub schema: String,
    pub table_name: String,
    pub column_id: u32,
    pub name: String,
    pub table_modified_without_offset: String,
    pub user_type_id: u32,
    pub system_type_id: u32,
    pub type_schema: String,
    pub type_name: String,
    /// Native bytes, including -1 for an unbounded SQL type.
    pub max_length: i32,
    pub precision: u8,
    pub scale: u8,
    pub nullable: bool,
    pub identity: bool,
    pub computed: Option<CatalogExpression>,
    pub default: Option<CatalogExpression>,
    pub collation: Option<String>,
}
impl MssqlColumnDefinition {
    pub fn key(&self) -> Result<String> {
        serde_json::to_vec(&(&self.table, self.column_id))
            .map(|bytes| digest(&bytes))
            .map_err(|_| CollectionStop::InvalidPage.error())
    }
    pub fn is_valid(&self) -> bool {
        self.table.is_valid()
            && self.column_id > 0
            && self.user_type_id > 0
            && self.system_type_id > 0
            && [&self.schema, &self.table_name, &self.name, &self.type_schema, &self.type_name]
                .into_iter()
                .all(|name| !name.is_empty() && name.len() <= 2048)
            && self.max_length >= -1
            && self.max_length <= i16::MAX as i32
            && self.collation.as_ref().is_none_or(|value| !value.is_empty() && value.len() <= 2048)
            && [&self.computed, &self.default].into_iter().flatten().all(|value| value.is_valid())
            && super::catalog::local_time(&self.table_modified_without_offset)
    }
}
