//! One selected native table's current XTP allocations. These are not row counts or pressure evidence.
use super::*;

pub const MSSQL_TABLE_MEMORY_SOURCE: &str = "memory-optimized table memory";

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct MssqlTableMemoryMetadata {
    pub observed_at: String,
    pub schema: String,
    pub name: String,
    #[serde(deserialize_with = "Option::deserialize")]
    pub memory_optimized: Option<bool>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub xtp_supported: Option<bool>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub dmv_permission: Option<bool>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub metadata_visible: Option<bool>,
    pub layout_available: bool,
}
impl MssqlTableMemoryMetadata {
    pub fn is_valid(&self) -> bool {
        ObservationBounds::new(&self.observed_at, &self.observed_at).is_some()
            && self.observed_at.len() <= 64
            && [&self.schema, &self.name].into_iter().all(|name| !name.is_empty() && name.len() <= 2048)
    }
    pub fn available(&self) -> bool {
        self.memory_optimized == Some(true)
            && self.xtp_supported == Some(true)
            && self.dmv_permission == Some(true)
            && self.metadata_visible == Some(true)
            && self.layout_available
    }
}

/// KiB quantities remain exact, independent nullable native bigint gauges; table-used includes row versions.
#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct MssqlTableMemory {
    pub table: MssqlObjectIdentity,
    pub observed_at: String,
    #[serde(deserialize_with = "Option::deserialize")]
    pub allocated_table_kibibytes: Option<String>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub used_table_kibibytes: Option<String>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub allocated_indexes_kibibytes: Option<String>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub used_indexes_kibibytes: Option<String>,
}
impl MssqlTableMemory {
    pub fn is_valid(&self) -> bool {
        self.table.is_valid()
            && self.table.database_id <= i32::MAX as u32
            && self.table.object_id <= i32::MAX as u32
            && self.observed_at.len() <= 64
            && ObservationBounds::new(&self.observed_at, &self.observed_at).is_some()
            && [
                &self.allocated_table_kibibytes,
                &self.used_table_kibibytes,
                &self.allocated_indexes_kibibytes,
                &self.used_indexes_kibibytes,
            ]
            .into_iter()
            .all(|value| {
                value.as_deref().is_none_or(|value| {
                    value.parse::<i64>().is_ok_and(|number| number >= 0 && number.to_string() == value)
                })
            })
    }
    pub fn key(&self) -> Result<String> {
        if !self.is_valid() {
            return Err(invalid());
        }
        self.table.key()
    }
    pub fn fits_source(&self, source: &CollectionDescription) -> bool {
        let Ok(context) = table_memory_context(source) else { return false };
        let SourceObservation::TableMemory { target, metadata, .. } = &source.observation else { return false };
        self.is_valid()
            && &self.table == target.as_ref()
            && metadata.available()
            && ObservationBounds::new(&context.observed_from, &context.observed_until)
                .is_some_and(|bounds| bounds.contains(&self.observed_at))
            && self.observed_at == metadata.observed_at
    }
}

pub fn table_memory_header_is_valid(snapshot: &StateSnapshotDescription) -> bool {
    snapshot.is_valid()
        && snapshot.dataset == SnapshotDataset::TableMemory
        && snapshot.records <= 1
        && matches!(snapshot.sources.as_slice(), [source] if source.source == MSSQL_TABLE_MEMORY_SOURCE
            && source.records == snapshot.records)
}

/// Original selected object, native source/clock and role only; no recursive CollectionDescription validation.
pub fn table_memory_context(source: &CollectionDescription) -> Result<NativeReadContext> {
    let SourceObservation::TableMemory { snapshot, target, metadata, database_writable } = &source.observation else {
        return Err(invalid());
    };
    let context = NativeReadContext {
        target: source.target.clone(),
        partition: source.partition.clone(),
        observed_from: snapshot.observed_from.clone(),
        observed_until: snapshot.observed_until.clone(),
        database_writable: Some(*database_writable),
    };
    if source.source != MSSQL_TABLE_MEMORY_SOURCE
        || source.continuity != SourceContinuity::Unverified
        || !table_memory_header_is_valid(snapshot)
        || !target.is_valid()
        || !metadata.is_valid()
        || target.database_id > i32::MAX as u32
        || target.object_id > i32::MAX as u32
        || !context.has_qualified_local_owner()
        || context.target.database().is_none()
        || context.target.engine() != crate::Engine::Mssql
        || !matches!(&context.partition, SourcePartition::InstanceLocal { member: Some(member), .. }
            if matches!(member.kind, SharedResourceKind::Instance | SharedResourceKind::Node))
        || !ObservationBounds::new(&context.observed_from, &context.observed_until)
            .is_some_and(|bounds| bounds.contains(&metadata.observed_at))
        || snapshot.sources[0].state == crate::SourceState::NotApplicable && metadata.memory_optimized != Some(false)
        || snapshot.records > 0 && !metadata.available()
        || matches!(snapshot.sources[0].state, crate::SourceState::Collected | crate::SourceState::Empty)
            && !metadata.available()
    {
        return Err(invalid());
    }
    Ok(context)
}
fn invalid() -> crate::Error {
    CollectionStop::InvalidPage.error()
}
#[cfg(test)]
mod tests;
