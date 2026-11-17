//! Native concurrent-index waiting phases establish the maintenance request as the waiter.
use super::*;

#[derive(Serialize, Deserialize, TS, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum PostgresIndexWaitPhase {
    WritersBeforeBuild,
    WritersBeforeValidation,
    OldSnapshots,
    ReadersBeforeMarkingDead,
    ReadersBeforeDropping,
}

impl PostgresIndexWaitPhase {
    // These exact native command/phase pairs are documented from PostgreSQL 12 onward:
    // https://www.postgresql.org/docs/12/progress-reporting.html#CREATE-INDEX-PROGRESS-REPORTING
    fn from_native(command: MaintenanceCommand, phase: &str) -> Option<Self> {
        use MaintenanceCommand::{CreateIndexConcurrently, ReindexConcurrently};
        match (command, phase) {
            (CreateIndexConcurrently | ReindexConcurrently, "waiting for writers before build") => {
                Some(Self::WritersBeforeBuild)
            }
            (CreateIndexConcurrently | ReindexConcurrently, "waiting for writers before validation") => {
                Some(Self::WritersBeforeValidation)
            }
            (CreateIndexConcurrently | ReindexConcurrently, "waiting for old snapshots") => Some(Self::OldSnapshots),
            (ReindexConcurrently, "waiting for readers before marking dead") => Some(Self::ReadersBeforeMarkingDead),
            (ReindexConcurrently, "waiting for readers before dropping") => Some(Self::ReadersBeforeDropping),
            _ => None,
        }
    }
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct PostgresIndexBuildWait {
    pub phase: PostgresIndexWaitPhase,
    /// A point-reported PID has no independently captured backend or transaction generation.
    pub unresolved_locker_pid: Option<String>,
}

impl MaintenanceProgressObservation {
    /// Classifies an original native point; it grants no lock-owner, duration or completion authority.
    pub fn postgres_index_wait(&self) -> Option<PostgresIndexBuildWait> {
        if !self.is_valid() {
            return None;
        }
        let MaintenanceProgressWork::PostgreSqlIndexBuild { progress } = &self.work else { return None };
        let phase = PostgresIndexWaitPhase::from_native(self.command, &self.phase)?;
        Some(PostgresIndexBuildWait {
            phase,
            unresolved_locker_pid: progress.current_locker_pid.as_ref().filter(|pid| pid.as_str() != "0").cloned(),
        })
    }
}
