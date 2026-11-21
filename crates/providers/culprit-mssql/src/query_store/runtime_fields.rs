//! The raw Query Store runtime projection and strict decoder share one documented native field/unit roster.
use super::layout::NativeLayout;
use crate::{
    read::{invalid_data, optional_counter, optional_nonnegative_number},
    value::Rec,
};
use culprit_core::{
    Result,
    telemetry::{MeasurementValue, RuntimeMetric, RuntimeMetricKey as Key, RuntimeMetricUnit as Unit},
};

#[derive(Clone, Copy)]
enum Availability {
    Always,
    LogBytes,
    Tempdb,
}
struct MetricFields {
    name: &'static str,
    key: Key,
    unit: Unit,
    availability: Availability,
    deviation: bool,
}
fn fields() -> [MetricFields; 12] {
    let pages = Unit::Pages { bytes_per_page: super::decode::SQL_SERVER_PAGE_BYTES };
    [
        MetricFields {
            name: "duration",
            key: Key::Duration,
            unit: Unit::Microseconds,
            availability: Availability::Always,
            deviation: true,
        },
        MetricFields {
            name: "cpu_time",
            key: Key::CpuTime,
            unit: Unit::Microseconds,
            availability: Availability::Always,
            deviation: true,
        },
        MetricFields {
            name: "logical_io_reads",
            key: Key::LogicalReads,
            unit: pages,
            availability: Availability::Always,
            deviation: true,
        },
        MetricFields {
            name: "physical_io_reads",
            key: Key::PhysicalReads,
            unit: pages,
            availability: Availability::Always,
            deviation: true,
        },
        MetricFields {
            name: "logical_io_writes",
            key: Key::LogicalWrites,
            unit: pages,
            availability: Availability::Always,
            deviation: true,
        },
        MetricFields {
            name: "rowcount",
            key: Key::Rows,
            unit: Unit::Count,
            availability: Availability::Always,
            deviation: true,
        },
        MetricFields {
            name: "dop",
            key: Key::Parallelism,
            unit: Unit::Ratio,
            availability: Availability::Always,
            deviation: true,
        },
        MetricFields {
            name: "query_max_used_memory",
            key: Key::PeakMemory,
            unit: pages,
            availability: Availability::Always,
            deviation: true,
        },
        MetricFields {
            name: "clr_time",
            key: Key::Native("mssql.clr-time".into()),
            unit: Unit::Microseconds,
            availability: Availability::Always,
            deviation: true,
        },
        MetricFields {
            name: "log_bytes_used",
            key: Key::LogBytes,
            unit: Unit::Bytes,
            availability: Availability::LogBytes,
            deviation: true,
        },
        MetricFields {
            name: "tempdb_space_used",
            key: Key::TemporaryStorage,
            unit: pages,
            availability: Availability::Tempdb,
            deviation: true,
        },
        MetricFields {
            name: "num_physical_io_reads",
            key: Key::PhysicalReadOperations,
            unit: Unit::Count,
            availability: Availability::Always,
            deviation: false,
        },
    ]
}
impl MetricFields {
    fn supported(&self, layout: &NativeLayout) -> bool {
        match self.availability {
            Availability::Always => true,
            Availability::LogBytes => layout.log_bytes,
            Availability::Tempdb => layout.tempdb,
        }
    }
    fn columns(&self) -> impl Iterator<Item = (String, &'static str)> {
        [("avg", "float"), ("last", "bigint"), ("min", "bigint"), ("max", "bigint"), ("stdev", "float")]
            .into_iter()
            .filter(|(stat, _)| *stat != "stdev" || self.deviation)
            .map(|(stat, ty)| (format!("{stat}_{}", self.name), ty))
    }
    fn decode(self, row: &Rec) -> Result<RuntimeMetric> {
        let field = |stat| format!("{stat}_{}", self.name);
        let endpoint = |stat| optional_counter(row, &field(stat)).map(|value| value.map(MeasurementValue::Integer));
        let metric = RuntimeMetric {
            key: self.key.clone(),
            unit: self.unit,
            average: optional_nonnegative_number(row, &field("avg"))?,
            last: endpoint("last")?,
            minimum: endpoint("min")?,
            maximum: endpoint("max")?,
            standard_deviation: if self.deviation { optional_nonnegative_number(row, &field("stdev"))? } else { None },
        };
        metric.is_valid().then_some(metric).ok_or_else(invalid_data)
    }
}

pub(crate) fn columns(layout: &NativeLayout) -> String {
    fields()
        .iter()
        .flat_map(|metric| {
            metric
                .columns()
                .map(|(field, ty)| {
                    if metric.supported(layout) {
                        format!("r.{field}")
                    } else {
                        format!("CAST(NULL AS {ty}) AS {field}")
                    }
                })
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>()
        .join(",")
}
pub(crate) fn decode(row: &Rec) -> Result<Vec<RuntimeMetric>> {
    fields().into_iter().map(|metric| metric.decode(row)).collect()
}

#[cfg(test)]
pub(crate) fn with_nullable_columns(row: &Rec) -> Rec {
    use crate::value::Val;
    let mut values = row.columns().map(|(name, value)| (name.to_owned(), value.clone())).collect::<Vec<_>>();
    for (field, _) in fields().iter().flat_map(MetricFields::columns) {
        if !values.iter().any(|(name, _)| name == &field) {
            values.push((field, Val::Null));
        }
    }
    Rec::of(&values.iter().map(|(name, value)| (name.as_str(), value.clone())).collect::<Vec<_>>())
}
#[cfg(test)]
mod tests;
