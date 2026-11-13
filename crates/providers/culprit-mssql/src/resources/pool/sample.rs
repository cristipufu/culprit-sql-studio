use super::*;
use NativeValue::{Cores, DtuPercent, Megabytes, Percent};
use ResourceLimitBasis::{Absolute, InstanceLimit, PoolLimit};
use ResourceStatistic::{Maximum, Mean, Point};
use culprit_core::MeasurementScope;
use culprit_core::MeasurementScope::{Pool, Server};

const BYTES_PER_SQL_MEGABYTE: f64 = 1024.0 * 1024.0;

#[derive(Clone, Copy)]
enum NativeValue {
    Percent,
    Cores,
    Megabytes,
    DtuPercent,
}
impl NativeValue {
    fn unit(self) -> ResourceUnit {
        match self {
            Self::Percent | Self::DtuPercent => ResourceUnit::Percent,
            Self::Cores => ResourceUnit::Cores,
            Self::Megabytes => ResourceUnit::Bytes,
        }
    }
    fn read(self, row: &Rec, column: &str, pricing: Pricing) -> Result<Option<f64>> {
        let value = super::super::sample::number(row, column)?;
        Ok(match self {
            Self::DtuPercent if pricing != Pricing::Dtu => None,
            Self::Megabytes => value.map(|value| value * BYTES_PER_SQL_MEGABYTE),
            _ => value,
        })
    }
}
pub(crate) fn columns() -> String {
    FIELDS.iter().map(|field| field.column).collect::<Vec<_>>().join(",")
}

struct Field {
    column: &'static str,
    key: &'static str,
    scope: MeasurementScope,
    native: NativeValue,
    statistic: ResourceStatistic,
    limit: ResourceLimitBasis,
}

const FIELDS: &[Field] = &[
    Field { column: "avg_cpu_percent", key: "cpu", scope: Pool, native: Percent, statistic: Mean, limit: PoolLimit },
    Field {
        column: "avg_data_io_percent",
        key: "data_io",
        scope: Pool,
        native: Percent,
        statistic: Mean,
        limit: PoolLimit,
    },
    Field {
        column: "avg_log_write_percent",
        key: "log_write",
        scope: Pool,
        native: Percent,
        statistic: Mean,
        limit: PoolLimit,
    },
    Field {
        column: "max_worker_percent",
        key: "workers",
        scope: Pool,
        native: Percent,
        statistic: Maximum,
        limit: PoolLimit,
    },
    Field {
        column: "max_session_percent",
        key: "sessions",
        scope: Pool,
        native: Percent,
        statistic: Maximum,
        limit: PoolLimit,
    },
    Field {
        column: "avg_instance_memory_percent",
        key: "instance_memory",
        scope: Server,
        native: Percent,
        statistic: Mean,
        limit: InstanceLimit,
    },
    Field {
        column: "avg_instance_cpu_percent",
        key: "instance_cpu",
        scope: Server,
        native: Percent,
        statistic: Mean,
        limit: InstanceLimit,
    },
    Field {
        column: "avg_edtu_percent",
        key: "edtu",
        scope: Pool,
        native: DtuPercent,
        statistic: Mean,
        limit: PoolLimit,
    },
    Field {
        column: "instance_vcores",
        key: "instance_vcores",
        scope: Server,
        native: Cores,
        statistic: Point,
        limit: Absolute,
    },
    Field {
        column: "used_storage_mb",
        key: "used_storage",
        scope: Pool,
        native: Megabytes,
        statistic: Point,
        limit: Absolute,
    },
    Field {
        column: "allocated_storage_mb",
        key: "allocated_storage",
        scope: Pool,
        native: Megabytes,
        statistic: Point,
        limit: Absolute,
    },
    Field {
        column: "storage_limit_mb",
        key: "storage_limit",
        scope: Pool,
        native: Megabytes,
        statistic: Point,
        limit: Absolute,
    },
    Field {
        column: "used_storage_percent",
        key: "used_storage_percent",
        scope: Pool,
        native: Percent,
        statistic: Point,
        limit: PoolLimit,
    },
    Field {
        column: "allocated_storage_percent",
        key: "allocated_storage_percent",
        scope: Pool,
        native: Percent,
        statistic: Point,
        limit: PoolLimit,
    },
];

pub(super) fn decode(row: &Rec, pricing: Pricing) -> Result<ResourceMeasurement> {
    let metrics = FIELDS
        .iter()
        .map(|field| {
            let value = field.native.read(row, field.column, pricing)?;
            Ok(ResourceMetricSample {
                key: field.key.into(),
                scope: field.scope,
                unit: field.native.unit(),
                statistic: field.statistic,
                period_ms: None,
                limit_basis: field.limit,
                value,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    let sample = ResourceMeasurement { at: canonical_time(&text(row, "sample_at")?)?, replica_id: None, metrics };
    if !sample.is_valid() {
        return Err(invalid_data());
    }
    Ok(sample)
}
