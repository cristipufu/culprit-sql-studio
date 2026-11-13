//! One native field table drives catalog discovery, SQL projection and interpretation.
use super::*;
use crate::{
    read::{optional_boolean, optional_integer, optional_signed_integer, signed_integer, text},
    value::{Rec, Val},
};
use culprit_core::{MeasurementScope, ResetMarker};
use std::collections::BTreeMap;

#[derive(Clone, Copy)]
enum Kind {
    Identity,
    IntegerProperty,
    TextProperty,
    BooleanProperty,
    Counter { unit: CounterUnit, nature: CounterNature },
}
struct Field {
    name: &'static str,
    kind: Kind,
    required_column: bool,
}
impl Field {
    const fn counter(name: &'static str, unit: CounterUnit, nature: CounterNature) -> Self {
        Self { name, kind: Kind::Counter { unit, nature }, required_column: false }
    }
    fn sql_type(&self) -> &'static str {
        match self.kind {
            Kind::BooleanProperty => "bit",
            Kind::TextProperty => "nvarchar(60)",
            _ => "bigint",
        }
    }
    fn append_native_value(&self, row: &Rec, sample: &mut NativeCounterSet) -> Result<()> {
        match self.kind {
            Kind::Identity => {
                signed_integer(row, self.name)?;
            }
            Kind::IntegerProperty => {
                sample.properties.insert(
                    self.name.into(),
                    optional_signed_integer(row, self.name)?.ok_or_else(invalid_data)?.to_string(),
                );
            }
            Kind::TextProperty => {
                let value = crate::read::optional_text(row, self.name)?.ok_or_else(invalid_data)?;
                if value.is_empty() || value.chars().any(char::is_control) {
                    return Err(invalid_data());
                }
                sample.properties.insert(self.name.into(), value);
            }
            Kind::BooleanProperty => {
                sample.properties.insert(
                    self.name.into(),
                    u8::from(optional_boolean(row, self.name)?.ok_or_else(invalid_data)?).to_string(),
                );
            }
            Kind::Counter { unit, nature } => {
                let count = optional_integer(row, self.name)?.ok_or_else(invalid_data)?;
                sample.counters.push(NativeCounterSample {
                    key: self.name.into(),
                    value: MeasurementValue::Integer(count.to_string()),
                    unit,
                    nature,
                    scope: MeasurementScope::Server,
                    reset: ResetMarker::Unknown,
                    native_object: NATIVE_OBJECT.into(),
                    native_name: self.name.into(),
                    native_instance: None,
                    native_type: None,
                });
            }
        }
        Ok(())
    }
}
use CounterNature::{Cumulative, Gauge, Unknown};
use CounterUnit::{Count, Milliseconds};

// Internal progress/load values retain unknown units/meaning. Unsupported quantum_length_us and
// total_cpu_idle_capped_ms are intentionally excluded. Optional columns are selected by native availability,
// including total_cpu_usage_ms/total_scheduler_delay_ms (SQL Server 2016+) and ideal_workers_limit (2019+).
const FIELDS: &[Field] = &[
    Field { name: IDENTITY_COLUMN, kind: Kind::Identity, required_column: true },
    Field { name: "parent_node_id", kind: Kind::IntegerProperty, required_column: true },
    Field { name: "cpu_id", kind: Kind::IntegerProperty, required_column: true },
    Field { name: "status", kind: Kind::TextProperty, required_column: true },
    Field { name: "is_online", kind: Kind::BooleanProperty, required_column: true },
    Field { name: "is_idle", kind: Kind::BooleanProperty, required_column: true },
    Field { name: "failed_to_create_worker", kind: Kind::BooleanProperty, required_column: false },
    Field::counter("current_tasks_count", Count, Gauge),
    Field::counter("runnable_tasks_count", Count, Gauge),
    Field::counter("current_workers_count", Count, Gauge),
    Field::counter("active_workers_count", Count, Gauge),
    Field::counter("work_queue_count", Count, Gauge),
    Field::counter("pending_disk_io_count", Count, Gauge),
    Field::counter("preemptive_switches_count", Count, Cumulative),
    Field::counter("context_switches_count", Count, Cumulative),
    Field::counter("idle_switches_count", Count, Cumulative),
    Field::counter("load_factor", CounterUnit::Unknown, Gauge),
    Field::counter("yield_count", CounterUnit::Unknown, Unknown),
    Field::counter("last_timer_activity", CounterUnit::Unknown, Gauge),
    Field::counter("total_cpu_usage_ms", Milliseconds, Cumulative),
    Field::counter("total_scheduler_delay_ms", Milliseconds, Cumulative),
    Field::counter("ideal_workers_limit", Count, Gauge),
];

pub(super) struct Columns {
    available: BTreeSet<String>,
}
impl Columns {
    pub(super) async fn read<R: NativeRead>(reader: &mut R) -> Result<Self> {
        let names = FIELDS.iter().map(|field| format!("N'{}'", field.name)).collect::<Vec<_>>().join(",");
        let query = format!(
            "SELECT /* culprit */ TOP (@P1) c.name FROM sys.all_columns c WHERE c.object_id=OBJECT_ID(N'{NATIVE_OBJECT}') AND c.name IN ({names}) ORDER BY c.name OPTION(MAXDOP 1)"
        );
        let rows = reader
            .query(&query, vec![Parameter::Integer((FIELDS.len() + 1) as i64)], FIELDS.len() + 1)
            .await
            .map_err(ReadError::into_error)?;
        if rows.len() > FIELDS.len() {
            return Err(CollectionStop::RowLimit.error());
        }
        let mut available = BTreeSet::new();
        for row in rows {
            let name = text(&row, "name")?;
            if !FIELDS.iter().any(|field| field.name == name) || !available.insert(name) {
                return Err(invalid_data());
            }
        }
        if FIELDS.iter().any(|field| field.required_column && !available.contains(field.name)) {
            // Missing and hidden metadata cannot establish scheduler scope. Do not guess a legacy layout.
            return Err(CollectionStop::SourceUnavailable.error());
        }
        Ok(Self { available })
    }
    pub(super) fn projection(&self) -> String {
        FIELDS
            .iter()
            .map(|field| {
                if self.available.contains(field.name) {
                    format!("[{}]", field.name)
                } else {
                    format!("CAST(NULL AS {}) AS [{}]", field.sql_type(), field.name)
                }
            })
            .collect::<Vec<_>>()
            .join(",")
    }
    pub(super) fn decode(&self, row: &Rec) -> Result<NativeCounterSet> {
        let mut sample = NativeCounterSet {
            identity: NativeIdentity::new(signed_integer(row, IDENTITY_COLUMN)?.to_string())
                .qualified("native_object", NATIVE_OBJECT),
            scope: MeasurementScope::Server,
            counters: Vec::new(),
            properties: BTreeMap::new(),
        };
        let mut unavailable = Vec::new();
        let mut native_null = Vec::new();
        for field in FIELDS {
            let value =
                row.columns().find(|(name, _)| *name == field.name).map(|(_, value)| value).ok_or_else(invalid_data)?;
            if !self.available.contains(field.name) {
                if *value != Val::Null {
                    return Err(invalid_data());
                }
                unavailable.push(field.name);
                continue;
            }
            if *value == Val::Null {
                if matches!(field.kind, Kind::Identity) {
                    return Err(invalid_data());
                }
                native_null.push(field.name);
                continue;
            }
            field.append_native_value(row, &mut sample)?;
        }
        for (key, names) in [("unavailable_columns", unavailable), ("native_null_columns", native_null)] {
            if !names.is_empty() {
                sample.properties.insert(key.into(), serde_json::to_string(&names).map_err(|_| invalid_data())?);
            }
        }
        if !sample.is_valid() {
            return Err(invalid_data());
        }
        Ok(sample)
    }
}

#[cfg(test)]
pub(crate) fn all_columns() -> Vec<Rec> {
    FIELDS.iter().map(|field| Rec::of(&[("name", Val::Text(field.name.into()))])).collect()
}
