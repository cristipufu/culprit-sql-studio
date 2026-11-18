//! One exact transport projection of the already decoded native point; no native read or admission decision.
use super::InstancePressurePoint;
use culprit_core::{Result, telemetry::*};

impl InstancePressurePoint {
    pub(crate) fn observation(&self) -> Result<MssqlInstanceResourceObservation> {
        let point = MssqlInstanceResourceObservation {
            observed_at: self.observed_at.clone(),
            schedulers: MssqlInstanceSchedulerPoint {
                population_rows: bounded_count(self.schedulers.population_rows)?,
                observed_rows: bounded_count(self.schedulers.observed_rows)?,
                unknown_values: bounded_count(self.schedulers.unknown_values)?,
                runnable_tasks: exact(self.schedulers.runnable_tasks),
                worker_queue: exact(self.schedulers.worker_queue),
                failed_worker_creations: exact(self.schedulers.failed_worker_creations),
            },
            memory: MssqlInstanceMemoryPoint {
                process_rows: bounded_count(self.memory.process_rows)?,
                system_rows: bounded_count(self.memory.system_rows)?,
                unknown_process_flags: bounded_count(self.memory.unknown_process_flags)?,
                unknown_system_flags: bounded_count(self.memory.unknown_system_flags)?,
                process_physical_low: self.memory.process_physical_low,
                process_virtual_low: self.memory.process_virtual_low,
                system_low: self.memory.system_low,
                physical_used_kib: exact(self.memory.physical_used_kib),
                virtual_committed_kib: exact(self.memory.virtual_committed_kib),
                virtual_available_kib: exact(self.memory.virtual_available_kib),
                system_total_kib: exact(self.memory.system_total_kib),
                system_available_kib: exact(self.memory.system_available_kib),
            },
            grant_rows: bounded_count(self.grant_rows)?,
            grant_waiters: bounded_count(self.grant_waiters)?,
            pending_io: MssqlPendingIoPoint {
                observed_rows: bounded_count(self.pending_io.observed_rows)?,
                unknown_flags: bounded_count(self.pending_io.unknown_flags)?,
                os_pending: exact(self.pending_io.os_pending),
                completion_pending: exact(self.pending_io.completion_pending),
            },
            io_waits: MssqlIoWaitPoint {
                population_rows: bounded_count(self.io_waits.population_rows)?,
                unknown_types: bounded_count(self.io_waits.unknown_types)?,
                observed_rows: bounded_count(self.io_waits.observed_rows)?,
                unknown_durations: bounded_count(self.io_waits.unknown_durations)?,
                longest_wait_ms: exact(self.io_waits.longest_wait_ms),
            },
        };
        point.is_valid().then_some(point).ok_or_else(|| CollectionStop::InvalidPage.error())
    }
}

fn exact(value: Option<u64>) -> Option<String> {
    value.map(|value| value.to_string())
}
fn bounded_count(value: u64) -> Result<u32> {
    u32::try_from(value).map_err(|_| CollectionStop::InvalidPage.error())
}
