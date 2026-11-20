//! One selected native catalog reader. No application table, physical-stats DMV, dictionary or segment scan.
use crate::{
    read::{
        NativeRead, Parameter, ReadError, boolean, integer, invalid_data, optional_boolean, optional_counter, text,
    },
    value::Rec,
};
use culprit_core::{Error, ErrorCode, Result, telemetry::*};
mod sql;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TargetPoint {
    pub observed_at: String,
    pub index_type: u8,
}

pub(crate) async fn target(reader: &mut impl NativeRead, target: &MssqlColumnstoreTarget) -> Result<TargetPoint> {
    if !target.is_valid() {
        return Err(invalid_data());
    }
    let rows = reader
        .query(
            sql::TARGET,
            vec![
                Parameter::Integer(i64::from(target.table.object_id)),
                Parameter::Integer(i64::from(target.index_id)),
                Parameter::Integer(i64::from(target.partition_number.unwrap_or(0))),
            ],
            2,
        )
        .await
        .map_err(ReadError::into_error)?;
    let [row] = rows.as_slice() else { return Err(CollectionStop::SourceUnavailable.error()) };
    validate_target(row, target)
}
fn validate_target(row: &Rec, target: &MssqlColumnstoreTarget) -> Result<TargetPoint> {
    if integer(row, "database_id")? != i64::from(target.table.database_id)
        || integer(row, "object_id")? != i64::from(target.table.object_id)
        || integer(row, "index_id")? != i64::from(target.index_id)
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
    let index_type = u8::try_from(integer(row, "index_type")?).map_err(|_| invalid_data())?;
    if !matches!(index_type, 5 | 6) || boolean(row, "is_memory_optimized")? || !boolean(row, "layout_available")? {
        return Err(Error::new(ErrorCode::Unsupported, culprit_core::t!("collection.unsupported")));
    }
    if !boolean(row, "partition_present")? {
        return Err(CollectionStop::SourceUnavailable.error());
    }
    Ok(TargetPoint { observed_at: canonical_time(&text(row, "observed_at")?)?, index_type })
}

pub(crate) struct Point {
    pub observed_at: String,
    pub partition_number: u32,
    pub row_group_id: u32,
    pub index_type: u8,
    pub delta_store_hobt_id: Option<String>,
    pub state: u8,
    pub total_rows: Option<String>,
    pub deleted_rows: Option<String>,
    pub size_bytes: Option<String>,
}
impl Point {
    pub fn bind(self, context: &NativeReadContext, target: &MssqlColumnstoreTarget) -> Result<TelemetryRecord> {
        let point = MssqlColumnstoreRowgroup {
            context: context.clone(),
            target: target.clone(),
            observed_at: self.observed_at,
            partition_number: self.partition_number,
            row_group_id: self.row_group_id,
            index_type: self.index_type,
            delta_store_hobt_id: self.delta_store_hobt_id,
            state: self.state,
            total_rows: self.total_rows,
            deleted_rows: self.deleted_rows,
            size_bytes: self.size_bytes,
        };
        Ok(TelemetryRecord::Operational(OperationalObservation {
            source: MSSQL_COLUMNSTORE_ROWGROUPS_SOURCE.into(),
            key: point.key()?,
            fact: OperationalFact::MssqlColumnstoreRowgroup(Box::new(point)),
        }))
    }
}
pub(crate) async fn read(
    reader: &mut impl NativeRead,
    target: &MssqlColumnstoreTarget,
    index_type: u8,
    limit: u32,
) -> Result<Vec<Point>> {
    if limit == 0 || limit > MAX_COLUMNSTORE_ROWGROUPS || !target.is_valid() {
        return Err(invalid_data());
    }
    let rows = reader
        .query(
            sql::ROWGROUPS,
            vec![
                Parameter::Integer(i64::from(limit) + 1),
                Parameter::Integer(i64::from(target.table.object_id)),
                Parameter::Integer(i64::from(target.index_id)),
                Parameter::Integer(i64::from(target.partition_number.unwrap_or(0))),
            ],
            limit as usize + 1,
        )
        .await
        .map_err(ReadError::into_error)?;
    rows.iter().map(|row| decode(row, target, index_type)).collect()
}
fn decode(row: &Rec, target: &MssqlColumnstoreTarget, index_type: u8) -> Result<Point> {
    if integer(row, "database_id")? != i64::from(target.table.database_id)
        || integer(row, "object_id")? != i64::from(target.table.object_id)
        || integer(row, "index_id")? != i64::from(target.index_id)
        || integer(row, "index_type")? != i64::from(index_type)
    {
        return Err(CollectionStop::SourceChanged.error());
    }
    let narrow = |name| u32::try_from(integer(row, name)?).map_err(|_| invalid_data());
    Ok(Point {
        observed_at: canonical_time(&text(row, "observed_at")?)?,
        partition_number: narrow("partition_number")?,
        row_group_id: narrow("row_group_id")?,
        index_type,
        delta_store_hobt_id: optional_counter(row, "delta_store_hobt_id")?,
        state: u8::try_from(integer(row, "state")?).map_err(|_| invalid_data())?,
        total_rows: optional_counter(row, "total_rows")?,
        deleted_rows: optional_counter(row, "deleted_rows")?,
        size_bytes: optional_counter(row, "size_in_bytes")?,
    })
}

#[cfg(test)]
pub(crate) mod tests;
