//! One native performance-counter read supplies both pressure and connection-churn projections.
use crate::read::{NativeRead, ReadError, integer, invalid_data, optional_text, signed_integer, text};
use culprit_core::{
    MeasurementScope, ResetMarker, Result,
    telemetry::{
        CollectionStop, CounterNature, CounterUnit, NativeCounterSample, OperationalFact, OperationalObservation,
        digest,
    },
};
use std::collections::{BTreeSet, HashMap};
pub(crate) const MAX_COUNTERS: usize = 32;
struct CounterSpec {
    key: &'static str,
    family: &'static str,
    name: &'static str,
    nature: CounterNature,
    unit: CounterUnit,
}
const SPECS: &[CounterSpec] = &[
    CounterSpec {
        key: "batches",
        family: "SQL Statistics",
        name: "Batch Requests/sec",
        nature: CounterNature::Cumulative,
        unit: CounterUnit::Count,
    },
    CounterSpec {
        key: "compilations",
        family: "SQL Statistics",
        name: "SQL Compilations/sec",
        nature: CounterNature::Cumulative,
        unit: CounterUnit::Count,
    },
    CounterSpec {
        key: "recompilations",
        family: "SQL Statistics",
        name: "SQL Re-Compilations/sec",
        nature: CounterNature::Cumulative,
        unit: CounterUnit::Count,
    },
    CounterSpec {
        key: "page-life",
        family: "Buffer Manager",
        name: "Page life expectancy",
        nature: CounterNature::Gauge,
        unit: CounterUnit::Seconds,
    },
    CounterSpec {
        key: "logins",
        family: "General Statistics",
        name: "Logins/sec",
        nature: CounterNature::Cumulative,
        unit: CounterUnit::Count,
    },
    CounterSpec {
        key: "connection-resets",
        family: "General Statistics",
        name: "Connection Reset/sec",
        nature: CounterNature::Cumulative,
        unit: CounterUnit::Count,
    },
];
fn sql() -> String {
    // The selector grammar comes only from these fixed specifications, never from a database value or user input.
    let filter = SPECS
        .iter()
        .map(|spec| format!("(object_name LIKE N'%{}%' AND counter_name=N'{}')", spec.family, spec.name))
        .collect::<Vec<_>>()
        .join(" OR ");
    format!(
        "SELECT RTRIM(object_name) AS native_object, RTRIM(counter_name) AS native_name,
RTRIM(instance_name) AS native_instance, cntr_value AS value, cntr_type AS native_type
FROM sys.dm_os_performance_counters WHERE {filter} OPTION(MAXDOP 1)"
    )
}
pub(crate) async fn read<R: NativeRead>(reader: &mut R, limit: usize) -> Result<Vec<NativeCounterSample>> {
    let limit = limit.min(MAX_COUNTERS);
    let rows = reader.query(&sql(), vec![], limit + 1).await.map_err(ReadError::into_error)?;
    if rows.len() > limit {
        return Err(CollectionStop::RowLimit.error());
    }
    if rows.is_empty() {
        return Err(CollectionStop::SourceUnavailable.error());
    }
    let mut identities = BTreeSet::new();
    rows.iter()
        .map(|row| {
            let native_object = text(row, "native_object")?;
            let native_name = text(row, "native_name")?;
            let spec = SPECS
                .iter()
                .find(|spec| spec.name == native_name && native_object.contains(spec.family))
                .ok_or_else(invalid_data)?;
            let code = signed_integer(row, "native_type")?;
            let nature = match code {
                272_696_320 | 272_696_576 => CounterNature::Cumulative,
                65_792 => CounterNature::Gauge,
                _ => CounterNature::Unknown,
            };
            let supported = nature == spec.nature;
            let instance = optional_text(row, "native_instance")?.ok_or_else(invalid_data)?;
            let counter = NativeCounterSample {
                key: spec.key.into(),
                value: culprit_core::telemetry::MeasurementValue::Integer(integer(row, "value")?.to_string()),
                unit: if supported { spec.unit } else { CounterUnit::Unknown },
                nature: if supported { nature } else { CounterNature::Unknown },
                scope: MeasurementScope::Server,
                reset: ResetMarker::Unknown,
                native_object,
                native_name,
                native_instance: (!instance.is_empty()).then_some(instance),
                native_type: Some(code.to_string()),
            };
            if !counter.is_valid()
                || !identities.insert((
                    counter.native_object.clone(),
                    counter.native_name.clone(),
                    counter.native_instance.clone(),
                ))
            {
                return Err(invalid_data());
            }
            Ok(counter)
        })
        .collect()
}
pub(crate) fn application(rows: &[NativeCounterSample]) -> HashMap<String, f64> {
    let mut values = HashMap::new();
    for spec in SPECS {
        if let Some(value) = global(rows, spec).and_then(|row| row.value.integer()) {
            values.insert(spec.name.into(), value as f64);
        }
    }
    values
}
fn global<'a>(rows: &'a [NativeCounterSample], spec: &CounterSpec) -> Option<&'a NativeCounterSample> {
    let mut matching = rows.iter().filter(|row| {
        row.key == spec.key && row.nature == spec.nature && row.unit == spec.unit && row.native_instance.is_none()
    });
    let selected = matching.next()?;
    matching.next().is_none().then_some(selected)
}
pub(crate) fn sampled_rates(
    before: &[NativeCounterSample],
    after: &[NativeCounterSample],
    elapsed_seconds: f64,
) -> HashMap<String, f64> {
    if !elapsed_seconds.is_finite() || elapsed_seconds <= 0.0 {
        return HashMap::new();
    }
    SPECS
        .iter()
        .filter(|spec| spec.nature == CounterNature::Cumulative)
        .filter_map(|spec| {
            let previous = global(before, spec)?;
            let current = global(after, spec)?;
            if previous.scope != current.scope
                || previous.reset != current.reset
                || previous.native_type != current.native_type
                || previous.native_object != current.native_object
            {
                return None;
            }
            let previous = previous.value.integer()?;
            let current = current.value.integer()?;
            let delta = current.checked_sub(previous)?;
            Some((spec.name.into(), delta as f64 / elapsed_seconds))
        })
        .collect()
}
pub(crate) fn observations(rows: Vec<NativeCounterSample>) -> Result<Vec<OperationalObservation>> {
    rows.into_iter()
        .map(|row| {
            let identity = serde_json::to_vec(&(&row.native_object, &row.native_name, &row.native_instance))
                .map_err(|_| invalid_data())?;
            Ok(OperationalObservation {
                source: culprit_core::source_id::PERFORMANCE_COUNTERS.into(),
                key: digest(&identity),
                fact: OperationalFact::Counter(row),
            })
        })
        .collect()
}

#[cfg(test)]
pub(crate) mod tests;
