//! Connected-instance optional-read safety. Queues/flags are native point facts, not Azure utilization,
//! database pressure grades, owner attribution or source-cost approval. The caller owns identity and shutdown.
use crate::{
    read::{NativeRead, ReadError, ReadResult},
    value::{Rec, Val},
};
use culprit_core::telemetry::{
    CollectionStop, MAX_INSTANCE_RESOURCE_GRANT_REQUESTS as MAX_GRANT_REQUESTS,
    MAX_INSTANCE_RESOURCE_PENDING_IOS as MAX_PENDING_IOS, MAX_INSTANCE_RESOURCE_SCHEDULERS as MAX_SCHEDULERS,
    MAX_INSTANCE_RESOURCE_WAITING_TASKS as MAX_WAITING_TASKS, canonical_time,
};

mod observation;
mod sql;
#[cfg(test)]
mod tests;

// Conservative optional-read policy, not native utilization percentages or storage-latency definitions.
const RUNNABLE_TASKS_PER_SCHEDULER: u64 = 1;
const OS_PENDING_IO_PRESSURE: u64 = 32;
const IO_RELATED_WAIT_PRESSURE_MS: u64 = 1000;

pub(crate) async fn read_instance_pressure<R: NativeRead>(reader: &mut R) -> ReadResult<InstancePressurePoint> {
    let rows = reader.query(&sql::query(), vec![], 1).await.map_err(|error| match error {
        ReadError::Stopped(CollectionStop::RowLimit) => ReadError::Stopped(CollectionStop::PressureUnavailable),
        error => error,
    })?;
    let [row] = rows.as_slice() else {
        return Err(ReadError::Stopped(CollectionStop::PressureUnavailable));
    };
    InstancePressurePoint::decode(row).map_err(ReadError::Stopped)
}

/// Native current-instance facts only. Nulls and bounded-prefix counts remain explicit; none of these values
/// proves database attribution. Optional memory detail never supplies missing admission flags.
#[derive(Debug)]
pub(crate) struct InstancePressurePoint {
    pub observed_at: String,
    pub schedulers: SchedulerPressurePoint,
    pub memory: InstanceMemoryPoint,
    pub grant_rows: u64,
    pub grant_waiters: u64,
    pub pending_io: PendingIoPressurePoint,
    pub io_waits: IoWaitPressurePoint,
}
#[derive(Debug)]
pub(crate) struct SchedulerPressurePoint {
    /// Full native scheduler prefix, before visible-online classification.
    pub population_rows: u64,
    pub observed_rows: u64,
    pub unknown_values: u64,
    pub runnable_tasks: Option<u64>,
    pub worker_queue: Option<u64>,
    pub failed_worker_creations: Option<u64>,
}
#[derive(Debug)]
pub(crate) struct InstanceMemoryPoint {
    pub process_rows: u64,
    pub system_rows: u64,
    pub unknown_process_flags: u64,
    pub unknown_system_flags: u64,
    pub process_physical_low: Option<bool>,
    pub process_virtual_low: Option<bool>,
    pub system_low: Option<bool>,
    pub physical_used_kib: Option<u64>,
    pub virtual_committed_kib: Option<u64>,
    pub virtual_available_kib: Option<u64>,
    pub system_total_kib: Option<u64>,
    pub system_available_kib: Option<u64>,
}
#[derive(Debug)]
pub(crate) struct PendingIoPressurePoint {
    pub observed_rows: u64,
    pub unknown_flags: u64,
    pub os_pending: Option<u64>,
    pub completion_pending: Option<u64>,
}
#[derive(Debug)]
pub(crate) struct IoWaitPressurePoint {
    /// Full native waiting-task prefix, before I/O-family classification.
    pub population_rows: u64,
    pub unknown_types: u64,
    pub observed_rows: u64,
    pub unknown_durations: u64,
    /// The documented current task wait includes signal wait, not just storage service time.
    pub longest_wait_ms: Option<u64>,
}
impl InstancePressurePoint {
    fn decode(row: &Rec) -> Result<Self, CollectionStop> {
        let unavailable = CollectionStop::PressureUnavailable;
        let Some(Val::Text(at)) = value(row, "observed_at") else {
            return Err(unavailable);
        };
        let observed_at = canonical_time(at).map_err(|_| unavailable)?;
        let schedulers = SchedulerPressurePoint {
            population_rows: counter(row, "scheduler_population_rows")?,
            observed_rows: counter(row, "scheduler_rows")?,
            unknown_values: counter(row, "scheduler_invalid")?,
            runnable_tasks: optional_counter(row, "runnable_tasks")?,
            worker_queue: optional_counter(row, "worker_queue")?,
            failed_worker_creations: optional_counter(row, "failed_worker_creations")?,
        };
        let memory = InstanceMemoryPoint {
            process_rows: counter(row, "process_memory_rows")?,
            system_rows: counter(row, "system_memory_rows")?,
            unknown_process_flags: counter(row, "process_memory_invalid")?,
            unknown_system_flags: counter(row, "system_memory_invalid")?,
            process_physical_low: flag(row, "process_physical_low")?,
            process_virtual_low: flag(row, "process_virtual_low")?,
            system_low: flag(row, "system_memory_low")?,
            physical_used_kib: optional_counter(row, "process_physical_used_kib")?,
            virtual_committed_kib: optional_counter(row, "process_virtual_committed_kib")?,
            virtual_available_kib: optional_counter(row, "process_virtual_available_kib")?,
            system_total_kib: optional_counter(row, "system_total_kib")?,
            system_available_kib: optional_counter(row, "system_available_kib")?,
        };
        let pending_io = PendingIoPressurePoint {
            observed_rows: counter(row, "pending_io_rows")?,
            unknown_flags: counter(row, "pending_io_invalid")?,
            os_pending: optional_counter(row, "os_pending_io")?,
            completion_pending: optional_counter(row, "completion_pending_io")?,
        };
        let io_waits = IoWaitPressurePoint {
            population_rows: counter(row, "io_waiter_population_rows")?,
            unknown_types: counter(row, "io_waiter_unknown_types")?,
            observed_rows: counter(row, "io_waiter_rows")?,
            unknown_durations: counter(row, "io_waiter_unknown_durations")?,
            longest_wait_ms: optional_counter(row, "longest_io_related_wait_ms")?,
        };
        let grant_rows = counter(row, "grant_rows")?;
        let grant_waiters = counter(row, "grant_waiters")?;
        let point = Self { observed_at, schedulers, memory, grant_rows, grant_waiters, pending_io, io_waits };
        point.observation().map_err(|_| unavailable)?;
        Ok(point)
    }
    pub fn is_complete(&self) -> bool {
        self.observation().is_ok_and(|point| point.has_complete_population())
    }
    pub fn admit(&self) -> Result<(), CollectionStop> {
        if !self.is_complete() {
            return Err(CollectionStop::PressureUnavailable);
        }
        let runnable_limit = self
            .schedulers
            .observed_rows
            .checked_mul(RUNNABLE_TASKS_PER_SCHEDULER)
            .ok_or(CollectionStop::PressureUnavailable)?;
        if self.schedulers.runnable_tasks.is_some_and(|tasks| tasks >= runnable_limit)
            || self.schedulers.worker_queue.is_some_and(|tasks| tasks > 0)
            || self.schedulers.failed_worker_creations.is_some_and(|failed| failed > 0)
            || self.memory.process_physical_low == Some(true)
            || self.memory.process_virtual_low == Some(true)
            || self.memory.system_low == Some(true)
            || self.grant_waiters > 0
            || self.pending_io.os_pending.is_some_and(|pending| pending >= OS_PENDING_IO_PRESSURE)
            || self.io_waits.longest_wait_ms.is_some_and(|wait| wait >= IO_RELATED_WAIT_PRESSURE_MS)
        {
            Err(CollectionStop::Pressure)
        } else {
            Ok(())
        }
    }
}
fn value<'a>(row: &'a Rec, name: &str) -> Option<&'a Val> {
    let mut matching = row.columns().filter(|(key, _)| *key == name);
    let first = matching.next().map(|(_, value)| value);
    if matching.next().is_some() { None } else { first }
}
fn counter(row: &Rec, name: &str) -> Result<u64, CollectionStop> {
    match value(row, name) {
        Some(Val::Int(value)) if *value >= 0 => Ok(*value as u64),
        _ => Err(CollectionStop::PressureUnavailable),
    }
}
fn optional_counter(row: &Rec, name: &str) -> Result<Option<u64>, CollectionStop> {
    match value(row, name) {
        Some(Val::Null) => Ok(None),
        _ => counter(row, name).map(Some),
    }
}
fn flag(row: &Rec, name: &str) -> Result<Option<bool>, CollectionStop> {
    match optional_counter(row, name)? {
        Some(0) => Ok(Some(false)),
        Some(1) => Ok(Some(true)),
        None => Ok(None),
        _ => Err(CollectionStop::PressureUnavailable),
    }
}
