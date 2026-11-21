//! Database-local change-feed progress and errors. No feature configuration or private error text is read.
use crate::{
    change_capture::{local_time, native_id, native_lsn},
    local_source::Context,
    read::{
        NativeRead, Parameter, ReadError, integer, invalid_data, optional_boolean, optional_counter, optional_integer,
        optional_signed_integer, optional_text, text,
    },
    value::Rec,
};
use culprit_core::{Result, telemetry::*};
mod sql;

pub(crate) struct Point {
    pub database_id: u32,
    pub observed_at: String,
    pub data: MssqlChangeFeedData,
}
impl Point {
    pub fn bind(self, context: &NativeReadContext, position: u32) -> Result<OperationalObservation> {
        let point = MssqlChangeFeedObservation {
            context: context.clone(),
            database_id: self.database_id,
            observed_at: self.observed_at,
            position,
            reset: culprit_core::ResetMarker::Unknown,
            data: self.data,
        };
        Ok(OperationalObservation {
            source: point.source().into(),
            key: point.key()?,
            fact: OperationalFact::MssqlChangeFeed(Box::new(point)),
        })
    }
}
pub(crate) async fn availability(reader: &mut impl NativeRead, context: &Context) -> Result<Point> {
    let rows = reader.query(&sql::availability(), vec![], 2).await.map_err(ReadError::into_error)?;
    let [row] = rows.as_slice() else { return Err(invalid_data()) };
    point(
        row,
        context.database_id,
        MssqlChangeFeedData::Availability(MssqlChangeFeedAvailability {
            native_major: context.activity.major,
            native_edition: context.activity.edition,
            has_database_state: optional_boolean(row, "has_database_state")?,
            has_database_performance_state: optional_boolean(row, "has_database_performance_state")?,
            scan_layout_available: crate::read::boolean(row, "scan_layout")?,
            error_layout_available: crate::read::boolean(row, "error_layout")?,
        }),
    )
}
pub(crate) async fn scans(reader: &mut impl NativeRead, database_id: u32, limit: usize) -> Result<Vec<Point>> {
    let rows = reader
        .query(sql::SCANS, vec![Parameter::Integer(limit as i64 + 1)], limit + 1)
        .await
        .map_err(ReadError::into_error)?;
    rows.iter()
        .map(|row| {
            point(
                row,
                database_id,
                MssqlChangeFeedData::ScanSession(Box::new(MssqlChangeFeedScanSession {
                    session_id: native_id(row, "session_id")?,
                    started_without_offset: local_time(row, "started")?,
                    ended_without_offset: local_time(row, "ended")?,
                    native_phase: optional_text(row, "batch_processing_phase")?,
                    duration_seconds: optional_counter(row, "duration")?,
                    error_count: optional_counter(row, "error_count")?,
                    transactions: optional_counter(row, "tran_count")?,
                    log_records: optional_counter(row, "log_record_count")?,
                    commands: optional_counter(row, "command_count")?,
                    schema_changes: optional_counter(row, "schema_change_count")?,
                    empty_scans: optional_counter(row, "empty_scan_count")?,
                    failed_sessions: optional_counter(row, "failed_sessions_count")?,
                    latency_seconds: optional_counter(row, "latency")?,
                    processed_commit_without_offset: local_time(row, "processed_commit")?,
                    batch_start_lsn: native_lsn(row, "batch_start_lsn")?,
                    currently_processed_lsn: native_lsn(row, "currently_processed_lsn")?,
                    batch_end_lsn: native_lsn(row, "batch_end_lsn")?,
                    processed_commit_lsn: native_lsn(row, "currently_processed_commit_lsn")?,
                })),
            )
        })
        .collect()
}
pub(crate) async fn errors(reader: &mut impl NativeRead, database_id: u32, limit: usize) -> Result<Vec<Point>> {
    let rows = reader
        .query(sql::ERRORS, vec![Parameter::Integer(limit as i64 + 1)], limit + 1)
        .await
        .map_err(ReadError::into_error)?;
    rows.iter()
        .map(|row| {
            point(
                row,
                database_id,
                MssqlChangeFeedData::Error(Box::new(MssqlChangeFeedError {
                    session_id: native_id(row, "session_id")?,
                    source_task: optional_integer(row, "source_task")?
                        .map(|v| v.try_into().map_err(|_| invalid_data()))
                        .transpose()?,
                    table_id: optional_signed_integer(row, "table_id")?,
                    table_group_id: optional_text(row, "table_group_id")?,
                    capture_phase_number: optional_id(row, "capture_phase_number")?,
                    entered_without_offset: local_time(row, "entered")?,
                    error_number: native_id(row, "error_number")?,
                    severity: integer(row, "error_severity")?.try_into().map_err(|_| invalid_data())?,
                    state: integer(row, "error_state")?.try_into().map_err(|_| invalid_data())?,
                    command_id: optional_id(row, "command_id")?,
                    batch_start_lsn: native_lsn(row, "batch_start_lsn")?,
                    batch_end_lsn: native_lsn(row, "batch_end_lsn")?,
                    transaction_begin_lsn: native_lsn(row, "tran_begin_lsn")?,
                    transaction_commit_lsn: native_lsn(row, "tran_commit_lsn")?,
                    sequence_value: native_lsn(row, "sequence_value")?,
                })),
            )
        })
        .collect()
}
fn optional_id(row: &Rec, name: &str) -> Result<Option<u32>> {
    optional_integer(row, name)?.map(|v| v.try_into().map_err(|_| invalid_data())).transpose()
}
fn point(row: &Rec, database_id: u32, data: MssqlChangeFeedData) -> Result<Point> {
    if !data.is_valid() {
        return Err(invalid_data());
    }
    if native_id(row, "database_id")? != database_id {
        return Err(CollectionStop::SourceChanged.error());
    }
    Ok(Point { database_id, observed_at: canonical_time(&text(row, "observed_at")?)?, data })
}
#[cfg(test)]
pub(crate) mod tests;
