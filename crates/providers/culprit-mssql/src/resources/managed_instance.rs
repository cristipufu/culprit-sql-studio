//! Documented MI service history uses one bounded selected-window read. Native labels do not identify a host.
use crate::{
    instance,
    read::{NativeRead, NativeStreamRead, Parameter, ReadError, invalid_data, optional_integer, optional_text, text},
    value::Rec,
};
use culprit_core::{ErrorCode, MeasurementScope, Result, telemetry::*};
mod layout;
mod sql;
pub(crate) use layout::Layout;

pub(crate) async fn layout<R: NativeRead>(reader: &mut R, major: u32) -> Result<Layout> {
    let rows = reader.query(&sql::layout(), vec![], layout::Field::ALL.len()).await.map_err(ReadError::into_error)?;
    Layout::decode(&rows, major)
}
pub(crate) struct Read {
    pub observed_at: String,
    pub records: Vec<ResourceMeasurement>,
    pub native_rows: u32,
    pub limited: bool,
}
pub(crate) async fn read<R: NativeStreamRead>(
    reader: &mut R,
    window: &super::Window,
    layout: &Layout,
    limit: u32,
    owner: &instance::Context,
) -> Result<Read> {
    if !layout.available() {
        return Err(CollectionStop::SourceUnavailable.error());
    }
    let limit = limit.min(MAX_MSSQL_MANAGED_INSTANCE_RESOURCE_ROWS);
    if limit == 0 {
        return Err(CollectionStop::RowLimit.error());
    }
    reader
        .begin_stream(
            &sql::samples(layout),
            vec![
                Parameter::Text(window.from.to_rfc3339()),
                Parameter::Text(window.until.to_rfc3339()),
                Parameter::Integer(i64::from(limit) + 1),
            ],
            limit as usize + 1,
        )
        .await
        .map_err(ReadError::into_error)?;
    let mut rows = Vec::new();
    loop {
        let batch = reader.next_batch().await.map_err(ReadError::into_error)?;
        if batch.rows.is_empty() && !batch.complete {
            return Err(invalid_data());
        }
        if rows.len().saturating_add(batch.rows.len()) > limit as usize + 1 {
            return Err(CollectionStop::RowLimit.error());
        }
        rows.extend(batch.rows);
        if batch.complete {
            break;
        }
    }
    if rows.is_empty() || rows.len() > limit as usize + 1 {
        return Err(invalid_data());
    }
    let mut observed = None;
    let mut records = Vec::new();
    let mut previous = None;
    for row in &rows {
        let observer = instance::observation(row)?;
        if observer.context_from_epoch(row)?.as_ref() != Some(owner) {
            return Err(CollectionStop::SourceChanged.error());
        }
        let at = canonical_time(&observer.observed_at)?;
        if observed.as_ref().is_some_and(|previous| previous != &at) {
            return Err(CollectionStop::SourceChanged.error());
        }
        observed = Some(at);
        match optional_integer(row, "present")? {
            None => {
                if rows.len() != 1 {
                    return Err(invalid_data());
                }
                for field in layout::Field::ALL {
                    if optional_text(row, field.name())?.is_some() {
                        return Err(invalid_data());
                    }
                }
            }
            Some(1) => {
                let native = sample(row)?;
                let at = canonical_time(&native.ended_at)?;
                if !window.contains(&at)? || previous.as_ref().is_some_and(|last| last >= &at) {
                    return Err(invalid_data());
                }
                previous = Some(at.clone());
                let value = native
                    .avg_cpu_percent
                    .as_deref()
                    .map(|value| value.parse::<f64>().map_err(|_| invalid_data()))
                    .transpose()?;
                let measurement = ResourceMeasurement {
                    mssql_managed_instance: Some(Box::new(native)),
                    at,
                    data_io_accounting: DataIoAccounting::Unknown,
                    replica_id: None,
                    replica_role: None,
                    metrics: vec![ResourceMetricSample {
                        key: "cpu".into(),
                        unit: ResourceUnit::Percent,
                        scope: MeasurementScope::Server,
                        statistic: ResourceStatistic::Mean,
                        period_ms: Some(MSSQL_MANAGED_INSTANCE_RESOURCE_PERIOD_MS),
                        limit_basis: ResourceLimitBasis::InstanceLimit,
                        value,
                    }],
                };
                if !measurement.is_valid()
                    || !measurement
                        .mssql_managed_instance
                        .as_ref()
                        .is_some_and(|native| native.fits_layout(&layout.native))
                {
                    return Err(invalid_data());
                }
                records.push(measurement);
            }
            _ => return Err(invalid_data()),
        }
    }
    let native_rows = records.len() as u32;
    let limited = native_rows > limit;
    records.truncate(limit as usize);
    Ok(Read { observed_at: observed.ok_or_else(invalid_data)?, records, native_rows, limited })
}
fn sample(row: &Rec) -> Result<MssqlManagedInstanceResourceSample> {
    Ok(MssqlManagedInstanceResourceSample {
        started_at: canonical_time(&text(row, "start_time")?)?,
        ended_at: canonical_time(&text(row, "end_time")?)?,
        avg_cpu_percent: optional_text(row, "avg_cpu_percent")?,
        virtual_core_count: optional_text(row, "virtual_core_count")?,
        reserved_storage_mb: optional_text(row, "reserved_storage_mb")?,
        storage_space_used_mb: optional_text(row, "storage_space_used_mb")?,
        io_request: optional_text(row, "io_request")?,
        io_bytes_read: optional_text(row, "io_bytes_read")?,
        io_bytes_written: optional_text(row, "io_bytes_written")?,
        resource_type: optional_text(row, "resource_type")?,
        resource_name: optional_text(row, "resource_name")?,
        sku: optional_text(row, "sku")?,
        hardware_generation: optional_text(row, "hardware_generation")?,
    })
}
#[cfg(test)]
pub(crate) mod tests;
