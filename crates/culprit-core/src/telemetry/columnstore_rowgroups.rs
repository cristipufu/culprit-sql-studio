//! Selected current catalog rows. Rowgroup numbers and HoBTs establish neither continuity nor reclaimable bytes.
use super::*;

pub const MSSQL_COLUMNSTORE_ROWGROUPS_SOURCE: &str = "columnstore rowgroups";
pub const MAX_COLUMNSTORE_ROWGROUPS: u32 = 128;

/// Native catalog selection within the explicitly chosen database route. The source must revalidate it;
/// this value alone authenticates neither an earlier capture nor a physical rowgroup generation.
#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct MssqlColumnstoreTarget {
    pub table: MssqlObjectIdentity,
    pub index_id: u32,
    pub table_modified_without_offset: String,
    #[serde(deserialize_with = "Option::deserialize")]
    pub partition_number: Option<u32>,
}
impl MssqlColumnstoreTarget {
    pub fn is_valid(&self) -> bool {
        self.table.is_valid()
            && self.table.database_id <= i32::MAX as u32
            && self.table.object_id <= i32::MAX as u32
            && (1..=i32::MAX as u32).contains(&self.index_id)
            && super::catalog::local_time(&self.table_modified_without_offset)
            && self.partition_number.is_none_or(|partition| (1..=i32::MAX as u32).contains(&partition))
    }
    pub fn key(&self) -> Result<String> {
        if !self.is_valid() {
            return Err(invalid());
        }
        serde_json::to_vec(self).map(|bytes| digest(&bytes)).map_err(|_| invalid())
    }
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct MssqlColumnstoreRowgroup {
    pub context: NativeReadContext,
    pub target: MssqlColumnstoreTarget,
    pub observed_at: String,
    pub partition_number: u32,
    /// Memory-optimized tails (-1) are outside this disk-based source.
    pub row_group_id: u32,
    pub index_type: u8,
    #[serde(deserialize_with = "Option::deserialize")]
    pub delta_store_hobt_id: Option<String>,
    /// Native state: 0 invisible, 1 open, 2 closed, 3 compressed, 4 tombstone.
    pub state: u8,
    /// Native bigint quantities remain exact strings. NULL is an unknown value, never a measured zero.
    #[serde(deserialize_with = "Option::deserialize")]
    pub total_rows: Option<String>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub deleted_rows: Option<String>,
    /// Excludes metadata and shared dictionaries. NCCI deleted rows also omit the separate delete buffer.
    #[serde(deserialize_with = "Option::deserialize")]
    pub size_bytes: Option<String>,
}
impl MssqlColumnstoreRowgroup {
    pub fn is_valid(&self) -> bool {
        rowgroup_owner_is_qualified(&self.context)
            && self.target.is_valid()
            && (1..=i32::MAX as u32).contains(&self.partition_number)
            && self.target.partition_number.is_none_or(|partition| partition == self.partition_number)
            && self.row_group_id <= i32::MAX as u32
            && matches!(self.index_type, 5 | 6)
            && self.state <= 4
            && [&self.delta_store_hobt_id, &self.total_rows, &self.deleted_rows, &self.size_bytes]
                .into_iter()
                .all(|value| value.as_deref().is_none_or(native_quantity))
            && self.total_rows.as_ref().zip(self.deleted_rows.as_ref()).is_none_or(|(total, deleted)| {
                total
                    .parse::<i64>()
                    .ok()
                    .zip(deleted.parse::<i64>().ok())
                    .is_some_and(|(total, deleted)| deleted <= total)
            })
            && ObservationBounds::new(&self.context.observed_from, &self.context.observed_until)
                .is_some_and(|bounds| bounds.contains(&self.observed_at))
    }
    pub fn key(&self) -> Result<String> {
        if !self.is_valid() {
            return Err(invalid());
        }
        serde_json::to_vec(&(&self.target, self.partition_number, self.row_group_id))
            .map(|bytes| digest(&bytes))
            .map_err(|_| invalid())
    }
    pub fn fits_source(&self, source: &CollectionDescription) -> bool {
        let SourceObservation::ColumnstoreRowgroups { target, index_type, .. } = &source.observation else {
            return false;
        };
        self.is_valid()
            && self.target == **target
            && self.index_type == *index_type
            && columnstore_rowgroup_context(source).is_ok_and(|context| context == self.context)
    }
}

pub fn columnstore_rowgroup_header_is_valid(snapshot: &StateSnapshotDescription) -> bool {
    snapshot.is_valid()
        && snapshot.dataset == SnapshotDataset::ColumnstoreRowgroups
        && snapshot.records <= MAX_COLUMNSTORE_ROWGROUPS
        && matches!(snapshot.sources.as_slice(), [source] if source.source == MSSQL_COLUMNSTORE_ROWGROUPS_SOURCE)
}

/// No recursive call into CollectionDescription validation; the common partition owner uses this same guard.
pub fn columnstore_rowgroup_context(source: &CollectionDescription) -> Result<NativeReadContext> {
    let SourceObservation::ColumnstoreRowgroups { snapshot, target, index_type, database_writable } =
        &source.observation
    else {
        return Err(invalid());
    };
    let context = NativeReadContext {
        target: source.target.clone(),
        partition: source.partition.clone(),
        observed_from: snapshot.observed_from.clone(),
        observed_until: snapshot.observed_until.clone(),
        database_writable: Some(*database_writable),
    };
    if source.source != MSSQL_COLUMNSTORE_ROWGROUPS_SOURCE
        || source.continuity != SourceContinuity::Unverified
        || !target.is_valid()
        || !matches!(*index_type, 5 | 6)
        || !columnstore_rowgroup_header_is_valid(snapshot)
        || !rowgroup_owner_is_qualified(&context)
    {
        return Err(invalid());
    }
    Ok(context)
}
fn rowgroup_owner_is_qualified(context: &NativeReadContext) -> bool {
    context.has_qualified_local_owner()
        && context.target.database().is_some()
        && context.target.engine() == crate::Engine::Mssql
        && context.database_writable.is_some()
        && matches!(&context.partition, SourcePartition::InstanceLocal { member: Some(member), .. }
            if matches!(member.kind, SharedResourceKind::Instance | SharedResourceKind::Node))
}
fn native_quantity(value: &str) -> bool {
    value.parse::<i64>().is_ok_and(|number| number >= 0 && number.to_string() == value)
}
fn invalid() -> Error {
    CollectionStop::InvalidPage.error()
}

#[cfg(test)]
mod tests;
