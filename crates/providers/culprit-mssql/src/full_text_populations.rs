//! One selected native runtime reader. It never starts indexing, scans user data or walks population ranges.
use crate::{
    read::{
        NativeRead, Parameter, ReadError, integer, invalid_data, optional_boolean, optional_integer,
        optional_signed_integer, optional_text, text,
    },
    value::{Rec, Val},
};
use culprit_core::{Error, ErrorCode, Result, telemetry::*};
mod sql;
const RUNTIME_FIELDS: &[&str] = &[
    "population_database_id",
    "population_table_id",
    "population_catalog_id",
    "population_type",
    "population_type_description",
    "status",
    "status_description",
    "completion_type",
    "completion_type_description",
    "queued_population_type",
    "queued_population_type_description",
    "is_clustered_index_scan",
    "range_count",
    "completed_range_count",
    "outstanding_batch_count",
    "started_without_offset",
];
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum StatePermission {
    ServerState,
    ServerPerformanceState,
    AzureDatabase,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TargetPoint {
    pub metadata: MssqlFullTextPopulationMetadata,
}
fn parameters(target: &MssqlFullTextPopulationTarget, limit: u32) -> Vec<Parameter> {
    vec![
        Parameter::Integer(i64::from(limit) + 1),
        Parameter::Integer(i64::from(target.table.object_id)),
        Parameter::Integer(i64::from(target.catalog_id)),
        Parameter::Integer(i64::from(target.unique_index_id)),
    ]
}
pub(crate) async fn target(
    reader: &mut impl NativeRead,
    target: &MssqlFullTextPopulationTarget,
) -> Result<TargetPoint> {
    if !target.is_valid() {
        return Err(invalid_data());
    }
    let rows = reader.query(&sql::target(), parameters(target, 1), 2).await.map_err(ReadError::into_error)?;
    let [row] = rows.as_slice() else { return Err(CollectionStop::SourceUnavailable.error()) };
    validate_target(row, target)
}
fn validate_target(row: &Rec, target: &MssqlFullTextPopulationTarget) -> Result<TargetPoint> {
    if integer(row, "database_id")? != i64::from(target.table.database_id)
        || integer(row, "object_id")? != i64::from(target.table.object_id)
        || integer(row, "catalog_id")? != i64::from(target.catalog_id)
        || integer(row, "unique_index_id")? != i64::from(target.unique_index_id)
        || crate::sql_time::native_time(&text(row, "created_without_offset")?)?
            != crate::sql_time::native_time(&target.table.created_without_offset)?
        || crate::sql_time::native_time(&text(row, "modified_without_offset")?)?
            != crate::sql_time::native_time(&target.table_modified_without_offset)?
    {
        return Err(CollectionStop::SourceChanged.error());
    }
    match optional_boolean(row, "can_view_definition")? {
        Some(true) => {}
        Some(false) => {
            return Err(Error::new(
                ErrorCode::PermissionDenied,
                culprit_core::t!("provider.mssql.columnstore_rowgroups.view_definition"),
            ));
        }
        None => return Err(CollectionStop::SourceUnavailable.error()),
    }
    let metadata = MssqlFullTextPopulationMetadata {
        observed_at: canonical_time(&text(row, "observed_at")?)?,
        schema: optional_text(row, "schema_name")?,
        table: optional_text(row, "table_name")?,
    };
    if !metadata.is_valid() {
        return Err(invalid_data());
    }
    Ok(TargetPoint { metadata })
}
pub(crate) struct Read {
    pub point: TargetPoint,
    pub coverage: StateSourceCoverage,
    rows: Vec<Rec>,
}
pub(crate) async fn read(
    reader: &mut impl NativeRead,
    target: &MssqlFullTextPopulationTarget,
    limit: u32,
    permission: StatePermission,
) -> Result<Read> {
    if !(1..=MAX_FULL_TEXT_POPULATIONS).contains(&limit) || !target.is_valid() {
        return Err(invalid_data());
    }
    let rows = reader
        .query(&sql::populations(permission), parameters(target, limit), limit as usize + 1)
        .await
        .map_err(ReadError::into_error)?;
    let first = rows.first().ok_or_else(|| CollectionStop::SourceUnavailable.error())?;
    let point = validate_target(first, target)?;
    let permission = optional_boolean(first, "can_read_state")?;
    let layout = optional_boolean(first, "layout_available")?;
    let available = permission == Some(true) && layout == Some(true);
    let mut output = Vec::new();
    for (index, row) in rows.iter().enumerate() {
        if validate_target(row, target)? != point
            || optional_boolean(row, "can_read_state")? != optional_boolean(first, "can_read_state")?
            || optional_boolean(row, "layout_available")? != optional_boolean(first, "layout_available")?
        {
            return Err(CollectionStop::SourceChanged.error());
        }
        let present = integer(row, "population_present")?;
        if present == 0 {
            if rows.len() != 1
                || !RUNTIME_FIELDS.iter().all(|name| {
                    match row.columns().find(|(field, _)| field == name).map(|(_, value)| value) {
                        Some(Val::Null) => true,
                        None => !available,
                        _ => false,
                    }
                })
            {
                return Err(invalid_data());
            }
            continue;
        }
        if present != 1 || !available {
            return Err(invalid_data());
        }
        if integer(row, "population_database_id")? != i64::from(target.table.database_id)
            || integer(row, "population_table_id")? != i64::from(target.table.object_id)
            || integer(row, "population_catalog_id")? != i64::from(target.catalog_id)
        {
            return Err(CollectionStop::SourceChanged.error());
        }
        // The lookahead is not published, but its native target is still checked.
        if index >= limit as usize {
            continue;
        }
        output.push(row.clone());
    }
    let complete = rows.len() <= limit as usize;
    let mut coverage = match (permission, layout) {
        (Some(false), _) => StateSourceCoverage::unavailable(
            MSSQL_FULL_TEXT_POPULATIONS_SOURCE.into(),
            ErrorCode::PermissionDenied,
            None,
        ),
        (_, Some(false)) => {
            StateSourceCoverage::unavailable(MSSQL_FULL_TEXT_POPULATIONS_SOURCE.into(), ErrorCode::Unsupported, None)
        }
        (Some(true), Some(true)) => {
            StateSourceCoverage::observed(MSSQL_FULL_TEXT_POPULATIONS_SOURCE.into(), output.len() as u32, complete)
        }
        _ => StateSourceCoverage::unavailable(
            MSSQL_FULL_TEXT_POPULATIONS_SOURCE.into(),
            CollectionStop::SourceUnavailable.error().code,
            Some(CollectionStop::SourceUnavailable),
        ),
    };
    if !complete {
        coverage.stopped = Some(CollectionStop::RowLimit);
    }
    Ok(Read { point, coverage, rows: output })
}
impl Read {
    pub(crate) fn records(
        self,
        context: &NativeReadContext,
        target: &MssqlFullTextPopulationTarget,
    ) -> Result<Vec<TelemetryRecord>> {
        self.rows
            .iter()
            .enumerate()
            .map(|(index, row)| {
                let number = |name| {
                    optional_integer(row, name)?
                        .map(|value| u32::try_from(value).map_err(|_| invalid_data()))
                        .transpose()
                };
                let code = |name| {
                    optional_signed_integer(row, name)?
                        .map(|value| i32::try_from(value).map_err(|_| invalid_data()))
                        .transpose()
                };
                let point = MssqlFullTextPopulation {
                    context: context.clone(),
                    target: target.clone(),
                    observed_at: self.point.metadata.observed_at.clone(),
                    position: index as u32 + 1,
                    population_type: code("population_type")?,
                    population_type_description: optional_text(row, "population_type_description")?,
                    status: code("status")?,
                    status_description: optional_text(row, "status_description")?,
                    completion_type: code("completion_type")?,
                    completion_type_description: optional_text(row, "completion_type_description")?,
                    queued_population_type: code("queued_population_type")?,
                    queued_population_type_description: optional_text(row, "queued_population_type_description")?,
                    clustered_index_scan: optional_boolean(row, "is_clustered_index_scan")?,
                    range_count: number("range_count")?,
                    completed_range_count: number("completed_range_count")?,
                    outstanding_batch_count: number("outstanding_batch_count")?,
                    started_without_offset: optional_text(row, "started_without_offset")?,
                };
                let key = point.key()?;
                Ok(TelemetryRecord::Operational(OperationalObservation {
                    source: MSSQL_FULL_TEXT_POPULATIONS_SOURCE.into(),
                    key,
                    fact: OperationalFact::MssqlFullTextPopulation(Box::new(point)),
                }))
            })
            .collect()
    }
}
#[cfg(test)]
pub(crate) mod tests;
