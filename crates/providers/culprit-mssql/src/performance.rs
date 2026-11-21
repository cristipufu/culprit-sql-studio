//! One native performance-counter read supplies exact inputs, pressure and connection-churn projections.
use crate::read::{NativeRead, ReadError, invalid_data, optional_text, signed_integer, text};
use culprit_core::{
    MeasurementScope, ResetMarker, Result,
    telemetry::{
        CollectionStop, CounterNature, MAX_NATIVE_COUNTERS, NativeCounterSample, OperationalObservation,
        PerformanceCounterInstances, PerformanceCounterSpec, digest,
    },
};
use std::collections::{BTreeSet, HashMap};
mod database;
#[cfg(test)]
pub(crate) use database::tests as test_support;
mod ratios;
mod resource_governor;
mod row;
pub(crate) use database::read_current;
pub(crate) use ratios::PerformanceRead;
pub(crate) use resource_governor::read as read_for_instance;
pub(crate) const MAX_COUNTERS: usize = MAX_NATIVE_COUNTERS;
const COUNTER_COLUMNS: &str = "RTRIM(object_name) AS native_object, RTRIM(counter_name) AS native_name,\nRTRIM(instance_name) AS native_instance, cntr_value AS value, cntr_type AS native_type";
#[derive(Clone, Copy)]
enum Population<'a> {
    Shared,
    InstanceInvestigation,
    Database(&'a crate::local_source::Context),
}
impl Population<'_> {
    fn scope(self) -> MeasurementScope {
        match self {
            Self::Shared | Self::InstanceInvestigation => MeasurementScope::Server,
            Self::Database(_) => MeasurementScope::Database,
        }
    }
    fn field(self, object: &str, name: &str, instance: Option<&str>) -> Option<&'static PerformanceCounterSpec> {
        match self {
            Self::Shared => specs().iter().find(|spec| spec.matches(object, name, instance)),
            Self::InstanceInvestigation => {
                specs().iter().find(|spec| spec.matches(object, name, instance)).or_else(|| {
                    let profile = culprit_core::Engine::Mssql.spec().performance_counters()?;
                    profile
                        .common_field(object, name, instance)
                        .or_else(|| profile.detailed_field(object, name, instance))
                })
            }
            Self::Database(owner) => {
                if instance != Some(owner.activity.database.database.as_str()) {
                    return None;
                }
                culprit_core::Engine::Mssql.spec().performance_counters()?.database_field(object, name)
            }
        }
    }
    fn selection(self) -> &'static str {
        match self {
            Self::InstanceInvestigation => culprit_core::telemetry::MSSQL_COMMON_COUNTER_PROFILE,
            Self::Shared | Self::Database(_) => culprit_core::telemetry::PERFORMANCE_COUNTER_PROFILE,
        }
    }
}

fn specs() -> &'static [PerformanceCounterSpec] {
    culprit_core::Engine::Mssql.spec().performance_counters().map(|profile| profile.fields).unwrap_or_default()
}
fn sql() -> String {
    query(false)
}
fn query(governed: bool) -> String {
    // The selector grammar comes only from these fixed specifications, never from a database value or user input.
    let investigation = culprit_core::Engine::Mssql
        .spec()
        .performance_counters()
        .map(|profile| profile.investigation_fields)
        .unwrap_or_default();
    let common = culprit_core::Engine::Mssql
        .spec()
        .performance_counters()
        .map(|profile| profile.common_fields)
        .unwrap_or_default();
    let detailed = culprit_core::Engine::Mssql
        .spec()
        .performance_counters()
        .map(|profile| profile.detailed_fields)
        .unwrap_or_default();
    let selected = specs()
        .iter()
        .chain(common.iter().filter(|_| governed))
        .chain(investigation.iter().filter(|_| governed))
        .map(|spec| (spec, false))
        .chain(detailed.iter().filter(|_| governed).map(|spec| (spec, true)));
    let filter =
        selected.map(|(spec, exact_object)| counter_predicate(spec, exact_object)).collect::<Vec<_>>().join(" OR ");
    let extra = if governed { resource_governor::columns() } else { String::new() };
    let relation = if governed { resource_governor::FROM } else { "FROM sys.dm_os_performance_counters" };
    format!(
        "DECLARE @counter_observed_at datetime2(7)=SYSUTCDATETIME();
/* culprit */ SELECT {COUNTER_COLUMNS},
CONVERT(nvarchar(40),@counter_observed_at,127)+N'Z' AS counter_observed_at,
CONVERT(int,SERVERPROPERTY(N'EngineEdition')) AS engine_edition,
CONVERT(nvarchar(128),DATABASEPROPERTYEX(DB_NAME(),N'Edition')) AS database_edition,
CONVERT(nvarchar(128),DATABASEPROPERTYEX(DB_NAME(),N'ReplicaID')) AS replica_id{extra}
{relation} WHERE {filter} OPTION(MAXDOP 1)"
    )
}
fn counter_predicate(spec: &PerformanceCounterSpec, exact_object: bool) -> String {
    let family = spec
        .object_family
        .map(|family| {
            if exact_object {
                format!("RIGHT(RTRIM(object_name),LEN(N':{family}'))=N':{family}' AND ")
            } else {
                format!("object_name LIKE N'%{family}%' AND ")
            }
        })
        .unwrap_or_default();
    let instance = match spec.instances {
        PerformanceCounterInstances::Uninstanced => " AND RTRIM(instance_name)=N''".into(),
        PerformanceCounterInstances::Named(names) => format!(
            " AND RTRIM(instance_name) IN ({})",
            names.iter().map(|name| format!("N'{name}'")).collect::<Vec<_>>().join(",")
        ),
        PerformanceCounterInstances::Any => String::new(),
    };
    format!("({family}counter_name=N'{}'{instance})", spec.native_name)
}
pub(crate) async fn read<R: NativeRead>(reader: &mut R, limit: usize) -> Result<PerformanceRead> {
    if specs().is_empty() {
        return Err(CollectionStop::SourceUnavailable.error());
    }
    let limit = limit.min(MAX_COUNTERS);
    let rows = reader.query(&sql(), vec![], limit + 1).await.map_err(ReadError::into_error)?;
    decode(&rows, limit, Population::Shared)
}
fn decode(rows: &[crate::value::Rec], limit: usize, population: Population<'_>) -> Result<PerformanceRead> {
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
        let input = row::CounterInput::decode(row)?;
        let spec = population.field(input.object(), input.name(), input.instance()).ok_or_else(invalid_data)?;
        if !identities.insert(input.identity().to_owned()) {
            return Err(invalid_data());
        }
        match input.measurement(spec, population.scope())? {
            row::CounterMeasurement::Value(counter) => counters.push(counter),
            row::CounterMeasurement::Rejected(gap) => rejected.push(gap),
        }
    }
    match population {
        Population::Shared | Population::InstanceInvestigation => {
            ratios::captured(rows, counters, rejected, population)
        }
        Population::Database(owner) => database::captured(rows, counters, owner),
    }
}
pub(crate) fn application(rows: &[NativeCounterSample]) -> HashMap<String, f64> {
    let mut values = HashMap::new();
    for spec in specs().iter().filter(|spec| spec.application) {
        if let Some(value) = global(rows, spec).and_then(|row| row.value.integer()) {
            values.insert(spec.native_name.into(), value as f64);
        }
    }
    values
}
fn global<'a>(rows: &'a [NativeCounterSample], spec: &PerformanceCounterSpec) -> Option<&'a NativeCounterSample> {
    let mut matching = rows.iter().filter(|row| {
        spec.object_family.is_some_and(|family| row.native_object.contains(family))
            && row.native_name == spec.native_name
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
    specs()
        .iter()
        .filter(|spec| spec.application && spec.nature == CounterNature::Cumulative)
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
            Some((spec.native_name.into(), delta as f64 / elapsed_seconds))
        })
        .collect()
}
pub(crate) fn observations(read: PerformanceRead) -> Result<Vec<OperationalObservation>> {
    std::iter::once(read.sample)
        .chain(read.resource_governor)
        .map(|sample| OperationalObservation::from_counters(culprit_core::source_id::PERFORMANCE_COUNTERS, sample))
        .collect()
}

#[cfg(test)]
pub(crate) mod tests;

#[cfg(test)]
mod native_tests;
