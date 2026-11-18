//! One native performance-counter read supplies exact inputs, pressure and connection-churn projections.
use crate::read::{NativeRead, ReadError, invalid_data, optional_text, signed_integer, text};
use culprit_core::{
    MeasurementScope, ResetMarker, Result,
    telemetry::{CollectionStop, CounterNature, CounterUnit, NativeCounterSample, OperationalObservation, digest},
};
use std::collections::{BTreeSet, HashMap};
mod ratios;
pub(crate) use ratios::PerformanceRead;
pub(crate) const MAX_COUNTERS: usize = 32;
struct CounterSpec {
    family: Option<&'static str>,
    name: &'static str,
    nature: CounterNature,
    unit: CounterUnit,
}
const SPECS: &[CounterSpec] = &[
    CounterSpec {
        family: Some("SQL Statistics"),
        name: "Batch Requests/sec",
        nature: CounterNature::Cumulative,
        unit: CounterUnit::Count,
    },
    CounterSpec {
        family: Some("SQL Statistics"),
        name: "SQL Compilations/sec",
        nature: CounterNature::Cumulative,
        unit: CounterUnit::Count,
    },
    CounterSpec {
        family: Some("SQL Statistics"),
        name: "SQL Re-Compilations/sec",
        nature: CounterNature::Cumulative,
        unit: CounterUnit::Count,
    },
    CounterSpec {
        family: Some("Buffer Manager"),
        name: "Page life expectancy",
        nature: CounterNature::Gauge,
        unit: CounterUnit::Seconds,
    },
    CounterSpec {
        family: Some("General Statistics"),
        name: "Logins/sec",
        nature: CounterNature::Cumulative,
        unit: CounterUnit::Count,
    },
    CounterSpec {
        family: Some("General Statistics"),
        name: "Connection Reset/sec",
        nature: CounterNature::Cumulative,
        unit: CounterUnit::Count,
    },
    CounterSpec {
        family: None,
        name: "RBPEX cache hit ratio",
        nature: CounterNature::FractionNumerator,
        unit: CounterUnit::Count,
    },
    CounterSpec {
        family: None,
        name: "RBPEX cache hit ratio base",
        nature: CounterNature::FractionBase,
        unit: CounterUnit::Count,
    },
];
fn sql() -> String {
    // The selector grammar comes only from these fixed specifications, never from a database value or user input.
    let filter = SPECS
        .iter()
        .map(|spec| match spec.family {
            Some(family) => format!("(object_name LIKE N'%{family}%' AND counter_name=N'{}')", spec.name),
            None => format!("counter_name=N'{}'", spec.name),
        })
        .collect::<Vec<_>>()
        .join(" OR ");
    format!(
        "DECLARE @counter_observed_at datetime2(7)=SYSUTCDATETIME();
/* culprit */ SELECT RTRIM(object_name) AS native_object, RTRIM(counter_name) AS native_name,
RTRIM(instance_name) AS native_instance, cntr_value AS value, cntr_type AS native_type,
CONVERT(nvarchar(40),@counter_observed_at,127)+N'Z' AS counter_observed_at,
CONVERT(int,SERVERPROPERTY(N'EngineEdition')) AS engine_edition,
CONVERT(nvarchar(128),DATABASEPROPERTYEX(DB_NAME(),N'Edition')) AS database_edition,
CONVERT(nvarchar(128),DATABASEPROPERTYEX(DB_NAME(),N'ReplicaID')) AS replica_id
FROM sys.dm_os_performance_counters WHERE {filter} OPTION(MAXDOP 1)"
    )
}
pub(crate) async fn read<R: NativeRead>(reader: &mut R, limit: usize) -> Result<PerformanceRead> {
    let limit = limit.min(MAX_COUNTERS);
    let rows = reader.query(&sql(), vec![], limit + 1).await.map_err(ReadError::into_error)?;
    decode(&rows, limit)
}
fn decode(rows: &[crate::value::Rec], limit: usize) -> Result<PerformanceRead> {
    if rows.len() > limit {
        return Err(CollectionStop::RowLimit.error());
    }
    if rows.is_empty() {
        return Err(CollectionStop::SourceUnavailable.error());
    }
    let mut identities = BTreeSet::new();
    let mut counters = Vec::new();
    let mut rejected = Vec::new();
    for row in rows {
        let native_object = text(row, "native_object")?;
        let native_name = text(row, "native_name")?;
        let spec = SPECS
            .iter()
            .find(|spec| spec.name == native_name && spec.family.is_none_or(|family| native_object.contains(family)))
            .ok_or_else(invalid_data)?;
        let instance = optional_text(row, "native_instance")?.ok_or_else(invalid_data)?;
        let native_instance = (!instance.is_empty()).then_some(instance);
        let identity =
            serde_json::to_vec(&(&native_object, &native_name, &native_instance)).map_err(|_| invalid_data())?;
        if !identities.insert(identity.clone()) {
            return Err(invalid_data());
        }
        let key = digest(&identity);
        let code = i32::try_from(signed_integer(row, "native_type")?).map_err(|_| invalid_data())?;
        let value = signed_integer(row, "value")?;
        if value < 0 && spec.family.is_none() {
            rejected.push(culprit_core::telemetry::CounterRatioGap {
                key,
                raw_value: value.to_string(),
                native_object,
                native_name,
                native_instance,
                native_type: code.to_string(),
            });
            continue;
        }
        if value < 0 {
            return Err(invalid_data());
        }
        // These generic Windows native types are documented by sys.dm_os_performance_counters. A name alone
        // never supplies fraction semantics: unexpected native codes remain exact, unqualified inputs.
        // https://learn.microsoft.com/sql/relational-databases/system-dynamic-management-objects/sys-dm-os-performance-counters-transact-sql
        let nature = match code {
            272_696_320 | 272_696_576 => CounterNature::Cumulative,
            65_792 => CounterNature::Gauge,
            537_003_264 => CounterNature::FractionNumerator,
            1_073_939_712 => CounterNature::FractionBase,
            _ => CounterNature::Unknown,
        };
        let supported = nature == spec.nature;
        let counter = NativeCounterSample {
            key,
            value: culprit_core::telemetry::MeasurementValue::Integer(value.to_string()),
            unit: if supported { spec.unit } else { CounterUnit::Unknown },
            nature: if supported { nature } else { CounterNature::Unknown },
            scope: MeasurementScope::Server,
            reset: ResetMarker::Unknown,
            native_object,
            native_name,
            native_instance,
            native_type: Some(code.to_string()),
        };
        if !counter.is_valid() {
            return Err(invalid_data());
        }
        counters.push(counter);
    }
    ratios::captured(rows, counters, rejected)
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
        spec.family.is_some_and(|family| row.native_object.contains(family))
            && row.native_name == spec.name
            && row.nature == spec.nature
            && row.unit == spec.unit
            && row.native_instance.is_none()
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
pub(crate) fn observations(read: PerformanceRead) -> Result<Vec<OperationalObservation>> {
    Ok(vec![OperationalObservation::from_counters(culprit_core::source_id::PERFORMANCE_COUNTERS, read.sample)?])
}

#[cfg(test)]
pub(crate) mod tests;

#[cfg(test)]
mod native_tests;
