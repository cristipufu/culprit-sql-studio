//! One native scheduler field contract drives acquisition and source-free interpretation.
use super::*;
use std::collections::BTreeSet;

pub const MSSQL_SCHEDULER_OBJECT: &str = "sys.dm_os_schedulers";
pub const MAX_MSSQL_SCHEDULERS: usize = 4096;

#[derive(Clone, Copy)]
pub enum MssqlSchedulerFieldKind {
    Identity,
    IntegerProperty,
    TextProperty,
    BooleanProperty,
    Counter { unit: CounterUnit, nature: CounterNature },
}
#[derive(Clone, Copy)]
pub struct MssqlSchedulerField {
    pub name: &'static str,
    pub kind: MssqlSchedulerFieldKind,
    pub required_column: bool,
}
impl MssqlSchedulerField {
    const fn counter(name: &'static str, unit: CounterUnit, nature: CounterNature) -> Self {
        Self { name, kind: MssqlSchedulerFieldKind::Counter { unit, nature }, required_column: false }
    }
}
use CounterNature::{Cumulative, Gauge, Unknown};
use CounterUnit::{Count, Milliseconds};
use MssqlSchedulerFieldKind::{BooleanProperty, Identity, IntegerProperty, TextProperty};

// Unknown internal progress/load semantics remain unknown. Neither unsupported quantum_length_us nor
// total_cpu_idle_capped_ms is acquired. Native catalog availability governs optional fields.
pub const MSSQL_SCHEDULER_FIELDS: &[MssqlSchedulerField] = &[
    MssqlSchedulerField { name: "scheduler_id", kind: Identity, required_column: true },
    MssqlSchedulerField { name: "parent_node_id", kind: IntegerProperty, required_column: true },
    MssqlSchedulerField { name: "cpu_id", kind: IntegerProperty, required_column: true },
    MssqlSchedulerField { name: "status", kind: TextProperty, required_column: true },
    MssqlSchedulerField { name: "is_online", kind: BooleanProperty, required_column: true },
    MssqlSchedulerField { name: "is_idle", kind: BooleanProperty, required_column: true },
    MssqlSchedulerField { name: "failed_to_create_worker", kind: BooleanProperty, required_column: false },
    MssqlSchedulerField::counter("current_tasks_count", Count, Gauge),
    MssqlSchedulerField::counter("runnable_tasks_count", Count, Gauge),
    MssqlSchedulerField::counter("current_workers_count", Count, Gauge),
    MssqlSchedulerField::counter("active_workers_count", Count, Gauge),
    MssqlSchedulerField::counter("work_queue_count", Count, Gauge),
    MssqlSchedulerField::counter("pending_disk_io_count", Count, Gauge),
    MssqlSchedulerField::counter("preemptive_switches_count", Count, Cumulative),
    MssqlSchedulerField::counter("context_switches_count", Count, Cumulative),
    MssqlSchedulerField::counter("idle_switches_count", Count, Cumulative),
    MssqlSchedulerField::counter("load_factor", CounterUnit::Unknown, Gauge),
    MssqlSchedulerField::counter("yield_count", CounterUnit::Unknown, Unknown),
    MssqlSchedulerField::counter("last_timer_activity", CounterUnit::Unknown, Gauge),
    MssqlSchedulerField::counter("total_cpu_usage_ms", Milliseconds, Cumulative),
    MssqlSchedulerField::counter("total_scheduler_delay_ms", Milliseconds, Cumulative),
    MssqlSchedulerField::counter("ideal_workers_limit", Count, Gauge),
];

/// Original field absence is explicit; native NULL and an unavailable catalog column are different facts.
pub enum MssqlSchedulerValue<'a> {
    Observed(&'a str),
    NativeNull,
    ColumnUnavailable,
}

pub fn scheduler_candidate(sample: &NativeCounterSet) -> bool {
    sample.identity.qualifiers.get("native_object").map(String::as_str) == Some(MSSQL_SCHEDULER_OBJECT)
        || sample.counters.iter().any(|counter| counter.native_object == MSSQL_SCHEDULER_OBJECT)
}

/// Borrowed admission only. A scheduler ID is local to its original source, never a lifetime or request owner.
pub struct MssqlSchedulerPoint<'a> {
    sample: &'a NativeCounterSet,
    unavailable: BTreeSet<&'static str>,
    native_null: BTreeSet<&'static str>,
}
impl<'a> MssqlSchedulerPoint<'a> {
    pub fn from_sample(sample: &'a NativeCounterSet) -> Result<Self> {
        if !sample.is_valid()
            || sample.scope != crate::MeasurementScope::Server
            || sample.identity.qualifiers.len() != 1
            || sample.identity.qualifiers.get("native_object").map(String::as_str) != Some(MSSQL_SCHEDULER_OBJECT)
            || !signed_native(&sample.identity.id)
            || !sample.ratios.is_empty()
            || !sample.ratio_gaps.is_empty()
        {
            return Err(invalid());
        }
        let point = Self {
            sample,
            unavailable: missing_columns(sample, "unavailable_columns")?,
            native_null: missing_columns(sample, "native_null_columns")?,
        };
        if !point.unavailable.is_disjoint(&point.native_null)
            || sample.properties.keys().any(|name| {
                !matches!(name.as_str(), "unavailable_columns" | "native_null_columns")
                    && !MSSQL_SCHEDULER_FIELDS.iter().any(|field| {
                        field.name == name && matches!(field.kind, IntegerProperty | TextProperty | BooleanProperty)
                    })
            })
            || sample.counters.iter().any(|counter| {
                !MSSQL_SCHEDULER_FIELDS.iter().any(|field| {
                    field.name == counter.key && matches!(field.kind, MssqlSchedulerFieldKind::Counter { .. })
                })
            })
        {
            return Err(invalid());
        }
        for field in MSSQL_SCHEDULER_FIELDS {
            point.validate_field(field)?;
        }
        Ok(point)
    }
    pub fn scheduler_id(&self) -> &'a str {
        &self.sample.identity.id
    }
    pub fn sample(&self) -> &'a NativeCounterSet {
        self.sample
    }
    pub fn field(&self, field: &MssqlSchedulerField) -> Result<MssqlSchedulerValue<'a>> {
        if self.unavailable.contains(field.name) {
            Ok(MssqlSchedulerValue::ColumnUnavailable)
        } else if self.native_null.contains(field.name) {
            Ok(MssqlSchedulerValue::NativeNull)
        } else {
            // from_sample established exactly one original representation for every field.
            self.observed(field).map(MssqlSchedulerValue::Observed).ok_or_else(invalid)
        }
    }
    fn observed(&self, field: &MssqlSchedulerField) -> Option<&'a str> {
        match field.kind {
            Identity => Some(self.scheduler_id()),
            MssqlSchedulerFieldKind::Counter { .. } => {
                let counter = self.sample.counters.iter().find(|counter| counter.key == field.name)?;
                let MeasurementValue::Integer(value) = &counter.value else { return None };
                Some(value.as_str())
            }
            _ => self.sample.properties.get(field.name).map(String::as_str),
        }
    }
    fn has_representation(&self, field: &MssqlSchedulerField) -> bool {
        match field.kind {
            Identity => true,
            MssqlSchedulerFieldKind::Counter { .. } => {
                self.sample.counters.iter().any(|counter| counter.key == field.name)
            }
            _ => self.sample.properties.contains_key(field.name),
        }
    }
    fn validate_field(&self, field: &MssqlSchedulerField) -> Result<()> {
        if self.unavailable.contains(field.name) || self.native_null.contains(field.name) {
            if matches!(field.kind, Identity)
                || self.unavailable.contains(field.name) && field.required_column
                || self.has_representation(field)
            {
                return Err(invalid());
            }
            return Ok(());
        }
        let value = self.observed(field).ok_or_else(invalid)?;
        let valid = match field.kind {
            Identity | IntegerProperty => signed_native(value),
            TextProperty => !value.is_empty() && !value.chars().any(char::is_control),
            BooleanProperty => matches!(value, "0" | "1"),
            MssqlSchedulerFieldKind::Counter { unit, nature } => {
                let counter =
                    self.sample.counters.iter().find(|counter| counter.key == field.name).ok_or_else(invalid)?;
                counter.native_object == MSSQL_SCHEDULER_OBJECT
                    && counter.native_name == field.name
                    && counter.native_instance.is_none()
                    && counter.native_type.is_none()
                    && counter.unit == unit
                    && counter.nature == nature
                    && counter.reset == crate::ResetMarker::Unknown
                    && value.parse::<i64>().is_ok_and(|number| number >= 0 && number.to_string() == value)
            }
        };
        valid.then_some(()).ok_or_else(invalid)
    }
}
fn missing_columns(sample: &NativeCounterSet, key: &str) -> Result<BTreeSet<&'static str>> {
    let Some(raw) = sample.properties.get(key) else { return Ok(BTreeSet::new()) };
    let names: Vec<String> = serde_json::from_str(raw).map_err(|_| invalid())?;
    if names.is_empty() || names.len() > MSSQL_SCHEDULER_FIELDS.len() {
        return Err(invalid());
    }
    let mut columns = BTreeSet::new();
    for name in names {
        let field = MSSQL_SCHEDULER_FIELDS.iter().find(|field| field.name == name).ok_or_else(invalid)?;
        if !columns.insert(field.name) {
            return Err(invalid());
        }
    }
    Ok(columns)
}
fn signed_native(value: &str) -> bool {
    value.parse::<i64>().is_ok_and(|number| number.to_string() == value)
}
fn invalid() -> Error {
    CollectionStop::InvalidPage.error()
}

#[cfg(test)]
mod tests;
