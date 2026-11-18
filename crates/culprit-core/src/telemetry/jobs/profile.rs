//! One recorded read-completeness value accompanies each closed native scheduler profile.
use super::*;

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
#[ts(export)]
pub enum JobsSnapshot {
    Timescale { snapshot: Box<TimescaleJobsSnapshot>, read_complete: bool },
    MssqlAgent { snapshot: Box<MssqlAgentJobsSnapshot>, read_complete: bool },
}
impl JobsSnapshot {
    pub fn from_source(
        source: &CollectionDescription,
        rows: Vec<OperationalObservation>,
        read_complete: bool,
    ) -> Result<Self> {
        let read = match &source.observation {
            SourceObservation::Jobs { .. } => {
                Self::Timescale { snapshot: Box::new(TimescaleJobsSnapshot::from_source(source, rows)?), read_complete }
            }
            SourceObservation::MssqlAgentJobs { .. } => Self::MssqlAgent {
                snapshot: Box::new(MssqlAgentJobsSnapshot::from_source(source, rows)?),
                read_complete,
            },
            _ => return Err(CollectionStop::InvalidPage.error()),
        };
        read.is_valid().then_some(read).ok_or_else(|| CollectionStop::InvalidPage.error())
    }
    pub fn context(&self) -> &NativeReadContext {
        match self {
            Self::Timescale { snapshot, .. } => &snapshot.context,
            Self::MssqlAgent { snapshot, .. } => &snapshot.context,
        }
    }
    pub fn observations(&self) -> &[OperationalObservation] {
        match self {
            Self::Timescale { snapshot, .. } => &snapshot.observations,
            Self::MssqlAgent { snapshot, .. } => &snapshot.observations,
        }
    }
    pub fn sources(&self) -> &[StateSourceCoverage] {
        match self {
            Self::Timescale { snapshot, .. } => &snapshot.sources,
            Self::MssqlAgent { snapshot, .. } => &snapshot.sources,
        }
    }
    pub fn read_complete(&self) -> bool {
        match self {
            Self::Timescale { read_complete, .. } | Self::MssqlAgent { read_complete, .. } => *read_complete,
        }
    }
    pub fn primary_source(&self) -> &'static str {
        match self {
            Self::Timescale { .. } => TIMESCALE_JOBS_SOURCE,
            Self::MssqlAgent { .. } => MSSQL_AGENT_JOBS_SOURCE,
        }
    }
    pub fn is_valid(&self) -> bool {
        match self {
            Self::Timescale { snapshot, read_complete } => snapshot.is_valid_read(*read_complete),
            Self::MssqlAgent { snapshot, read_complete } => snapshot.is_valid_read(*read_complete),
        }
    }
}
