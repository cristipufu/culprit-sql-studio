//! Read-only database-local CDC statistics. Native errors are retained without their private message text.
use crate::change_capture::{local_time, native_id, native_lsn};
use crate::{
    local_source::Context,
    read::{
        NativeRead, Parameter, ReadError, integer, invalid_data, optional_boolean, optional_counter, optional_integer,
        optional_text, text,
    },
    value::Rec,
};
use culprit_core::{Result, telemetry::*};
mod sql;

pub(crate) struct Point {
    pub database_id: u32,
    pub observed_at: String,
    pub data: MssqlCdcData,
}
impl Point {
    pub fn bind(self, context: &NativeReadContext, position: u32) -> Result<OperationalObservation> {
        let point = MssqlCdcObservation {
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
            fact: OperationalFact::MssqlCdc(Box::new(point)),
        })
    }
}

pub(crate) async fn status(reader: &mut impl NativeRead, context: &Context) -> Result<Point> {
    let permission = MssqlCdcPermission::for_major(context.activity.major);
    let rows = reader
        .query(&sql::status(), vec![Parameter::Text(permission.native_name().into())], 2)
        .await
        .map_err(ReadError::into_error)?;
    let [row] = rows.as_slice() else { return Err(invalid_data()) };
    point(
        row,
        context.database_id,
        MssqlCdcData::Status(MssqlCdcStatus {
            native_major: context.activity.major,
            native_edition: context.activity.edition,
            enabled: optional_boolean(row, "enabled")?,
            permission,
            has_permission: optional_boolean(row, "has_permission")?,
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
                MssqlCdcData::ScanSession(Box::new(MssqlCdcScanSession {
                    session_id: native_id(row, "session_id")?,
                    started_without_offset: local_time(row, "started")?,
                    ended_without_offset: local_time(row, "ended")?,
                    native_phase: optional_text(row, "scan_phase")?,
                    duration_seconds: optional_counter(row, "duration")?,
                    error_count: optional_counter(row, "error_count")?,
                    transactions: optional_counter(row, "tran_count")?,
                    log_records: optional_counter(row, "log_record_count")?,
                    commands: optional_counter(row, "command_count")?,
                    failed_sessions: optional_counter(row, "failed_sessions_count")?,
                    latency_seconds: optional_counter(row, "latency")?,
                    last_commit_without_offset: local_time(row, "last_commit")?,
                    last_cdc_commit_without_offset: local_time(row, "last_cdc_commit")?,
                    start_lsn: native_lsn(row, "start_lsn")?,
                    current_lsn: native_lsn(row, "current_lsn")?,
                    end_lsn: native_lsn(row, "end_lsn")?,
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
                MssqlCdcData::Error(MssqlCdcError {
                    session_id: native_id(row, "session_id")?,
                    phase_number: optional_integer(row, "phase_number")?
                        .map(|value| value.try_into().map_err(|_| invalid_data()))
                        .transpose()?,
                    entered_without_offset: local_time(row, "entered")?,
                    error_number: native_id(row, "error_number")?,
                    severity: integer(row, "error_severity")?.try_into().map_err(|_| invalid_data())?,
                    state: integer(row, "error_state")?.try_into().map_err(|_| invalid_data())?,
                    start_lsn: native_lsn(row, "start_lsn")?,
                    begin_lsn: native_lsn(row, "begin_lsn")?,
                    sequence_value: native_lsn(row, "sequence_value")?,
                }),
            )
        })
        .collect()
}
fn point(row: &Rec, database_id: u32, data: MssqlCdcData) -> Result<Point> {
    let native_database_id = native_id(row, "database_id")?;
    if !data.is_valid() {
        return Err(invalid_data());
    }
    if native_database_id != database_id {
        return Err(CollectionStop::SourceChanged.error());
    }
    Ok(Point { database_id, observed_at: canonical_time(&text(row, "observed_at")?)?, data })
}

#[cfg(test)]
pub(crate) mod tests;
