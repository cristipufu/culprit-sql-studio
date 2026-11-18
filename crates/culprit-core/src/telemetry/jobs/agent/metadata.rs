use super::*;
#[derive(Serialize, Deserialize, TS, Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum MssqlAgentObject {
    Jobs,
    Steps,
    History,
    Activity,
    Sessions,
    Services,
}
impl MssqlAgentObject {
    pub fn native_name(self) -> &'static str {
        match self {
            Self::Jobs => "msdb.dbo.sysjobs",
            Self::Steps => "msdb.dbo.sysjobsteps",
            Self::History => "msdb.dbo.sysjobhistory",
            Self::Activity => "msdb.dbo.sysjobactivity",
            Self::Sessions => "msdb.dbo.syssessions",
            Self::Services => "sys.dm_server_services",
        }
    }
}
#[derive(Serialize, Deserialize, TS, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum MssqlAgentAccess {
    Available,
    PermissionDenied,
    Unreported,
    UnsupportedColumns,
    NotApplicable,
}
#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct MssqlAgentCapability {
    pub object: MssqlAgentObject,
    pub access: MssqlAgentAccess,
    pub columns_sha256: Option<String>,
    pub object_id: Option<String>,
}
#[derive(Serialize, Deserialize, TS, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum MssqlAgentAvailability {
    Supported,
    NotApplicable,
}
#[derive(Serialize, Deserialize, TS, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum MssqlAgentVisibility {
    Sysadmin,
    ExplicitSelect,
    Unknown,
}
#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
#[ts(export)]
pub enum MssqlAgentMetadataRecheck {
    NotRead,
    Unchanged,
    Changed,
    Unavailable { error_code: crate::ErrorCode },
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct MssqlAgentJobsMetadata {
    pub engine_edition: u32,
    pub major_version: u32,
    pub availability: MssqlAgentAvailability,
    pub visibility: MssqlAgentVisibility,
    pub capabilities: Vec<MssqlAgentCapability>,
    /// A bounded retained tail, never a promised retention period or complete execution history.
    pub history_limit: u32,
    /// Preserve the original discovery used to interpret the rows; final denial never rewrites that original.
    pub recheck: MssqlAgentMetadataRecheck,
}
impl MssqlAgentJobsMetadata {
    pub fn capability(&self, object: MssqlAgentObject) -> Option<&MssqlAgentCapability> {
        self.capabilities.iter().find(|capability| capability.object == object)
    }
    pub fn available(&self, object: MssqlAgentObject) -> bool {
        self.capability(object).is_some_and(|capability| capability.access == MssqlAgentAccess::Available)
    }
    pub fn is_valid(&self) -> bool {
        let mut objects = BTreeSet::new();
        self.major_version >= 11
            && (1..=MAX_MSSQL_AGENT_JOB_OBSERVATIONS as u32).contains(&self.history_limit)
            && self.capabilities.len() == 6
            && self.capabilities.iter().all(|capability| {
                objects.insert(capability.object)
                    && capability.columns_sha256.as_ref().is_none_or(|hash| valid_digest(hash))
                    && capability.object_id.as_ref().is_none_or(|value| nonzero_i32(value))
                    && (capability.access != MssqlAgentAccess::Available
                        || capability.columns_sha256.is_some() && capability.object_id.is_some())
            })
            && match self.availability {
                MssqlAgentAvailability::Supported => {
                    [2, 3, 8].contains(&self.engine_edition)
                        && self
                            .capabilities
                            .iter()
                            .all(|capability| capability.access != MssqlAgentAccess::NotApplicable)
                }
                MssqlAgentAvailability::NotApplicable => {
                    [4, 5].contains(&self.engine_edition)
                        && self.visibility == MssqlAgentVisibility::Unknown
                        && self.capabilities.iter().all(|capability| {
                            capability.access == MssqlAgentAccess::NotApplicable
                                && capability.columns_sha256.is_none()
                                && capability.object_id.is_none()
                        })
                }
            }
    }
    pub fn matches_snapshot(&self, snapshot: &StateSnapshotDescription) -> bool {
        self.is_valid()
            && snapshot.is_valid()
            && snapshot.dataset == SnapshotDataset::Jobs
            && snapshot.records as usize <= MAX_MSSQL_AGENT_JOB_OBSERVATIONS
            && snapshot.sources.len() == MSSQL_AGENT_SOURCES.len()
            && snapshot.sources.iter().map(|source| source.source.as_str()).collect::<BTreeSet<_>>()
                == MSSQL_AGENT_SOURCES.into_iter().collect()
            && (self.availability != MssqlAgentAvailability::NotApplicable
                || snapshot.records == 0
                    && snapshot.sources.iter().all(|source| source.state == crate::SourceState::NotApplicable))
            && (self.availability == MssqlAgentAvailability::NotApplicable
                || self.coverage_matches_capabilities(snapshot))
    }
    fn coverage_matches_capabilities(&self, snapshot: &StateSnapshotDescription) -> bool {
        snapshot.sources.iter().all(|source| {
            if source.source == MSSQL_AGENT_HISTORY_SOURCE && source.records > self.history_limit
                || source.source == MSSQL_AGENT_CONTEXT_SOURCE && source.records > 2
            {
                return false;
            }
            let available = match source.source.as_str() {
                MSSQL_AGENT_JOBS_SOURCE => self.available(MssqlAgentObject::Jobs),
                MSSQL_AGENT_STEPS_SOURCE => self.available(MssqlAgentObject::Steps),
                MSSQL_AGENT_HISTORY_SOURCE => self.available(MssqlAgentObject::History),
                MSSQL_AGENT_CONTEXT_SOURCE => {
                    self.available(MssqlAgentObject::Sessions) || self.available(MssqlAgentObject::Services)
                }
                _ => false,
            };
            if !available {
                return source.records == 0
                    && !source.complete_scope
                    && source.state == crate::SourceState::Unavailable;
            }
            !source.complete_scope
                || self.visibility == MssqlAgentVisibility::Sysadmin
                    && self.recheck == MssqlAgentMetadataRecheck::Unchanged
                    && source.source != MSSQL_AGENT_HISTORY_SOURCE
                    && (source.source != MSSQL_AGENT_CONTEXT_SOURCE
                        || source.records == 2
                            && self.available(MssqlAgentObject::Sessions)
                            && self.available(MssqlAgentObject::Services))
        })
    }
    pub fn matches_source(&self, source: &CollectionDescription, snapshot: &StateSnapshotDescription) -> bool {
        if !self.matches_snapshot(snapshot)
            || source.target.engine() != crate::Engine::Mssql
            || source.source != MSSQL_AGENT_JOBS_SOURCE
            || !source.partition.is_valid_for(&source.target)
        {
            return false;
        }
        match (&source.target, &source.partition, self.engine_edition) {
            (CollectionTarget::Database(_), SourcePartition::Target, 5) => {
                self.availability == MssqlAgentAvailability::NotApplicable
            }
            (
                CollectionTarget::Shared(target),
                SourcePartition::InstanceLocal { member: Some(member), epoch: Some(_) },
                _,
            ) => target.kind == SharedResourceKind::Instance && target == member,
            _ => false,
        }
    }
    pub fn accepts(&self, snapshot: &StateSnapshotDescription, row: &OperationalObservation) -> bool {
        if !row.is_valid() || self.availability != MssqlAgentAvailability::Supported {
            return false;
        }
        let Some(bounds) = ObservationBounds::new(&snapshot.observed_from, &snapshot.observed_until) else {
            return false;
        };
        match &row.fact {
            OperationalFact::MssqlAgentJob(point) => {
                row.source == MSSQL_AGENT_JOBS_SOURCE
                    && self.available(MssqlAgentObject::Jobs)
                    && bounds.contains(&point.observed_at)
                    && (point.activity.is_none()
                        || self.available(MssqlAgentObject::Activity) && self.available(MssqlAgentObject::Sessions))
            }
            OperationalFact::MssqlAgentStep(point) => {
                row.source == MSSQL_AGENT_STEPS_SOURCE
                    && self.available(MssqlAgentObject::Steps)
                    && bounds.contains(&point.observed_at)
            }
            OperationalFact::MssqlAgentHistory(point) => {
                row.source == MSSQL_AGENT_HISTORY_SOURCE
                    && self.available(MssqlAgentObject::History)
                    && bounds.contains(&point.observed_at)
            }
            OperationalFact::MssqlAgentContext(point) => {
                row.source == MSSQL_AGENT_CONTEXT_SOURCE
                    && bounds.contains(&point.observed_at)
                    && (point.session.is_none() || self.available(MssqlAgentObject::Sessions))
                    && (point.services.is_empty() || self.available(MssqlAgentObject::Services))
            }
            _ => false,
        }
    }
}
