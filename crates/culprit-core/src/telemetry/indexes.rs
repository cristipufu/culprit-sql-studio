//! Native index metadata and usage remain separate from app display strings and inferred reset clocks.
use super::*;

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct MssqlCatalogIndexColumn {
    pub index_column_id: u32,
    pub column_id: u32,
    pub name: String,
    pub key_ordinal: u32,
    pub descending: bool,
    pub included: bool,
}
#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[ts(export)]
pub struct MssqlIndexUsageCounters {
    pub row_present: bool,
    pub user_seeks: Option<String>,
    pub user_scans: Option<String>,
    pub user_lookups: Option<String>,
    /// Update operations, not rows modified.
    pub user_updates: Option<String>,
    pub last_user_seek_without_offset: Option<String>,
    pub last_user_scan_without_offset: Option<String>,
    pub last_user_lookup_without_offset: Option<String>,
    pub last_user_update_without_offset: Option<String>,
    pub reset: crate::ResetMarker,
}
impl MssqlIndexUsageCounters {
    fn is_valid(&self) -> bool {
        let counters = [&self.user_seeks, &self.user_scans, &self.user_lookups, &self.user_updates];
        let times = [
            &self.last_user_seek_without_offset,
            &self.last_user_scan_without_offset,
            &self.last_user_lookup_without_offset,
            &self.last_user_update_without_offset,
        ];
        counters.into_iter().flatten().all(|value| valid_counter(value))
            && counters.into_iter().all(|value| value.is_some() == self.row_present)
            && times.into_iter().flatten().all(|at| super::catalog::local_time(at))
            && (self.row_present || times.into_iter().all(Option::is_none))
            && valid_reset(&self.reset)
    }
}
#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[serde(tag = "kind", content = "data", rename_all = "kebab-case")]
#[ts(export)]
pub enum MssqlIndexUsage {
    NotCollected,
    NotApplicable,
    Observed(Box<MssqlIndexUsageCounters>),
}
#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[ts(export)]
pub struct MssqlIndexSnapshot {
    pub table: MssqlObjectIdentity,
    pub schema: String,
    pub table_name: String,
    pub index_id: u32,
    pub name: String,
    pub table_modified_without_offset: String,
    pub type_code: u32,
    pub native_type: String,
    pub memory_optimized: bool,
    pub unique: bool,
    pub primary_key: bool,
    pub disabled: bool,
    pub columns: Vec<MssqlCatalogIndexColumn>,
    pub filter: Option<CatalogExpression>,
    pub reserved_bytes: Option<String>,
    pub usage: MssqlIndexUsage,
}
impl MssqlIndexSnapshot {
    pub fn key(&self) -> Result<String> {
        serde_json::to_vec(&(&self.table, self.index_id))
            .map(|bytes| digest(&bytes))
            .map_err(|_| CollectionStop::InvalidPage.error())
    }
    pub(super) fn redact(&mut self, engine: crate::Engine) {
        if let Some(filter) = &mut self.filter {
            filter.redact(engine);
        }
    }
    pub fn is_valid(&self) -> bool {
        let mut ordinals = std::collections::BTreeSet::new();
        let mut keys = std::collections::BTreeSet::new();
        self.table.is_valid()
            && self.index_id > 0
            && self.type_code > 0
            && [&self.schema, &self.table_name, &self.name, &self.native_type]
                .into_iter()
                .all(|name| !name.is_empty() && name.len() <= 2048)
            && super::catalog::local_time(&self.table_modified_without_offset)
            && self.columns.len() <= 1024
            && self.columns.iter().all(|column| {
                column.column_id > 0
                    && column.index_column_id > 0
                    && !column.name.is_empty()
                    && column.name.len() <= 2048
                    && ordinals.insert(column.index_column_id)
                    && (column.key_ordinal == 0 || (!column.included && keys.insert(column.key_ordinal)))
            })
            && self.filter.as_ref().is_none_or(CatalogExpression::is_valid)
            && self.reserved_bytes.as_deref().is_none_or(valid_counter)
            && match &self.usage {
                MssqlIndexUsage::Observed(usage) => !self.memory_optimized && self.type_code != 4 && usage.is_valid(),
                MssqlIndexUsage::NotApplicable => self.memory_optimized || self.type_code == 4,
                MssqlIndexUsage::NotCollected => true,
            }
    }
}
