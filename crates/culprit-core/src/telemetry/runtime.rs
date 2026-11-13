//! Interval aggregates carry their actual units. Missing measurements remain absent.
use super::*;

pub const MAX_STATEMENT_METRICS: usize = 64;

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
}
impl NativeMeasurementKey {
    pub fn is_valid(&self) -> bool {
        self.native.is_valid() && self.references.is_valid()
    }
}
#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct RuntimeMeasurement {
    pub key: NativeMeasurementKey,
    pub executions: String,
    pub metrics: Vec<RuntimeMetric>,
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
    pub fn is_valid(&self) -> bool {
        let mut keys = std::collections::BTreeSet::new();
        self.key.is_valid()
            && valid_counter(&self.executions)
            && self.metrics.len() <= MAX_STATEMENT_METRICS
            && self.metrics.iter().all(|metric| metric.is_valid() && keys.insert(&metric.key))
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
}
impl WaitMeasurement {
    pub fn is_valid(&self) -> bool {
        self.key.is_valid()
            && valid_key(&self.category)
            && self.unit.is_time()
            && self.total.is_valid()
            && self.average.is_none_or(|value| value.is_finite() && value >= 0.0)
            && [&self.minimum, &self.maximum].into_iter().flatten().all(MeasurementValue::is_valid)
    }
}
