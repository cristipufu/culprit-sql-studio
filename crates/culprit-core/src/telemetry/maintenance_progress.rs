//! Native maintenance progress is a bounded point, not completion history, lock acquisition or resource ownership.
use super::*;

pub const MAX_MAINTENANCE_PROGRESS_PHASE_BYTES: usize = 512;

mod work;
pub use work::*;
mod wait;
pub use wait::*;

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct MaintenanceProgressObservation {
    pub observed_at: String,
    pub actor: ActivityKey,
    pub statement: Option<NativeIdentity>,
    /// OIDs are native references inside this read's database/member context, not object incarnation proofs.
    pub database_oid: String,
    pub relation_oid: String,
    /// Native zero is retained, notably for a non-concurrent PostgreSQL CREATE INDEX.
    pub index_oid: Option<String>,
    pub command: MaintenanceCommand,
    pub phase: String,
    pub work: MaintenanceProgressWork,
}

impl MaintenanceProgressObservation {
    pub fn fits_source_scope(&self, source: &CollectionDescription) -> bool {
        let SourceObservation::MaintenanceProgress { snapshot, result } = &source.observation else { return false };
        result.is_valid_source(source, snapshot) && self.matches_result(&result.read_context(source, snapshot), result)
    }

    fn matches_result(&self, context: &NativeReadContext, result: &FocusedRequestResult) -> bool {
        self.is_valid()
            && self.work.engine() == context.target.engine()
            && result.matches_point(context, &self.actor, self.statement.as_ref(), &self.observed_at)
    }

    pub fn is_valid(&self) -> bool {
        valid_time(&self.observed_at)
            && self.actor.is_valid()
            && self.actor.request.is_some()
            && self.statement.as_ref().is_none_or(NativeIdentity::is_valid)
            && oid(&self.database_oid)
            && self.database_oid != "0"
            && oid(&self.relation_oid)
            && self.index_oid.as_deref().is_none_or(oid)
            && self.index_oid.is_some() == self.work.has_index_reference()
            && self.work.accepts_command(self.command)
            && !self.phase.is_empty()
            && self.phase.len() <= MAX_MAINTENANCE_PROGRESS_PHASE_BYTES
            && !self.phase.chars().any(char::is_control)
            && self.work.is_valid()
    }

    pub fn key(&self) -> Result<String> {
        if !self.is_valid() {
            return Err(CollectionStop::InvalidPage.error());
        }
        serde_json::to_vec(&(
            &self.actor,
            &self.database_oid,
            &self.relation_oid,
            &self.index_oid,
            self.command,
            self.work.kind(),
        ))
        .map(|bytes| digest(&bytes))
        .map_err(|_| CollectionStop::InvalidPage.error())
    }

    pub fn relation_reference(&self) -> Option<NativeIdentity> {
        (self.relation_oid != "0")
            .then(|| NativeIdentity::new(&self.relation_oid).qualified("database_oid", &self.database_oid))
    }

    pub fn index_reference(&self) -> Option<NativeIdentity> {
        self.index_oid
            .as_deref()
            .filter(|oid| *oid != "0")
            .map(|oid| NativeIdentity::new(oid).qualified("database_oid", &self.database_oid))
    }
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct MaintenanceProgressSnapshot {
    pub source: String,
    pub context: NativeReadContext,
    pub coverage: StateSourceCoverage,
    pub result: FocusedRequestResult,
    pub observations: Vec<MaintenanceProgressObservation>,
}

impl MaintenanceProgressSnapshot {
    pub fn is_valid(&self) -> bool {
        self.is_valid_read(true)
    }

    /// Readback gaps never rewrite the original source count or membership verdict.
    pub fn is_valid_read(&self, read_complete: bool) -> bool {
        let header = StateSnapshotDescription {
            dataset: SnapshotDataset::MaintenanceProgress,
            observed_from: self.context.observed_from.clone(),
            observed_until: self.context.observed_until.clone(),
            records: self.coverage.records,
            sources: vec![self.coverage.clone()],
        };
        valid_key(&self.source)
            && self.result.is_valid_header(&header)
            && self.result.is_valid_for(&self.context)
            && self.coverage.source == self.source
            && self.observations.len() <= self.coverage.records as usize
            && (!read_complete || self.coverage.records as usize == self.observations.len())
            && self.observations.len() <= MAX_FOCUSED_REQUEST_OBSERVATIONS
            && focused_request_coverage_fits_context(&self.context, &self.coverage)
            && self.observations.iter().all(|row| row.matches_result(&self.context, &self.result))
    }
}

fn valid_time(value: &str) -> bool {
    value.len() <= 64 && canonical_time(value).is_ok()
}

fn canonical_counter(value: &str) -> bool {
    valid_counter(value) && value.parse::<u64>().is_ok_and(|number| number.to_string() == value)
}

fn oid(value: &str) -> bool {
    canonical_counter(value) && value.parse::<u32>().is_ok()
}

#[cfg(test)]
mod tests;
