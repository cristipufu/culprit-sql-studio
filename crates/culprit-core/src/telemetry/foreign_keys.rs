use super::*;

#[derive(Serialize, Deserialize, TS, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum MssqlForeignKeyAction {
    NoAction,
    Cascade,
    SetNull,
    SetDefault,
}
#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct MssqlForeignKeyColumn {
    pub ordinal: u32,
    pub parent_column_id: u32,
    pub parent_column: String,
    pub referenced_column_id: u32,
    pub referenced_column: String,
}
#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[ts(export)]
pub struct MssqlForeignKey {
    pub identity: MssqlObjectIdentity,
    pub schema: String,
    pub name: String,
    pub modified_without_offset: String,
    pub parent: MssqlObjectIdentity,
    pub parent_schema: String,
    pub parent_table: String,
    pub referenced: MssqlObjectIdentity,
    pub referenced_schema: String,
    pub referenced_table: String,
    pub columns: Vec<MssqlForeignKeyColumn>,
    pub delete_action: MssqlForeignKeyAction,
    pub update_action: MssqlForeignKeyAction,
    pub disabled: bool,
    pub trusted: bool,
    pub not_for_replication: bool,
    pub system_named: bool,
}
impl MssqlForeignKey {
    pub fn is_valid(&self) -> bool {
        let name = |name: &str| !name.is_empty() && name.len() <= 2048;
        let mut parent_columns = std::collections::BTreeSet::new();
        let mut referenced_columns = std::collections::BTreeSet::new();
        self.identity.is_valid()
            && self.parent.is_valid()
            && self.referenced.is_valid()
            && self.identity.database_id == self.parent.database_id
            && self.identity.database_id == self.referenced.database_id
            && [
                &self.schema,
                &self.name,
                &self.parent_schema,
                &self.parent_table,
                &self.referenced_schema,
                &self.referenced_table,
            ]
            .into_iter()
            .all(|value| name(value))
            && super::catalog::local_time(&self.modified_without_offset)
            && !self.columns.is_empty()
            && self.columns.len() <= 1024
            && self.columns.iter().enumerate().all(|(ordinal, column)| {
                column.ordinal as usize == ordinal + 1
                    && column.parent_column_id > 0
                    && column.referenced_column_id > 0
                    && name(&column.parent_column)
                    && name(&column.referenced_column)
                    && parent_columns.insert(column.parent_column_id)
                    && referenced_columns.insert(column.referenced_column_id)
            })
    }
}
