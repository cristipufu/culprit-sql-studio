//! Original MySQL Group Replication points. A group view is not a counter-reset epoch.
use super::*;

pub const MYSQL_GROUP_REPLICATION_SOURCE: &str = "mysql-group-replication";
pub const MYSQL_GROUP_MEMBERS_SOURCE: &str = "group-replication-members";
pub const MYSQL_GROUP_STATISTICS_SOURCE: &str = "group-replication-statistics";
pub const MYSQL_GROUP_MAX_MEMBERS: usize = 128;

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct MysqlGroupReplicationMetadata {
    pub observed_at: String,
    #[serde(deserialize_with = "Option::deserialize")]
    pub group_id: Option<String>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub plugin_status: Option<String>,
    pub members_layout: bool,
    pub statistics_layout: bool,
}
impl MysqlGroupReplicationMetadata {
    pub fn is_valid(&self) -> bool {
        clock(&self.observed_at)
            && self.group_id.as_deref().is_none_or(valid_key)
            && self.plugin_status.as_deref().is_none_or(valid_key)
    }
    pub fn same_configuration(&self, other: &Self) -> bool {
        let Self { observed_at: _, group_id, plugin_status, members_layout, statistics_layout } = self;
        group_id == &other.group_id
            && plugin_status == &other.plugin_status
            && members_layout == &other.members_layout
            && statistics_layout == &other.statistics_layout
    }
    pub fn available(&self, source: &str) -> bool {
        self.plugin_status.as_deref() == Some("ACTIVE")
            && self.group_id.is_some()
            && match source {
                MYSQL_GROUP_MEMBERS_SOURCE => self.members_layout,
                MYSQL_GROUP_STATISTICS_SOURCE => self.statistics_layout,
                _ => false,
            }
    }
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct MysqlGroupMember {
    pub group_id: String,
    pub member_id: String,
    pub channel: String,
    pub observed_at: String,
    pub state: String,
    #[serde(deserialize_with = "Option::deserialize")]
    pub role: Option<String>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub version: Option<String>,
}

/// Exact native integers. Queues/rows-validating are gauges; the other totals have unknown reset history.
#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct MysqlGroupStatistics {
    pub group_id: String,
    pub member_id: String,
    pub channel: String,
    pub observed_at: String,
    #[serde(deserialize_with = "Option::deserialize")]
    pub view_id: Option<String>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub certification_queue: Option<String>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub checked: Option<String>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub conflicts: Option<String>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub rows_validating: Option<String>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub applier_queue: Option<String>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub remote_applied: Option<String>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub local_proposed: Option<String>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub local_rollback: Option<String>,
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[serde(tag = "kind", content = "data", rename_all = "kebab-case", deny_unknown_fields)]
#[ts(export)]
pub enum MysqlGroupReplicationPoint {
    Member(Box<MysqlGroupMember>),
    Statistics(Box<MysqlGroupStatistics>),
}
impl MysqlGroupReplicationPoint {
    pub fn source(&self) -> &'static str {
        match self {
            Self::Member(_) => MYSQL_GROUP_MEMBERS_SOURCE,
            Self::Statistics(_) => MYSQL_GROUP_STATISTICS_SOURCE,
        }
    }
    pub fn group_id(&self) -> &str {
        match self {
            Self::Member(row) => &row.group_id,
            Self::Statistics(row) => &row.group_id,
        }
    }
    pub fn member_id(&self) -> &str {
        match self {
            Self::Member(row) => &row.member_id,
            Self::Statistics(row) => &row.member_id,
        }
    }
    pub fn channel(&self) -> &str {
        match self {
            Self::Member(row) => &row.channel,
            Self::Statistics(row) => &row.channel,
        }
    }
    pub fn observed_at(&self) -> &str {
        match self {
            Self::Member(row) => &row.observed_at,
            Self::Statistics(row) => &row.observed_at,
        }
    }
    pub fn is_valid(&self) -> bool {
        valid_key(self.group_id())
            && valid_key(self.member_id())
            && valid_key(self.channel())
            && clock(self.observed_at())
            && match self {
                Self::Member(row) => {
                    valid_key(&row.state)
                        && row.role.as_deref().is_none_or(valid_key)
                        && row.version.as_deref().is_none_or(valid_key)
                }
                Self::Statistics(row) => {
                    row.view_id.as_deref().is_none_or(valid_key)
                        && [
                            &row.certification_queue,
                            &row.checked,
                            &row.conflicts,
                            &row.rows_validating,
                            &row.applier_queue,
                            &row.remote_applied,
                            &row.local_proposed,
                            &row.local_rollback,
                        ]
                        .into_iter()
                        .flatten()
                        .all(|value| value.parse::<u64>().is_ok_and(|n| n.to_string() == *value))
                }
            }
    }
    pub fn key(&self) -> Result<String> {
        if !self.is_valid() {
            return Err(invalid());
        }
        Ok(digest(
            &serde_json::to_vec(&(self.source(), self.group_id(), self.channel(), self.member_id()))
                .map_err(|_| invalid())?,
        ))
    }
    pub fn fits_source(&self, source: &CollectionDescription) -> bool {
        let Ok(context) = group_replication_context(source) else { return false };
        let SourceObservation::GroupReplication { metadata, .. } = &source.observation else { return false };
        self.is_valid()
            && metadata.available(self.source())
            && metadata.group_id.as_deref() == Some(self.group_id())
            && ObservationBounds::new(&context.observed_from, &context.observed_until)
                .is_some_and(|bounds| bounds.contains(self.observed_at()))
    }
}

pub fn group_replication_header_is_valid(snapshot: &StateSnapshotDescription) -> bool {
    snapshot.is_valid()
        && snapshot.dataset == SnapshotDataset::GroupReplication
        && snapshot.records <= (MYSQL_GROUP_MAX_MEMBERS * 2) as u32
        && snapshot.sources.len() == 2
        && snapshot.sources.iter().all(|row| {
            matches!(row.source.as_str(), MYSQL_GROUP_MEMBERS_SOURCE | MYSQL_GROUP_STATISTICS_SOURCE)
                && row.records <= MYSQL_GROUP_MAX_MEMBERS as u32
        })
        && snapshot.sources[0].source != snapshot.sources[1].source
}
pub fn group_replication_context(source: &CollectionDescription) -> Result<NativeReadContext> {
    let SourceObservation::GroupReplication { snapshot, metadata } = &source.observation else { return Err(invalid()) };
    let context = NativeReadContext {
        target: source.target.clone(),
        partition: source.partition.clone(),
        observed_from: snapshot.observed_from.clone(),
        observed_until: snapshot.observed_until.clone(),
        database_writable: None,
    };
    if source.source != MYSQL_GROUP_REPLICATION_SOURCE
        || source.continuity != SourceContinuity::Unverified
        || !group_replication_header_is_valid(snapshot)
        || !metadata.is_valid()
        || !context.has_qualified_local_owner()
        || !matches!((&context.target, &context.partition),
            (CollectionTarget::Shared(resource), SourcePartition::InstanceLocal { member: Some(member), .. })
            if resource == member && resource.engine == crate::Engine::Mysql && resource.kind == SharedResourceKind::Instance)
        || !ObservationBounds::new(&context.observed_from, &context.observed_until)
            .is_some_and(|bounds| bounds.contains(&metadata.observed_at))
        || snapshot.sources.iter().any(|row| {
            (row.records > 0 || matches!(row.state, crate::SourceState::Collected | crate::SourceState::Empty))
                && !metadata.available(&row.source)
        })
    {
        return Err(invalid());
    }
    Ok(context)
}
fn clock(value: &str) -> bool {
    value.len() <= 64 && ObservationBounds::new(value, value).is_some()
}
fn invalid() -> crate::Error {
    CollectionStop::InvalidPage.error()
}
#[cfg(test)]
mod tests;
