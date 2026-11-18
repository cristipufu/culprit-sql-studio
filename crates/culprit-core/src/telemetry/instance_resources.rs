//! One native shared-instance resource point. Acquisition, admission and historical attribution remain separate.
use super::*;

pub const INSTANCE_RESOURCES_SOURCE: &str = "instance-resources";
pub const MAX_INSTANCE_RESOURCE_SCHEDULERS: u32 = 1024;
pub const MAX_INSTANCE_RESOURCE_GRANT_REQUESTS: u32 = 256;
pub const MAX_INSTANCE_RESOURCE_PENDING_IOS: u32 = 256;
pub const MAX_INSTANCE_RESOURCE_WAITING_TASKS: u32 = 256;

/// Native bounded-prefix aggregation from the documented SQL Server/Managed Instance DMVs. Counts stay exact;
/// the observation carries no selected-database denominator, resource-owner claim or admission verdict.
#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct MssqlInstanceResourceObservation {
    pub observed_at: String,
    pub schedulers: MssqlInstanceSchedulerPoint,
    pub memory: MssqlInstanceMemoryPoint,
    pub grant_rows: u32,
    pub grant_waiters: u32,
    pub pending_io: MssqlPendingIoPoint,
    pub io_waits: MssqlIoWaitPoint,
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct MssqlInstanceSchedulerPoint {
    /// All native rows sampled before visible-online classification, including the cap + 1 witness.
    pub population_rows: u32,
    /// VISIBLE ONLINE rows within that original bounded prefix.
    pub observed_rows: u32,
    pub unknown_values: u32,
    pub runnable_tasks: Option<String>,
    pub worker_queue: Option<String>,
    /// Schedulers reporting failed_to_create_worker, not a count of worker-creation attempts.
    pub failed_worker_creations: Option<String>,
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct MssqlInstanceMemoryPoint {
    /// Each native memory DMV is expected to return exactly one row; zero/two remain visible evidence gaps.
    pub process_rows: u32,
    pub system_rows: u32,
    pub unknown_process_flags: u32,
    pub unknown_system_flags: u32,
    pub process_physical_low: Option<bool>,
    pub process_virtual_low: Option<bool>,
    pub system_low: Option<bool>,
    /// Native KB, retained as exact whole KiB without f64 conversion or a database attribution.
    pub physical_used_kib: Option<String>,
    pub virtual_committed_kib: Option<String>,
    pub virtual_available_kib: Option<String>,
    pub system_total_kib: Option<String>,
    pub system_available_kib: Option<String>,
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct MssqlPendingIoPoint {
    pub observed_rows: u32,
    pub unknown_flags: u32,
    pub os_pending: Option<String>,
    /// OS-completed requests awaiting SQL Server completion, separate from OS-pending work.
    pub completion_pending: Option<String>,
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct MssqlIoWaitPoint {
    /// All native tasks sampled before I/O-family classification, including the cap + 1 witness.
    pub population_rows: u32,
    pub unknown_types: u32,
    pub observed_rows: u32,
    pub unknown_durations: u32,
    /// Current native task wait, including signal wait. This is not storage service time or request duration.
    pub longest_wait_ms: Option<String>,
}

impl MssqlInstanceResourceObservation {
    pub fn is_valid(&self) -> bool {
        self.observed_at.len() <= 64
            && ObservationBounds::new(&self.observed_at, &self.observed_at).is_some()
            && self.schedulers.population_rows <= MAX_INSTANCE_RESOURCE_SCHEDULERS + 1
            && self.schedulers.observed_rows <= self.schedulers.population_rows
            && self.schedulers.unknown_values <= self.schedulers.population_rows
            && self.memory.process_rows <= 2
            && self.memory.system_rows <= 2
            && self.memory.unknown_process_flags <= self.memory.process_rows
            && self.memory.unknown_system_flags <= self.memory.system_rows
            && (self.memory.process_rows == 1
                || self.memory.process_physical_low.is_none()
                    && self.memory.process_virtual_low.is_none()
                    && self.memory.physical_used_kib.is_none()
                    && self.memory.virtual_committed_kib.is_none()
                    && self.memory.virtual_available_kib.is_none())
            && (self.memory.system_rows == 1
                || self.memory.system_low.is_none()
                    && self.memory.system_total_kib.is_none()
                    && self.memory.system_available_kib.is_none())
            && self.grant_rows <= MAX_INSTANCE_RESOURCE_GRANT_REQUESTS + 1
            && self.grant_waiters <= self.grant_rows
            && self.pending_io.observed_rows <= MAX_INSTANCE_RESOURCE_PENDING_IOS + 1
            && self.pending_io.unknown_flags <= self.pending_io.observed_rows
            && self.io_waits.population_rows <= MAX_INSTANCE_RESOURCE_WAITING_TASKS + 1
            && self.io_waits.observed_rows <= self.io_waits.population_rows
            && self.io_waits.unknown_durations <= self.io_waits.population_rows
            && self
                .io_waits
                .unknown_types
                .checked_add(self.io_waits.observed_rows)
                .is_some_and(|rows| rows <= self.io_waits.population_rows)
            && self.exact_values().into_iter().all(|value| value.is_none_or(valid_native_count))
            && self.schedulers.failed_worker_creations.as_deref().is_none_or(|value| {
                native_count(value).is_some_and(|value| value <= u64::from(self.schedulers.observed_rows))
            })
            && match (self.pending_io.os_pending.as_deref(), self.pending_io.completion_pending.as_deref()) {
                (Some(os), Some(completed)) => {
                    native_count(os).zip(native_count(completed)).is_some_and(|(os, completed)| {
                        os.checked_add(completed) == Some(u64::from(self.pending_io.observed_rows))
                    })
                }
                _ => true,
            }
            && (self.io_waits.observed_rows > 0 || self.io_waits.longest_wait_ms.is_none())
    }

    /// Full bounded required populations are independent of both measured values and any optional-read policy.
    /// Optional memory sizes never replace missing flags or establish source membership.
    pub fn has_complete_population(&self) -> bool {
        self.is_valid()
            && (1..=MAX_INSTANCE_RESOURCE_SCHEDULERS).contains(&self.schedulers.population_rows)
            && self.schedulers.observed_rows > 0
            && self.schedulers.unknown_values == 0
            && self.schedulers.runnable_tasks.is_some()
            && self.schedulers.worker_queue.is_some()
            && self.schedulers.failed_worker_creations.is_some()
            && self.memory.process_rows == 1
            && self.memory.system_rows == 1
            && self.memory.unknown_process_flags == 0
            && self.memory.unknown_system_flags == 0
            && self.memory.process_physical_low.is_some()
            && self.memory.process_virtual_low.is_some()
            && self.memory.system_low.is_some()
            && self.grant_rows <= MAX_INSTANCE_RESOURCE_GRANT_REQUESTS
            && self.pending_io.observed_rows <= MAX_INSTANCE_RESOURCE_PENDING_IOS
            && self.pending_io.unknown_flags == 0
            && self.pending_io.os_pending.is_some()
            && self.pending_io.completion_pending.is_some()
            && self.io_waits.population_rows <= MAX_INSTANCE_RESOURCE_WAITING_TASKS
            && self.io_waits.unknown_types == 0
            && self.io_waits.unknown_durations == 0
            && (self.io_waits.observed_rows == 0 || self.io_waits.longest_wait_ms.is_some())
    }

    pub fn key(&self) -> Result<String> {
        if !self.is_valid() {
            return Err(invalid());
        }
        serde_json::to_vec(&("mssql-instance-resources", &self.observed_at))
            .map(|bytes| digest(&bytes))
            .map_err(|_| invalid())
    }

    pub fn matches_context(&self, context: &NativeReadContext) -> bool {
        self.is_valid()
            && context.is_valid()
            && matches!(&context.target, CollectionTarget::Shared(resource) if resource.engine == crate::Engine::Mssql && resource.kind == SharedResourceKind::Instance)
            && ObservationBounds::new(&context.observed_from, &context.observed_until)
                .is_some_and(|bounds| bounds.contains(&self.observed_at))
    }

    pub fn fits_source(&self, source: &CollectionDescription) -> bool {
        source.observation.snapshot().is_some_and(|header| {
            header.dataset == SnapshotDataset::Operational
                && header.sources.iter().any(|coverage| {
                    coverage.source == INSTANCE_RESOURCES_SOURCE
                        && coverage.records == 1
                        && (!coverage.complete_scope || self.has_complete_population())
                })
        }) && NativeReadContext::from_snapshot(source).is_ok_and(|context| self.matches_context(&context))
    }

    fn exact_values(&self) -> [Option<&str>; 11] {
        [
            self.schedulers.runnable_tasks.as_deref(),
            self.schedulers.worker_queue.as_deref(),
            self.schedulers.failed_worker_creations.as_deref(),
            self.memory.physical_used_kib.as_deref(),
            self.memory.virtual_committed_kib.as_deref(),
            self.memory.virtual_available_kib.as_deref(),
            self.memory.system_total_kib.as_deref(),
            self.memory.system_available_kib.as_deref(),
            self.pending_io.os_pending.as_deref(),
            self.pending_io.completion_pending.as_deref(),
            self.io_waits.longest_wait_ms.as_deref(),
        ]
    }
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct InstanceResourceRead {
    /// Collector bounds intentionally carry no member, process or database scope proof.
    pub context: ObservationReadContext,
    pub source: StateSourceCoverage,
    pub observation: Option<Box<MssqlInstanceResourceObservation>>,
    pub read_complete: bool,
}

impl InstanceResourceRead {
    pub fn is_valid(&self) -> bool {
        let retained = u32::from(self.observation.is_some());
        self.context.is_valid()
            && self.context.native().is_none_or(|context| {
                matches!(&context.target, CollectionTarget::Shared(resource) if resource.engine == crate::Engine::Mssql && resource.kind == SharedResourceKind::Instance)
            })
            && self.source.source == INSTANCE_RESOURCES_SOURCE
            && self.source.is_valid()
            && self.source.state != crate::SourceState::Empty
            && self.source.records <= 1
            && retained <= self.source.records
            && (!self.read_complete || retained == self.source.records)
            && self.observation.as_deref().is_none_or(|point| {
                point.is_valid()
                    && matches!(self.source.state, crate::SourceState::Collected | crate::SourceState::Partial)
                    && (!self.source.complete_scope || point.has_complete_population())
                    && self.context.native().is_none_or(|context| point.matches_context(context))
            })
    }

    pub fn from_source(
        source: &CollectionDescription,
        observations: &[OperationalObservation],
        read_complete: bool,
    ) -> Result<Self> {
        let header = source.observation.snapshot().ok_or_else(invalid)?;
        if header.dataset != SnapshotDataset::Operational {
            return Err(invalid());
        }
        let coverage = header
            .sources
            .iter()
            .find(|coverage| coverage.source == INSTANCE_RESOURCES_SOURCE)
            .ok_or_else(invalid)?
            .clone();
        let mut points = observations.iter().filter(|row| row.source == INSTANCE_RESOURCES_SOURCE);
        let observation = points
            .next()
            .map(|row| {
                if !row.is_valid() {
                    return Err(invalid());
                }
                let OperationalFact::MssqlInstanceResources(point) = &row.fact else {
                    return Err(invalid());
                };
                Ok(point.clone())
            })
            .transpose()?;
        if points.next().is_some() {
            return Err(invalid());
        }
        let read = Self {
            context: ObservationReadContext::Native { context: Box::new(NativeReadContext::from_snapshot(source)?) },
            source: coverage,
            observation,
            read_complete,
        };
        read.is_valid().then_some(read).ok_or_else(invalid)
    }
}

fn native_count(value: &str) -> Option<u64> {
    value.parse::<u64>().ok().filter(|number| *number <= i64::MAX as u64 && number.to_string() == value)
}
fn valid_native_count(value: &str) -> bool {
    native_count(value).is_some()
}
fn invalid() -> Error {
    CollectionStop::InvalidPage.error()
}

#[cfg(test)]
mod tests;
