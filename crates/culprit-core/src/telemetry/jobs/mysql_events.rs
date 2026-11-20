//! Native event definitions and last starts are points, never successful-run history or current workers.
use super::*;

pub const MYSQL_SCHEDULED_EVENTS_SOURCE: &str = "mysql-scheduled-events";
pub const MAX_MYSQL_SCHEDULED_EVENTS: usize = 128;

#[derive(Serialize, Deserialize, TS, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum MysqlEventSchedulerSetting {
    On,
    Off,
    Disabled,
    Unknown,
}

#[derive(Serialize, Deserialize, TS, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum MysqlEventStatus {
    Enabled,
    Disabled,
    ReplicaSideDisabled,
    SlavesideDisabled,
    Unknown,
}

#[derive(Serialize, Deserialize, TS, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum MysqlEventType {
    OneTime,
    Recurring,
    Unknown,
}

#[derive(Serialize, Deserialize, TS, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum MysqlEventCatalogLayout {
    Available,
    Unsupported,
    PermissionDenied,
    ReadFailed,
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct MysqlEventsMetadata {
    /// This is the global configured value. ON does not establish that a scheduler thread is running.
    pub scheduler_setting: Option<MysqlEventSchedulerSetting>,
    pub scheduler_observed_at: String,
    /// Direct native global/schema EVENT privileges prove visibility; role-only grants remain unknown.
    pub database_event_visibility: bool,
    pub layout: MysqlEventCatalogLayout,
    /// Retained native local fields keep the original event and catalog-session zone context; no UTC conversion.
    pub session_time_zone: Option<String>,
    pub database_writable: bool,
}
impl MysqlEventsMetadata {
    pub fn matches_snapshot(&self, snapshot: &StateSnapshotDescription) -> bool {
        snapshot.is_valid()
            && snapshot.dataset == SnapshotDataset::Jobs
            && snapshot.records as usize <= MAX_MYSQL_SCHEDULED_EVENTS
            && self.session_time_zone.as_ref().is_none_or(|zone| valid_key(zone) && zone.len() <= 128)
            && ObservationBounds::new(&snapshot.observed_from, &snapshot.observed_until)
                .is_some_and(|bounds| bounds.contains(&self.scheduler_observed_at))
            && matches!(snapshot.sources.as_slice(),[coverage]
                if coverage.source == MYSQL_SCHEDULED_EVENTS_SOURCE
                && (!coverage.complete_scope || self.database_event_visibility && self.layout == MysqlEventCatalogLayout::Available)
                && (self.layout == MysqlEventCatalogLayout::Available
                    || coverage.records == 0 && coverage.state == crate::SourceState::Unavailable))
    }
    pub fn matches_source(&self, source: &CollectionDescription, snapshot: &StateSnapshotDescription) -> bool {
        let owner = NativeReadContext {
            target: source.target.clone(),
            partition: source.partition.clone(),
            observed_from: snapshot.observed_from.clone(),
            observed_until: snapshot.observed_until.clone(),
            database_writable: Some(self.database_writable),
        };
        self.matches_snapshot(snapshot)
            && source.source == MYSQL_SCHEDULED_EVENTS_SOURCE
            && source.continuity == SourceContinuity::Unverified
            && owner.target.engine() == crate::Engine::Mysql
            && owner.target.database().is_some()
            && owner.has_qualified_local_owner()
            && matches!(&owner.partition,SourcePartition::InstanceLocal{member:Some(member),..} if member.kind==SharedResourceKind::Instance)
    }
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct MysqlScheduledEvent {
    pub observed_at: String,
    pub schema: String,
    pub name: String,
    pub status: Option<MysqlEventStatus>,
    pub event_type: Option<MysqlEventType>,
    pub time_zone: Option<String>,
    pub interval_value: Option<String>,
    pub interval_field: Option<String>,
    pub execute_at_without_offset: Option<String>,
    pub starts_without_offset: Option<String>,
    pub ends_without_offset: Option<String>,
    /// Native LAST_EXECUTED records a start. NULL supplies neither a success nor a failure outcome.
    pub last_started_without_offset: Option<String>,
    pub created_without_offset: Option<String>,
    pub altered_without_offset: Option<String>,
}
impl MysqlScheduledEvent {
    pub fn is_valid(&self) -> bool {
        canonical_time(&self.observed_at).is_ok()
            && label(&self.schema, 256)
            && label(&self.name, 256)
            && self.time_zone.as_ref().is_none_or(|zone| valid_key(zone) && zone.len() <= 128)
            && self.interval_value.as_ref().is_none_or(|value| label(value, 256))
            && self.interval_field.as_ref().is_none_or(|value| label(value, 64))
            && [
                &self.execute_at_without_offset,
                &self.starts_without_offset,
                &self.ends_without_offset,
                &self.last_started_without_offset,
                &self.created_without_offset,
                &self.altered_without_offset,
            ]
            .into_iter()
            .all(|time| time.as_ref().is_none_or(|time| super::super::catalog::local_time(time)))
    }
    /// Uniqueness inside one original catalog read only; names/coarse creation times are no incarnation.
    pub fn key(&self) -> Result<String> {
        if !self.is_valid() {
            return Err(invalid());
        }
        serde_json::to_vec(&(&self.schema, &self.name)).map(|bytes| digest(&bytes)).map_err(|_| invalid())
    }
    pub fn understood(&self) -> bool {
        self.status.is_some_and(|status| status != MysqlEventStatus::Unknown)
            && self.event_type.is_some_and(|kind| kind != MysqlEventType::Unknown)
    }
    pub fn fits_source(&self, source: &CollectionDescription) -> bool {
        let SourceObservation::MysqlEvents { snapshot, metadata } = &source.observation else { return false };
        self.is_valid()
            && metadata.matches_source(source, snapshot)
            && metadata.layout == MysqlEventCatalogLayout::Available
            && source.target.database().is_some_and(|database| database.database == self.schema)
            && ObservationBounds::new(&snapshot.observed_from, &snapshot.observed_until)
                .is_some_and(|bounds| bounds.contains(&self.observed_at))
    }
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct MysqlEventsSnapshot {
    pub context: NativeReadContext,
    pub metadata: MysqlEventsMetadata,
    pub sources: Vec<StateSourceCoverage>,
    pub observations: Vec<OperationalObservation>,
}
impl MysqlEventsSnapshot {
    pub fn from_source(source: &CollectionDescription, observations: Vec<OperationalObservation>) -> Result<Self> {
        let SourceObservation::MysqlEvents { snapshot, metadata } = &source.observation else { return Err(invalid()) };
        if !metadata.matches_source(source, snapshot) {
            return Err(invalid());
        }
        let read = Self {
            context: NativeReadContext::from_snapshot(source)?,
            metadata: metadata.as_ref().clone(),
            sources: snapshot.sources.clone(),
            observations,
        };
        read.is_valid_read(false).then_some(read).ok_or_else(invalid)
    }
    pub fn is_valid_read(&self, read_complete: bool) -> bool {
        let [coverage] = self.sources.as_slice() else { return false };
        let snapshot = StateSnapshotDescription {
            dataset: SnapshotDataset::Jobs,
            observed_from: self.context.observed_from.clone(),
            observed_until: self.context.observed_until.clone(),
            records: coverage.records,
            sources: self.sources.clone(),
        };
        let source = CollectionDescription {
            target: self.context.target.clone(),
            partition: self.context.partition.clone(),
            source: MYSQL_SCHEDULED_EVENTS_SOURCE.into(),
            continuity: SourceContinuity::Unverified,
            observation: SourceObservation::MysqlEvents {
                snapshot: snapshot.clone(),
                metadata: Box::new(self.metadata.clone()),
            },
        };
        if self.context.database_writable != Some(self.metadata.database_writable)
            || !self.metadata.matches_source(&source, &snapshot)
            || self.observations.len() > MAX_MYSQL_SCHEDULED_EVENTS
            || self.observations.len() > coverage.records as usize
            || read_complete && self.observations.len() != coverage.records as usize
        {
            return false;
        }
        let mut keys = BTreeSet::new();
        self.observations.iter().all(|row| {
            let OperationalFact::MysqlScheduledEvent(event) = &row.fact else { return false };
            row.is_valid()
                && event.fits_source(&source)
                && keys.insert(&row.key)
                && matches!(coverage.state, crate::SourceState::Collected | crate::SourceState::Partial)
                && (!coverage.complete_scope || event.understood())
        })
    }
}

#[cfg(test)]
mod tests;
