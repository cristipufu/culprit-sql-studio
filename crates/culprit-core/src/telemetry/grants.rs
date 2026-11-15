//! SQL Server's retained statement-cache counters. Entry identity does not establish continuity after eviction.
use super::*;

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[ts(export)]
pub struct StatementGrantStatistics {
    pub context: CachedStatementContext,
    /// Only a complete, bounded native statement is available. Missing or module-prefix-only text stays explicit.
    pub statement: DefinitionPayload,
    pub available_columns: Vec<String>,
    pub executions: Option<String>,
    pub last_execution_without_offset: Option<String>,
    pub total_grant_kibibytes: Option<String>,
    pub total_used_kibibytes: Option<String>,
    pub last_grant_kibibytes: Option<String>,
    pub last_used_kibibytes: Option<String>,
    pub min_grant_kibibytes: Option<String>,
    pub min_used_kibibytes: Option<String>,
    pub max_grant_kibibytes: Option<String>,
    pub max_used_kibibytes: Option<String>,
    pub reset: crate::ResetMarker,
}
impl StatementGrantStatistics {
    pub fn is_valid(&self) -> bool {
        let mut columns = std::collections::BTreeSet::new();
        let counters = [
            &self.total_grant_kibibytes,
            &self.total_used_kibibytes,
            &self.last_grant_kibibytes,
            &self.last_used_kibibytes,
            &self.min_grant_kibibytes,
            &self.min_used_kibibytes,
            &self.max_grant_kibibytes,
            &self.max_used_kibibytes,
        ];
        self.context.is_valid()
            && self.statement.is_valid()
            && (!self.statement.is_available()
                || self.context.identity.has_plan_handle()
                    && self.context.identity.has_sql_handle()
                    && self.context.source_text_complete
                    && self
                        .context
                        .source_text_bytes
                        .as_deref()
                        .is_some_and(|bytes| bytes.parse::<u64>().is_ok_and(|bytes| bytes > 0)))
            && self.executions.as_deref().is_none_or(valid_counter)
            && self.executions.is_some() == self.context.identity.has_plan_handle()
            && self.available_columns.len() <= 128
            && self.available_columns.iter().all(|name| valid_key(name) && columns.insert(name))
            && counters.into_iter().flatten().all(|value| valid_counter(value))
            && (self.context.identity.has_plan_handle() || counters.iter().all(|value| value.is_none()))
            && self.last_execution_without_offset.as_deref().is_none_or(|v| {
                v.len() <= 64 && chrono::NaiveDateTime::parse_from_str(v, "%Y-%m-%dT%H:%M:%S%.f").is_ok()
            })
            && valid_reset(&self.reset)
    }
}

/// Independent original reads are retained separately, including source coverage and native observation bounds.
#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[ts(export)]
pub struct StatementGrantSnapshot {
    /// Caller-owned read identity, distinct from native member/epoch authority. Archive reads use their capture ID.
    pub capture: CaptureIdentity,
    pub description: CollectionDescription,
    pub observations: Vec<StatementGrantStatistics>,
}

impl StatementGrantSnapshot {
    pub fn is_valid(&self) -> bool {
        self.is_valid_read(true)
    }

    /// A verified archive read can retain fewer decoded rows than its immutable native descriptor. Read gaps are
    /// supplied independently; a complete read still requires exact membership counts.
    pub fn is_valid_read(&self, read_complete: bool) -> bool {
        let Some(snapshot) = self.snapshot() else { return false };
        let mut identities = std::collections::BTreeSet::new();
        self.valid_description()
            && self.observations.len() <= CollectionBudget::upper_limits().max_snapshot_rows as usize
            && self.observations.len() <= snapshot.records as usize
            && (!read_complete || snapshot.records as usize == self.observations.len())
            && self
                .observations
                .iter()
                .all(|row| row.is_valid() && row.context.identity.key().is_ok_and(|key| identities.insert(key)))
    }

    /// Exact read reference. Display language and changing metric values do not determine this key.
    pub fn key(&self) -> Result<String> {
        if !self.valid_description() {
            return Err(CollectionStop::InvalidPage.error());
        }
        let snapshot = self.snapshot().ok_or_else(|| CollectionStop::InvalidPage.error())?;
        let from = canonical_time(&snapshot.observed_from)?;
        let until = canonical_time(&snapshot.observed_until)?;
        self.digest_key(&(
            &self.capture,
            &self.description.target,
            &self.description.partition,
            &self.description.source,
            snapshot.dataset,
            from,
            until,
        ))
    }

    /// An opaque analysis namespace. Unknown members are isolated to this read; this ID itself grants no authority.
    pub fn scope_key(&self) -> Result<String> {
        if !self.valid_description() {
            return Err(CollectionStop::InvalidPage.error());
        }
        if !self.description.is_partition_qualified() {
            return self.key();
        }
        self.digest_key(&(&self.description.target, &self.description.partition, &self.description.source))
    }

    pub fn entry_key(&self, row: &StatementGrantStatistics) -> Result<String> {
        if !row.is_valid() {
            return Err(CollectionStop::InvalidPage.error());
        }
        self.digest_key(&(self.scope_key()?, row.context.database_id, row.context.identity.key()?))
    }

    fn snapshot(&self) -> Option<&StateSnapshotDescription> {
        match &self.description.observation {
            SourceObservation::Snapshot(snapshot) if snapshot.dataset == SnapshotDataset::StatementGrants => {
                Some(snapshot)
            }
            _ => None,
        }
    }

    fn valid_description(&self) -> bool {
        let Some(snapshot) = self.snapshot() else { return false };
        valid_key(&self.capture.observation_id)
            && self.description.target.is_valid()
            && valid_key(&self.description.source)
            && matches!(self.description.partition, SourcePartition::InstanceLocal { .. })
            && self.description.partition_is_consistent()
            && snapshot.is_valid()
            && snapshot.sources.len() == 1
            && snapshot.sources[0].source == self.description.source
    }

    fn digest_key(&self, value: &impl Serialize) -> Result<String> {
        serde_json::to_vec(value).map(|bytes| digest(&bytes)).map_err(|_| CollectionStop::InvalidPage.error())
    }
}

#[cfg(test)]
mod tests;
