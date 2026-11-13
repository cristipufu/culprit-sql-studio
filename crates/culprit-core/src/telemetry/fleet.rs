//! Static fleet assignments and explicit collection policies. Scheduling never grants native read permission.
use super::*;

pub const MAX_FLEET_TARGETS: usize = 1_024;
pub const MAX_TARGET_SCHEDULES: usize = 32;
pub const MAX_HISTORY_LOOKBACK_SECONDS: u32 = MAX_INTERVAL_DISCOVERY_SECONDS;

#[derive(Serialize, Deserialize, TS, Clone, Debug)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct FleetInventory {
    pub targets: Vec<FleetTarget>,
}

#[derive(Serialize, Deserialize, TS, Clone, Debug)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct FleetTarget {
    /// Stable logical assignment, separate from its connection route and native resource identity.
    pub id: String,
    pub connection: String,
    pub subject: CollectionSubject,
    pub shard: String,
    /// Configured physical-resource group. All members must belong to the same static shard.
    /// This is an admission policy, never evidence that a native measurement has this scope.
    pub admission_group: String,
    pub schedules: Vec<FleetSchedule>,
}

#[derive(Serialize, Deserialize, TS, Clone, Debug)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct FleetSchedule {
    pub id: String,
    pub every_seconds: u32,
    /// Added to the minimum period; jitter never accelerates a configured cadence.
    pub jitter_seconds: u32,
    pub collection: ScheduledCollection,
    #[serde(default)]
    pub budget: CollectionBudget,
}

#[derive(Serialize, Deserialize, TS, Clone, Debug)]
#[serde(tag = "kind", rename_all = "kebab-case")]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub enum ScheduledCollection {
    QueryHistory {
        lookback_seconds: u32,
        reconcile_seconds: u32,
    },
    ResourceHistory {
        window_seconds: u32,
        overlap_seconds: u32,
    },
    DeadlockHistory {
        window_seconds: u32,
        overlap_seconds: u32,
    },
    Activity {
        #[serde(default)]
        policy: ActivityPolicy,
    },
    Snapshot {
        dataset: SnapshotDataset,
    },
}
impl ScheduledCollection {
    pub fn collection(&self) -> TelemetryCollection {
        match self {
            Self::QueryHistory { .. } => TelemetryCollection::QueryHistory,
            Self::ResourceHistory { .. } => TelemetryCollection::ResourceHistory,
            Self::DeadlockHistory { .. } => TelemetryCollection::DeadlockHistory,
            Self::Activity { .. } => TelemetryCollection::Activity,
            Self::Snapshot { dataset } => TelemetryCollection::Snapshot(*dataset),
        }
    }
}
impl FleetInventory {
    pub fn validate(&self, shard: &str) -> Result<()> {
        let mut targets = std::collections::BTreeSet::new();
        let mut owners = std::collections::BTreeSet::new();
        let mut groups = std::collections::BTreeMap::new();
        let mut routes = std::collections::BTreeMap::new();
        if !identifier(shard) || self.targets.is_empty() || self.targets.len() > MAX_FLEET_TARGETS {
            return Err(invalid_inventory());
        }
        let mut selected = false;
        for target in &self.targets {
            target.subject.validate()?;
            let assignment = (&target.shard, &target.admission_group);
            if !identifier(&target.id)
                || !identifier(&target.connection)
                || !identifier(&target.shard)
                || !identifier(&target.admission_group)
                || !targets.insert(&target.id)
                || !owners.insert(target.subject.assignment_key(&target.connection)?)
                || target.schedules.is_empty()
                || target.schedules.len() > MAX_TARGET_SCHEDULES
                || groups.insert(&target.admission_group, &target.shard).is_some_and(|owner| owner != &target.shard)
                || routes.insert(&target.connection, assignment).is_some_and(|owner| owner != assignment)
            {
                return Err(invalid_inventory());
            }
            selected |= target.shard == shard;
            let mut ids = std::collections::BTreeSet::new();
            let mut collections = Vec::new();
            for schedule in &target.schedules {
                schedule.validate()?;
                let collection = schedule.collection.collection();
                if !ids.insert(&schedule.id) || collections.contains(&collection) {
                    return Err(invalid_inventory());
                }
                // Native interval discovery is explicitly a database API. Shared histories require their own route.
                if collection == TelemetryCollection::QueryHistory
                    && matches!(target.subject, CollectionSubject::Shared { .. })
                {
                    return Err(invalid_inventory());
                }
                collections.push(collection);
            }
        }
        if !selected {
            return Err(invalid_inventory());
        }
        Ok(())
    }
}
impl FleetSchedule {
    pub fn validate(&self) -> Result<()> {
        if !identifier(&self.id)
            || !(MIN_ACTIVITY_POLL_SECONDS..=86_400).contains(&self.every_seconds)
            || self.jitter_seconds > self.every_seconds / 2
        {
            return Err(invalid_inventory());
        }
        self.budget.validate()?;
        match &self.collection {
            ScheduledCollection::QueryHistory { lookback_seconds, reconcile_seconds } => {
                if !(3_600..=MAX_HISTORY_LOOKBACK_SECONDS).contains(lookback_seconds)
                    || *reconcile_seconds < self.every_seconds
                    || reconcile_seconds > lookback_seconds
                {
                    return Err(invalid_inventory());
                }
            }
            ScheduledCollection::ResourceHistory { window_seconds, overlap_seconds }
            | ScheduledCollection::DeadlockHistory { window_seconds, overlap_seconds } => {
                if *window_seconds < self.every_seconds
                    || *window_seconds > MAX_HISTORY_WINDOW_SECONDS
                    || *overlap_seconds > *window_seconds
                    || window_seconds.saturating_add(*overlap_seconds) > MAX_HISTORY_WINDOW_SECONDS
                {
                    return Err(invalid_inventory());
                }
            }
            ScheduledCollection::Activity { policy } => policy.validate()?,
            ScheduledCollection::Snapshot { .. } => {}
        }
        Ok(())
    }
}
fn identifier(value: &str) -> bool {
    !value.is_empty() && value.len() <= 256 && !value.chars().any(char::is_control)
}
fn invalid_inventory() -> Error {
    Error::invalid(t!("fleet.invalid_inventory"))
}

#[derive(Serialize, Deserialize, TS, Clone, Debug)]
#[ts(export)]
pub struct FleetCollectionEvent {
    pub target: String,
    pub connection: String,
    pub schedule: String,
    pub work: FleetWork,
    pub status: CollectionStatus,
    pub capture: Option<CaptureIdentity>,
    pub error_code: Option<ErrorCode>,
    /// Failed or missed work remains visible even when no rows arrived at a destination.
    pub gap: Option<TelemetryRange>,
}

#[derive(Serialize, Deserialize, TS, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum FleetWork {
    DiscoverIntervals,
    Capture,
    SkipCommitted,
}

#[derive(Serialize, Deserialize, TS, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum FleetGapReason {
    BackfillLimit,
    NativeRetention,
}

/// Bounds identify work whose coverage is missing or incomplete, not a fabricated count of lost native events.
#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[ts(export)]
pub struct FleetGap {
    pub id: String,
    pub target: String,
    pub connection: String,
    pub schedule: String,
    pub range: TelemetryRange,
    pub reason: FleetGapReason,
    pub recorded_at: String,
    pub capture: Option<CaptureIdentity>,
}

#[derive(Serialize, Deserialize, TS, Clone, Debug)]
#[ts(export)]
pub struct FleetGapPage {
    pub gaps: Vec<FleetGap>,
    /// Monotonic local insertion position. New gaps cannot be lost behind a hash-sorted cursor.
    pub cursor: String,
    pub more: bool,
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, Default)]
#[ts(export)]
pub struct FleetSummary {
    pub attempts: u32,
    pub complete: u32,
    pub incomplete: u32,
}

#[cfg(test)]
mod tests;
