//! Current native parent classification is separate from historical originals and table-only index advice.
use super::*;
use std::collections::BTreeSet;

pub const MAX_CURRENT_MSSQL_INDEX_ROWS: usize = 10_000;

#[derive(Serialize, Deserialize, TS, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum MssqlIndexParentKind {
    Table,
    IndexedView,
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct MssqlIndexedViewCatalog {
    /// The existing Indexes acquisition owns tables and views together. This is not an independent source read.
    pub source: crate::SourceOutcome,
    /// Retained index rows from the combined population, including table rows. Unknown after source refusal.
    #[serde(deserialize_with = "super::configuration::required_nullable")]
    pub retained_index_rows: Option<u32>,
    /// Only the parent/index prefix. Optional configuration/partition availability remains on each row.
    pub parent_prefix_complete: bool,
    pub indexes: Vec<MssqlIndexSnapshot>,
}
impl MssqlIndexedViewCatalog {
    pub fn is_valid(&self) -> bool {
        self.source.source == crate::source_id::INDEXES
            && self.retained_index_rows.is_none_or(|rows| rows as usize <= MAX_CURRENT_MSSQL_INDEX_ROWS)
            && self.indexes.len() <= self.retained_index_rows.unwrap_or(0) as usize
            && match self.source.state {
                crate::SourceState::Unavailable | crate::SourceState::NotApplicable => !self.parent_prefix_complete,
                crate::SourceState::Empty => {
                    self.retained_index_rows == Some(0) && self.parent_prefix_complete && self.indexes.is_empty()
                }
                crate::SourceState::Collected => {
                    self.retained_index_rows.is_some_and(|count| count > 0) && self.parent_prefix_complete
                }
                crate::SourceState::Partial => self.retained_index_rows.is_some(),
            }
            && self.indexes.iter().all(|index| index.parent_kind == MssqlIndexParentKind::IndexedView)
            && mssql_index_rows_are_consistent(self.indexes.iter())
    }
}

/// One acquired catalog population cannot contain two keys for one index or contradict its native parent row.
/// This is a point-consistency guard, not a cross-capture incarnation or continuity assertion.
pub fn mssql_index_rows_are_consistent<'a>(indexes: impl IntoIterator<Item = &'a MssqlIndexSnapshot>) -> bool {
    let mut keys = BTreeSet::new();
    let mut database = None;
    let mut parents = std::collections::BTreeMap::new();
    indexes.into_iter().all(|index| {
        let parent = (
            index.parent_kind,
            &index.parent_object,
            &index.parent_schema,
            &index.parent_name,
            &index.parent_modified_without_offset,
            index.parent_memory_optimized,
        );
        index.is_valid()
            && database
                .replace(index.parent_object.database_id)
                .is_none_or(|before| before == index.parent_object.database_id)
            && index.key().is_ok_and(|key| keys.insert(key))
            && parents.insert(index.parent_object.object_id, parent).is_none_or(|before| before == parent)
    })
}

#[cfg(test)]
mod tests;
