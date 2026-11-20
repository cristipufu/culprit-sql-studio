//! Selected current populations, not a full-text history, freshness verdict or continuous index incarnation.
use super::*;

pub const MSSQL_FULL_TEXT_POPULATIONS_SOURCE: &str = "full-text populations";
pub const MAX_FULL_TEXT_POPULATIONS: u32 = 64;

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct MssqlFullTextPopulationMetadata {
    pub observed_at: String,
    #[serde(deserialize_with = "Option::deserialize")]
    pub schema: Option<String>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub table: Option<String>,
}
impl MssqlFullTextPopulationMetadata {
    pub fn is_valid(&self) -> bool {
        canonical_time(&self.observed_at).is_ok()
            && [&self.schema, &self.table]
                .into_iter()
                .all(|name| name.as_deref().is_none_or(|name| !name.is_empty() && name.len() <= 512))
    }
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct MssqlFullTextPopulationTarget {
    pub table: MssqlObjectIdentity,
    pub table_modified_without_offset: String,
    pub catalog_id: u32,
    pub unique_index_id: u32,
}
impl MssqlFullTextPopulationTarget {
    pub fn is_valid(&self) -> bool {
        self.table.is_valid()
            && [self.table.database_id, self.table.object_id, self.catalog_id, self.unique_index_id]
                .into_iter()
                .all(|id| (1..=i32::MAX as u32).contains(&id))
            && super::catalog::local_time(&self.table_modified_without_offset)
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
pub struct MssqlFullTextPopulation {
    pub context: NativeReadContext,
    pub target: MssqlFullTextPopulationTarget,
    pub observed_at: String,
    /// Position in this original native snapshot, never a stable event/population identity.
    pub position: u32,
    #[serde(deserialize_with = "Option::deserialize")]
    pub population_type: Option<i32>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub population_type_description: Option<String>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub status: Option<i32>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub status_description: Option<String>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub completion_type: Option<i32>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub completion_type_description: Option<String>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub queued_population_type: Option<i32>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub queued_population_type_description: Option<String>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub clustered_index_scan: Option<bool>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub range_count: Option<u32>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub completed_range_count: Option<u32>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub outstanding_batch_count: Option<u32>,
    /// Native datetime without an offset. This is not acquisition UTC or a proven elapsed duration.
    #[serde(deserialize_with = "Option::deserialize")]
    pub started_without_offset: Option<String>,
}
impl MssqlFullTextPopulation {
    pub fn is_valid(&self) -> bool {
        full_text_population_owner_is_qualified(&self.context)
            && self.target.is_valid()
            && (1..=MAX_FULL_TEXT_POPULATIONS).contains(&self.position)
            && [self.range_count, self.completed_range_count, self.outstanding_batch_count]
                .into_iter()
                .flatten()
                .all(|value| value <= i32::MAX as u32)
            && [
                &self.population_type_description,
                &self.status_description,
                &self.completion_type_description,
                &self.queued_population_type_description,
            ]
            .into_iter()
            .all(|value| value.as_deref().is_none_or(|text| text.len() <= 512 && !text.chars().any(char::is_control)))
            && self.started_without_offset.as_deref().is_none_or(super::catalog::local_time)
            && ObservationBounds::new(&self.context.observed_from, &self.context.observed_until)
                .is_some_and(|bounds| bounds.contains(&self.observed_at))
    }
    pub fn key(&self) -> Result<String> {
        if !self.is_valid() {
            return Err(invalid());
        }
        serde_json::to_vec(&(&self.target, self.position)).map(|bytes| digest(&bytes)).map_err(|_| invalid())
    }
    pub fn fits_source(&self, source: &CollectionDescription) -> bool {
        let SourceObservation::FullTextPopulations { target, metadata, .. } = &source.observation else { return false };
        self.is_valid()
            && self.target == **target
            && self.observed_at == metadata.observed_at
            && full_text_population_context(source).is_ok_and(|context| context == self.context)
    }
}
pub fn full_text_population_header_is_valid(
    snapshot: &StateSnapshotDescription,
    metadata: &MssqlFullTextPopulationMetadata,
) -> bool {
    snapshot.is_valid()
        && metadata.is_valid()
        && snapshot.dataset == SnapshotDataset::FullTextPopulations
        && ObservationBounds::new(&snapshot.observed_from, &snapshot.observed_until)
            .is_some_and(|bounds| bounds.contains(&metadata.observed_at))
        && snapshot.records <= MAX_FULL_TEXT_POPULATIONS
        && matches!(snapshot.sources.as_slice(), [coverage] if coverage.source == MSSQL_FULL_TEXT_POPULATIONS_SOURCE)
}
pub fn full_text_population_context(source: &CollectionDescription) -> Result<NativeReadContext> {
    let SourceObservation::FullTextPopulations { snapshot, target, metadata, database_writable } = &source.observation
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
    if source.source != MSSQL_FULL_TEXT_POPULATIONS_SOURCE
        || source.continuity != SourceContinuity::Unverified
        || !target.is_valid()
        || !full_text_population_header_is_valid(snapshot, metadata)
        || !full_text_population_owner_is_qualified(&context)
    {
        return Err(invalid());
    }
    Ok(context)
}
fn full_text_population_owner_is_qualified(context: &NativeReadContext) -> bool {
    context.has_qualified_local_owner()
        && context.target.database().is_some()
        && context.target.engine() == crate::Engine::Mssql
        && context.database_writable.is_some()
        && matches!(&context.partition, SourcePartition::InstanceLocal { member: Some(member), .. }
            if matches!(member.kind, SharedResourceKind::Instance | SharedResourceKind::Node))
}
fn invalid() -> crate::Error {
    CollectionStop::InvalidPage.error()
}
#[cfg(test)]
mod tests;
