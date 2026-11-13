//! Native units, scopes and aggregation are defined once, before either consumer renders or exports them.
use crate::{
    read::{integer, invalid_data},
    value::{Rec, Val},
};
use ResourceLimitBasis::{Absolute, DatabaseLimit, InstanceLimit, PoolLimit};
use ResourceStatistic::{Maximum, Mean, Point};
use ResourceUnit::{Cores, DatabaseTransactionUnits, Percent};
use culprit_core::MeasurementScope::{Database, Server};
use culprit_core::{Result, telemetry::*};

#[derive(Clone, Copy)]
#[repr(usize)]
pub(crate) enum Metric {
    Cpu,
    DataIo,
    LogWrite,
    Memory,
    Workers,
    Sessions,
    XtpStorage,
    InstanceCpu,
    InstanceMemory,
    DtuLimit,
    CpuLimit,
}

const METRIC_COUNT: usize = 11;

struct ResourceField {
    metric: Metric,
    column: &'static str,
    key: &'static str,
    scope: culprit_core::MeasurementScope,
    unit: ResourceUnit,
    statistic: ResourceStatistic,
    period_ms: Option<u32>,
    limit_basis: ResourceLimitBasis,
}
const FIELDS: [ResourceField; METRIC_COUNT] = [
    ResourceField {
        metric: Metric::Cpu,
        column: "avg_cpu_percent",
        key: "cpu",
        scope: Database,
        unit: Percent,
        statistic: Mean,
        period_ms: Some(super::SAMPLE_PERIOD_MS),
        limit_basis: DatabaseLimit,
    },
    ResourceField {
        metric: Metric::DataIo,
        column: "avg_data_io_percent",
        key: "data-io",
        scope: Database,
        unit: Percent,
        statistic: Mean,
        period_ms: Some(super::SAMPLE_PERIOD_MS),
        limit_basis: DatabaseLimit,
    },
    ResourceField {
        metric: Metric::LogWrite,
        column: "avg_log_write_percent",
        key: "log-write",
        scope: Database,
        unit: Percent,
        statistic: Mean,
        period_ms: Some(super::SAMPLE_PERIOD_MS),
        limit_basis: DatabaseLimit,
    },
    ResourceField {
        metric: Metric::Memory,
        column: "avg_memory_usage_percent",
        key: "memory",
        scope: Database,
        unit: Percent,
        statistic: Mean,
        period_ms: Some(super::SAMPLE_PERIOD_MS),
        limit_basis: DatabaseLimit,
    },
    ResourceField {
        metric: Metric::Workers,
        column: "max_worker_percent",
        key: "workers",
        scope: Database,
        unit: Percent,
        statistic: Maximum,
        period_ms: Some(super::SAMPLE_PERIOD_MS),
        limit_basis: DatabaseLimit,
    },
    ResourceField {
        metric: Metric::Sessions,
        column: "max_session_percent",
        key: "sessions",
        scope: Database,
        unit: Percent,
        statistic: Maximum,
        period_ms: Some(super::SAMPLE_PERIOD_MS),
        limit_basis: DatabaseLimit,
    },
    ResourceField {
        metric: Metric::XtpStorage,
        column: "xtp_storage_percent",
        key: "xtp-storage",
        scope: Database,
        unit: Percent,
        statistic: Point,
        period_ms: None,
        limit_basis: PoolLimit,
    },
    ResourceField {
        metric: Metric::InstanceCpu,
        column: "avg_instance_cpu_percent",
        key: "cpu",
        scope: Server,
        unit: Percent,
        statistic: Mean,
        period_ms: Some(60_000),
        limit_basis: InstanceLimit,
    },
    ResourceField {
        metric: Metric::InstanceMemory,
        column: "avg_instance_memory_percent",
        key: "memory",
        scope: Server,
        unit: Percent,
        statistic: Mean,
        period_ms: None,
        limit_basis: InstanceLimit,
    },
    ResourceField {
        metric: Metric::DtuLimit,
        column: "dtu_limit",
        key: "dtu-limit",
        scope: Database,
        unit: DatabaseTransactionUnits,
        statistic: Point,
        period_ms: None,
        limit_basis: Absolute,
    },
    ResourceField {
        metric: Metric::CpuLimit,
        column: "cpu_limit",
        key: "cpu-limit",
        scope: Database,
        unit: Cores,
        statistic: Point,
        period_ms: None,
        limit_basis: Absolute,
    },
];

#[derive(Clone)]
pub(crate) struct Sample {
    pub at: String,
    pub replica_role: i64,
    values: [Option<f64>; METRIC_COUNT],
}
impl Sample {
    pub(super) fn decode(row: &Rec) -> Result<Self> {
        let mut values = [None; METRIC_COUNT];
        for field in &FIELDS {
            values[field.metric as usize] = number(row, field.column)?;
        }
        Ok(Self {
            at: canonical_time(&crate::read::text(row, "sample_at")?)?,
            replica_role: integer(row, "replica_role")?,
            values,
        })
    }
    pub(crate) fn value(&self, metric: Metric) -> Option<f64> {
        self.values[metric as usize]
    }
    pub(crate) fn exported(&self) -> ResourceMeasurement {
        ResourceMeasurement {
            at: self.at.clone(),
            replica_id: Some(self.replica_role.to_string()),
            metrics: FIELDS
                .iter()
                .map(|field| ResourceMetricSample {
                    key: field.key.into(),
                    scope: field.scope,
                    unit: field.unit,
                    statistic: field.statistic,
                    period_ms: field.period_ms,
                    limit_basis: field.limit_basis,
                    value: self.value(field.metric),
                })
                .collect(),
        }
    }
    pub(crate) fn snapshot(&self) -> culprit_core::ResourceSample {
        culprit_core::ResourceSample {
            at: self.at.clone(),
            cpu_pct: self.value(Metric::Cpu),
            data_io_pct: self.value(Metric::DataIo),
            log_write_pct: self.value(Metric::LogWrite),
            memory_pct: self.value(Metric::Memory),
            workers_pct: self.value(Metric::Workers),
        }
    }
}
pub(super) fn columns() -> String {
    std::iter::once("replica_role").chain(FIELDS.iter().map(|field| field.column)).collect::<Vec<_>>().join(",")
}
pub(super) fn number(row: &Rec, name: &str) -> Result<Option<f64>> {
    match row.columns().find(|(column, _)| *column == name).map(|(_, value)| value) {
        Some(Val::Null) => Ok(None),
        Some(Val::Int(value)) if *value >= 0 => Ok(Some(*value as f64)),
        Some(Val::Float(value)) if value.is_finite() && *value >= 0.0 => Ok(Some(*value)),
        _ => Err(invalid_data()),
    }
}
