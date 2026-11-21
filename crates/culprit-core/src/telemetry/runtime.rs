//! Interval aggregates carry their actual units. Missing measurements remain absent.
use super::*;
mod page_server_reads;
pub use page_server_reads::{PageServerReadAvailability, PageServerReadStatistics, PageServerReadSummary};
#[cfg(test)]
mod cohort_tests;
#[cfg(test)]
mod wait_fragment_tests;

pub const MAX_STATEMENT_METRICS: usize = 64;
/// Query Store describes wait categories and execution outcomes with nvarchar(128).
pub const MAX_NATIVE_WAIT_DESCRIPTION_UNITS: usize = 128;

fn required_nullable<'de, D: serde::Deserializer<'de>, T: Deserialize<'de>>(
    reader: D,
) -> std::result::Result<Option<T>, D::Error> {
    Option::<T>::deserialize(reader)
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq)]
#[serde(tag = "kind", content = "value", rename_all = "kebab-case", deny_unknown_fields)]
#[ts(export)]
pub enum MeasurementValue {
    /// Decimal wire encoding preserves integers above JavaScript's exact range.
    Integer(String),
    /// Some engines expose cumulative execution time as a fractional native number.
    Real(f64),
}
impl MeasurementValue {
    pub fn integer(&self) -> Option<u64> {
        match self {
            Self::Integer(value) => value.parse().ok(),
            Self::Real(_) => None,
        }
    }

    pub(super) fn is_valid(&self) -> bool {
        match self {
            Self::Integer(value) => valid_counter(value),
            Self::Real(value) => value.is_finite() && *value >= 0.0,
        }
    }
}
#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(tag = "kind", content = "name", rename_all = "kebab-case")]
#[ts(export)]
pub enum RuntimeMetricKey {
    Executions,
    /// The source's exact cumulative statement error count, without an inferred reason or failing request.
    Errors,
    /// The source's exact cumulative warning count; warnings are not failed executions.
    Warnings,
    Duration,
    CpuTime,
    LogicalReads,
    PhysicalReads,
    LogicalWrites,
    Rows,
    Parallelism,
    /// Per-execution peak memory; an average of peaks is distinct from average resident memory.
    PeakMemory,
    LogBytes,
    /// Per-execution average temporary-space use, in native bytes or pages. Weighting it by executions yields
    /// used-space observations, never write traffic, simultaneous allocation or a measured peak.
    TemporaryStorage,
    TemporaryBlocksRead,
    TemporaryBlocksWritten,
    TemporaryReadTime,
    TemporaryWriteTime,
    TemporaryTables,
    TemporaryDiskTables,
    SortMergePasses,
    Native(String),
}
#[derive(Serialize, Deserialize, TS, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "kebab-case")]
#[ts(export)]
pub enum RuntimeMetricUnit {
    Picoseconds,
    Nanoseconds,
    Microseconds,
    Milliseconds,
    Seconds,
    Count,
    Bytes,
    Pages { bytes_per_page: u32 },
    Ratio,
}
impl RuntimeMetricUnit {
    pub fn is_valid(self) -> bool {
        !matches!(self, Self::Pages { bytes_per_page: 0 })
    }
    pub(super) fn is_time(self) -> bool {
        matches!(self, Self::Picoseconds | Self::Nanoseconds | Self::Microseconds | Self::Milliseconds | Self::Seconds)
    }
}
impl RuntimeMetricKey {
    pub fn accepts(&self, unit: RuntimeMetricUnit) -> bool {
        unit.is_valid()
            && match self {
                Self::Duration | Self::CpuTime | Self::TemporaryReadTime | Self::TemporaryWriteTime => unit.is_time(),
                Self::Executions
                | Self::Errors
                | Self::Warnings
                | Self::Rows
                | Self::TemporaryTables
                | Self::TemporaryDiskTables
                | Self::SortMergePasses => unit == RuntimeMetricUnit::Count,
                Self::TemporaryBlocksRead | Self::TemporaryBlocksWritten => {
                    matches!(unit, RuntimeMetricUnit::Pages { .. })
                }
                Self::LogicalReads | Self::PhysicalReads | Self::LogicalWrites => matches!(
                    unit,
                    RuntimeMetricUnit::Count | RuntimeMetricUnit::Bytes | RuntimeMetricUnit::Pages { .. }
                ),
                Self::PeakMemory | Self::TemporaryStorage => {
                    matches!(unit, RuntimeMetricUnit::Bytes | RuntimeMetricUnit::Pages { .. })
                }
                Self::LogBytes => unit == RuntimeMetricUnit::Bytes,
                Self::Parallelism => unit == RuntimeMetricUnit::Ratio,
                Self::Native(name) => valid_key(name),
            }
    }
}
#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct RuntimeMetric {
    pub key: RuntimeMetricKey,
    pub unit: RuntimeMetricUnit,
    pub average: f64,
    pub minimum: Option<f64>,
    pub maximum: Option<f64>,
    pub standard_deviation: Option<f64>,
}
impl RuntimeMetric {
    pub fn average(key: RuntimeMetricKey, unit: RuntimeMetricUnit, average: f64) -> Self {
        Self { key, unit, average, minimum: None, maximum: None, standard_deviation: None }
    }
    pub fn is_valid(&self) -> bool {
        let number = |value: f64| value.is_finite() && value >= 0.0;
        self.key.accepts(self.unit)
            && number(self.average)
            && [self.minimum, self.maximum, self.standard_deviation].into_iter().flatten().all(number)
    }
}
#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq, Hash)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct NativeMeasurementKey {
    pub native: NativeIdentity,
    pub references: StatementReferences,
    /// Source-owned comparison population inside this exact observation. Some asserts disjoint fragments and
    /// preserves outcome, replica, reset and context distinctions independently of plan and native row identity.
    /// None authorizes no aggregation beyond the complete native references and qualifiers.
    pub cohort: Option<NativeIdentity>,
}
impl NativeMeasurementKey {
    pub fn is_valid(&self) -> bool {
        self.native.is_valid()
            && self.references.is_valid()
            && self.cohort.as_ref().is_none_or(NativeIdentity::is_valid)
    }
}
#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct RuntimeMeasurement {
    pub key: NativeMeasurementKey,
    pub executions: String,
    pub metrics: Vec<RuntimeMetric>,
    /// Source-qualified remote 8-KiB page reads for this exact native runtime fragment. Native integer
    /// statistics remain exact and independent from physical/local-cache reads and execution time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub page_server_reads: Option<Box<PageServerReadStatistics>>,
    /// UTC instants, normalized at the shared export boundary without changing their nanosecond precision.
    pub first_execution_at: Option<String>,
    pub last_execution_at: Option<String>,
}
impl RuntimeMeasurement {
    pub(super) fn canonicalize_instants(&mut self) -> Result<()> {
        for instant in [&mut self.first_execution_at, &mut self.last_execution_at].into_iter().flatten() {
            *instant = canonical_time(instant)?;
        }
        Ok(())
    }

    pub fn metric(&self, key: &RuntimeMetricKey) -> Option<&RuntimeMetric> {
        self.metrics.iter().find(|metric| &metric.key == key)
    }
    pub fn page_server_reads_match_source(&self, source: &CollectionDescription) -> bool {
        let declared = source.query_history().and_then(|history| {
            history.native_properties.get(PageServerReadAvailability::PROPERTY).map(String::as_str)
        });
        match (declared, self.page_server_reads.as_deref()) {
            (None, None) => true,
            (Some(declared), Some(statistics)) => {
                PageServerReadAvailability::from_native_property(declared) == Some(statistics.availability)
            }
            _ => false,
        }
    }
    pub fn is_valid(&self) -> bool {
        let mut keys = std::collections::BTreeSet::new();
        self.key.is_valid()
            && valid_counter(&self.executions)
            && self.metrics.len() <= MAX_STATEMENT_METRICS
            && self.metrics.iter().all(|metric| metric.is_valid() && keys.insert(&metric.key))
            && self.page_server_reads.as_ref().is_none_or(|statistics| statistics.is_valid())
            && [&self.first_execution_at, &self.last_execution_at]
                .into_iter()
                .flatten()
                .all(|at| super::identity::valid_utc_instant(at))
    }
}
#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct WaitMeasurement {
    pub key: NativeMeasurementKey,
    /// Source-native category identity, independent of a localized label or a particular engine's numeric codes.
    pub category: String,
    pub unit: RuntimeMetricUnit,
    pub total: MeasurementValue,
    pub average: Option<f64>,
    pub minimum: Option<MeasurementValue>,
    pub maximum: Option<MeasurementValue>,
    /// Last value in this original native fragment; no order across independently sampled fragments is implied.
    #[serde(deserialize_with = "required_nullable")]
    pub last: Option<MeasurementValue>,
    /// Native standard deviation in `unit`, never inferred from another fragment or from a total.
    #[serde(deserialize_with = "required_nullable")]
    pub standard_deviation: Option<f64>,
    /// Original native labels, independent of the category/outcome identities used by interpretation.
    #[serde(deserialize_with = "required_nullable")]
    pub category_description: Option<String>,
    #[serde(deserialize_with = "required_nullable")]
    pub outcome_description: Option<String>,
}
impl WaitMeasurement {
    pub fn is_valid(&self) -> bool {
        self.key.is_valid()
            && valid_key(&self.category)
            && self.unit.is_time()
            && self.total.is_valid()
            && self.average.is_none_or(|value| value.is_finite() && value >= 0.0)
            && [&self.minimum, &self.maximum, &self.last].into_iter().flatten().all(MeasurementValue::is_valid)
            && self.standard_deviation.is_none_or(|value| value.is_finite() && value >= 0.0)
            && [&self.category_description, &self.outcome_description]
                .into_iter()
                .flatten()
                .all(|value| value.encode_utf16().count() <= MAX_NATIVE_WAIT_DESCRIPTION_UNITS)
    }
}
