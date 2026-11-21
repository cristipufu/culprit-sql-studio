//! Closed native scheduler descriptor and borrowed point; shared native-field admission owns representation.
use super::*;
pub const MSSQL_SCHEDULER_OBJECT: &str = "sys.dm_os_schedulers";
pub const MAX_MSSQL_SCHEDULERS: usize = 4096;
const fn spinlock(name: &'static str, unit: CounterUnit, nature: CounterNature) -> MssqlNativeField {
    MssqlNativeField {
        name,
        kind: MssqlNativeFieldKind::UnqualifiedCounter { unit, nature, required_global_trace_flag: 8134 },
        required_column: false,
        introduced_in: Some(17),
    }
}
use CounterNature::{Cumulative, Gauge, Unknown};
use CounterUnit::{Count, Milliseconds};
use MssqlNativeFieldKind::{BooleanProperty, Identity, IntegerProperty, TextProperty};

// Unknown internal progress/load semantics remain unknown. Neither unsupported quantum_length_us nor
// total_cpu_idle_capped_ms is acquired. Native catalog availability governs optional fields.
pub const MSSQL_SCHEDULER_FIELDS: &[MssqlNativeField] = &[
    MssqlNativeField { name: "scheduler_id", kind: Identity, required_column: true, introduced_in: None },
    MssqlNativeField { name: "parent_node_id", kind: IntegerProperty, required_column: true, introduced_in: None },
    MssqlNativeField { name: "cpu_id", kind: IntegerProperty, required_column: true, introduced_in: None },
    MssqlNativeField { name: "status", kind: TextProperty, required_column: true, introduced_in: None },
    MssqlNativeField { name: "is_online", kind: BooleanProperty, required_column: true, introduced_in: None },
    MssqlNativeField { name: "is_idle", kind: BooleanProperty, required_column: true, introduced_in: None },
    MssqlNativeField {
        name: "failed_to_create_worker",
        kind: BooleanProperty,
        required_column: false,
        introduced_in: None,
    },
    MssqlNativeField::counter("current_tasks_count", Count, Gauge),
    MssqlNativeField::counter("runnable_tasks_count", Count, Gauge),
    MssqlNativeField::counter("current_workers_count", Count, Gauge),
    MssqlNativeField::counter("active_workers_count", Count, Gauge),
    MssqlNativeField::counter("work_queue_count", Count, Gauge),
    MssqlNativeField::counter("pending_disk_io_count", Count, Gauge),
    MssqlNativeField::counter("preemptive_switches_count", Count, Cumulative),
    MssqlNativeField::counter("context_switches_count", Count, Cumulative),
    MssqlNativeField::counter("idle_switches_count", Count, Cumulative),
    MssqlNativeField::counter("load_factor", CounterUnit::Unknown, Gauge),
    MssqlNativeField::counter("yield_count", CounterUnit::Unknown, Unknown),
    MssqlNativeField::counter("last_timer_activity", CounterUnit::Unknown, Gauge),
    MssqlNativeField::counter("total_cpu_usage_ms", Milliseconds, Cumulative),
    MssqlNativeField::counter("total_scheduler_delay_ms", Milliseconds, Cumulative),
    MssqlNativeField::counter("ideal_workers_limit", Count, Gauge),
    // SQL Server 2022+, selected only when the original native metadata exposes these columns.
    MssqlNativeField::counter_since("queued_disk_io_count", Count, Gauge, 16),
    MssqlNativeField::counter_since("total_enqueued_tasks", Count, Cumulative, 16),
    // SQL Server 2025+. These include internal SQLOS work, not query execution counts or CPU demand.
    MssqlNativeField::counter_since("total_signal_time", Milliseconds, Cumulative, 17),
    MssqlNativeField::counter_since("total_waits_completed", Count, Cumulative, 17),
    MssqlNativeField::counter_since("total_completed_tasks", Count, Cumulative, 17),
    // https://learn.microsoft.com/en-us/sql/relational-databases/system-dynamic-management-objects/sys-dm-os-schedulers-transact-sql
    // Collection requires global TF 8134. This reader neither observes its state nor enables it; raw native
    // scalars live in diagnostic properties, outside NativeCounterSample, including when their value is zero.
    spinlock("spinlock_wait_time_ms", Milliseconds, Cumulative),
    spinlock("spinlock_max_wait_time_ms", Milliseconds, Gauge),
    spinlock("spinlock_wait_count", Count, Cumulative),
];

pub fn scheduler_candidate(sample: &NativeCounterSet) -> bool {
    sample.identity.qualifiers.get("native_object").map(String::as_str) == Some(MSSQL_SCHEDULER_OBJECT)
        || sample.counters.iter().any(|counter| counter.native_object == MSSQL_SCHEDULER_OBJECT)
}
pub struct MssqlSchedulerPoint<'a>(MssqlNativeFieldRoster<'a>);
impl<'a> MssqlSchedulerPoint<'a> {
    pub fn from_sample(sample: &'a NativeCounterSet) -> Result<Self> {
        Ok(Self(MssqlNativeFieldRoster::from_sample(sample, MSSQL_SCHEDULER_OBJECT, MSSQL_SCHEDULER_FIELDS)?))
    }
    pub fn scheduler_id(&self) -> &'a str {
        self.0.native_id()
    }
    pub fn sample(&self) -> &'a NativeCounterSet {
        self.0.sample()
    }
    pub fn field(&self, field: &MssqlNativeField) -> Result<MssqlNativeFieldValue<'a>> {
        self.0.field(field)
    }
}
#[cfg(test)]
mod tests;
