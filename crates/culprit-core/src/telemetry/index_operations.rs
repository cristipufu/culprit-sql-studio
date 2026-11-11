//! Native index counters are observations of metadata-cache lifetime, not a proven restart epoch.
use super::*;
use std::collections::BTreeMap;

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
#[ts(export)]
pub struct MssqlIndexIdentity {
    pub database_id: u32,
    pub object_id: u32,
    pub index_id: u32,
    pub partition_number: u32,
    /// Native internal columnstore rowset discriminator; NULL means this is not an internal columnstore rowset.
    pub hobt_id: Option<String>,
}
impl MssqlIndexIdentity {
    pub fn key(&self) -> Result<String> {
        serde_json::to_vec(self).map(|bytes| digest(&bytes)).map_err(|_| CollectionStop::InvalidPage.error())
    }
    fn is_valid(&self) -> bool {
        self.database_id > 0
            && self.object_id > 0
            && self.partition_number > 0
            && self.hobt_id.as_deref().is_none_or(valid_counter)
    }
}

#[derive(Serialize, Deserialize, TS, Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum MssqlIndexCounter {
    LeafGhosts,
    NonleafInserts,
    NonleafDeletes,
    NonleafUpdates,
    LeafPageMerges,
    NonleafPageMerges,
    LobFetchPages,
    LobFetchBytes,
    LobOrphansCreated,
    LobOrphansInserted,
    RowOverflowFetchPages,
    RowOverflowFetchBytes,
    ValuesPushedOffRow,
    ValuesPulledInRow,
    RowLocks,
    PageLocks,
    PageCompressionAttempts,
    PageCompressions,
    VersionsGeneratedInRow,
    VersionsGeneratedOffRow,
    GhostVersionsInRow,
    GhostVersionsOffRow,
    InsertOverGhostVersionsInRow,
    InsertOverGhostVersionsOffRow,
    CompactionAttempts,
    CompactionsCompleted,
    CompactionsSkipped,
    CompactionsIneligible,
    CompactionFailures,
    CompactionRowsMoved,
    CompactionPagesDeallocated,

    LeafInserts,
    LeafUpdates,
    LeafDeletes,
    LeafAllocations,
    NonleafAllocations,
    RangeScans,
    SingletonLookups,
    ForwardedFetches,
    RowLockWaits,
    RowLockWaitMs,
    PageLockWaits,
    PageLockWaitMs,
    PageLatchWaits,
    PageLatchWaitMs,
    PageIoLatchWaits,
    PageIoLatchWaitMs,
    TreePageLatchWaits,
    TreePageLatchWaitMs,
    TreePageIoLatchWaits,
    TreePageIoLatchWaitMs,
    LockEscalations,
    LockEscalationAttempts,
}
impl MssqlIndexCounter {
    pub fn unit(self) -> CounterUnit {
        match self {
            Self::RowLockWaitMs
            | Self::PageLockWaitMs
            | Self::PageLatchWaitMs
            | Self::PageIoLatchWaitMs
            | Self::TreePageLatchWaitMs
            | Self::TreePageIoLatchWaitMs => CounterUnit::Milliseconds,
            Self::LobFetchPages | Self::RowOverflowFetchPages => CounterUnit::Pages { bytes_per_page: 8192 },
            Self::LobFetchBytes | Self::RowOverflowFetchBytes => CounterUnit::Bytes,
            _ => CounterUnit::Count,
        }
    }
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[ts(export)]
pub struct MssqlIndexOperations {
    pub identity: MssqlIndexIdentity,
    pub schema: Option<String>,
    pub object: Option<String>,
    pub index: Option<String>,
    pub object_type: Option<String>,
    pub engine_internal: Option<bool>,
    pub leading_key_is_identity: Option<bool>,
    pub leading_key_default: Option<crate::sql::DefaultGenerator>,
    pub optimize_for_sequential_key: Option<bool>,
    /// An absent key is an unavailable native column; a present NULL remains an unknown measurement.
    pub counters: BTreeMap<MssqlIndexCounter, Option<String>>,
    pub reset: crate::ResetMarker,
}
impl MssqlIndexOperations {
    pub fn is_valid(&self) -> bool {
        self.identity.is_valid()
            && [&self.schema, &self.object, &self.index, &self.object_type]
                .into_iter()
                .all(|name| name.as_deref().is_none_or(|name| !name.is_empty() && name.len() <= 2048))
            && self.counters.values().flatten().all(|value| valid_counter(value))
            && matches!(self.reset, crate::ResetMarker::Unknown)
    }
    pub fn counter(&self, kind: MssqlIndexCounter) -> Option<&str> {
        self.counters.get(&kind).and_then(|value| value.as_deref())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn index_counter_identity_preserves_partition_rowset_and_unknown_reset() {
        let mut row = MssqlIndexOperations {
            identity: MssqlIndexIdentity {
                database_id: 5,
                object_id: 100,
                index_id: 0,
                partition_number: 1,
                hobt_id: Some("9007199254740993".into()),
            },
            schema: None,
            object: None,
            index: None,
            object_type: None,
            engine_internal: None,
            leading_key_is_identity: None,
            leading_key_default: None,
            optimize_for_sequential_key: None,
            counters: BTreeMap::from([
                (MssqlIndexCounter::ForwardedFetches, Some("0".into())),
                (MssqlIndexCounter::RowLockWaitMs, None),
            ]),
            reset: crate::ResetMarker::Unknown,
        };
        assert!(row.is_valid());
        assert_eq!(row.counter(MssqlIndexCounter::ForwardedFetches), Some("0"));
        assert_eq!(row.counter(MssqlIndexCounter::RowLockWaitMs), None);
        let key = row.identity.key().unwrap();
        row.identity.partition_number = 2;
        assert_ne!(row.identity.key().unwrap(), key);
        row.identity.partition_number = 1;
        row.identity.hobt_id = None;
        assert_ne!(row.identity.key().unwrap(), key);
        assert!(row.is_valid());
        row.reset = crate::ResetMarker::At("2020-01-01T00:00:00Z".into());
        assert!(!row.is_valid(), "server startup does not establish this metadata-cache lifetime");
    }
}
