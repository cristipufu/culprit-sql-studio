//! CDC DMV rows are retained points. Scan IDs and process epochs do not establish a CDC reset generation.
use super::change_capture::{counter, local_time, lsn};
use super::*;

pub const MSSQL_CDC_STATUS_SOURCE: &str = "cdc status";
pub const MSSQL_CDC_SCANS_SOURCE: &str = "cdc scan sessions";
pub const MSSQL_CDC_ERRORS_SOURCE: &str = "cdc processing errors";
pub const MAX_MSSQL_CDC_SCAN_ROWS: usize = 33;
pub const MAX_MSSQL_CDC_ERROR_ROWS: usize = 64;

#[derive(Serialize, Deserialize, TS, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum MssqlCdcPermission {
    ViewDatabaseState,
    ViewDatabasePerformanceState,
}
impl MssqlCdcPermission {
    pub fn for_major(major: u32) -> Self {
        if major >= 16 { Self::ViewDatabasePerformanceState } else { Self::ViewDatabaseState }
    }
    pub fn native_name(self) -> &'static str {
        match self {
            Self::ViewDatabaseState => "VIEW DATABASE STATE",
            Self::ViewDatabasePerformanceState => "VIEW DATABASE PERFORMANCE STATE",
        }
    }
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[ts(export)]
pub struct MssqlCdcStatus {
    pub native_major: u32,
    #[ts(type = "number")]
    pub native_edition: i64,
    pub enabled: Option<bool>,
    pub permission: MssqlCdcPermission,
    pub has_permission: Option<bool>,
    pub scan_layout_available: bool,
    pub error_layout_available: bool,
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[ts(export)]
pub struct MssqlCdcScanSession {
    /// Zero is the native aggregate, not a scan session or SQL Server session ID.
    pub session_id: u32,
    pub started_without_offset: Option<String>,
    pub ended_without_offset: Option<String>,
    pub native_phase: Option<String>,
    pub duration_seconds: Option<String>,
    pub error_count: Option<String>,
    pub transactions: Option<String>,
    pub log_records: Option<String>,
    pub commands: Option<String>,
    pub failed_sessions: Option<String>,
    /// Native phase-7 latency. Aggregate zero retains the last nonzero value, not a current lag measurement.
    pub latency_seconds: Option<String>,
    pub last_commit_without_offset: Option<String>,
    pub last_cdc_commit_without_offset: Option<String>,
    pub start_lsn: Option<String>,
    pub current_lsn: Option<String>,
    pub end_lsn: Option<String>,
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[ts(export)]
pub struct MssqlCdcError {
    /// Zero means the error did not occur in a CDC scan session. It is never a SQL Server session ID.
    pub session_id: u32,
    pub phase_number: Option<u32>,
    pub entered_without_offset: Option<String>,
    pub error_number: u32,
    pub severity: u8,
    pub state: u8,
    pub start_lsn: Option<String>,
    pub begin_lsn: Option<String>,
    pub sequence_value: Option<String>,
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[serde(tag = "kind", content = "value", rename_all = "kebab-case")]
#[ts(export)]
pub enum MssqlCdcData {
    Status(MssqlCdcStatus),
    ScanSession(Box<MssqlCdcScanSession>),
    Error(MssqlCdcError),
}

impl MssqlCdcData {
    pub fn is_valid(&self) -> bool {
        match self {
            Self::Status(status) => {
                status.native_major > 0
                    && matches!(status.native_edition, 2 | 3 | 4 | 5 | 8)
                    && status.permission == MssqlCdcPermission::for_major(status.native_major)
            }
            Self::ScanSession(row) => {
                [
                    &row.started_without_offset,
                    &row.ended_without_offset,
                    &row.last_commit_without_offset,
                    &row.last_cdc_commit_without_offset,
                ]
                .into_iter()
                .all(local_time)
                    && row.native_phase.as_ref().is_none_or(|phase| !phase.is_empty() && phase.chars().count() <= 200)
                    && [
                        &row.duration_seconds,
                        &row.error_count,
                        &row.transactions,
                        &row.log_records,
                        &row.commands,
                        &row.failed_sessions,
                        &row.latency_seconds,
                    ]
                    .into_iter()
                    .all(counter)
                    && [&row.start_lsn, &row.current_lsn, &row.end_lsn].into_iter().all(lsn)
            }
            Self::Error(row) => {
                row.error_number > 0
                    && (1..=25).contains(&row.severity)
                    && local_time(&row.entered_without_offset)
                    && [&row.start_lsn, &row.begin_lsn, &row.sequence_value].into_iter().all(lsn)
            }
        }
    }
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[ts(export)]
pub struct MssqlCdcObservation {
    pub context: NativeReadContext,
    pub database_id: u32,
    /// This query's native UTC observation point, independent of all native local timestamps below.
    pub observed_at: String,
    /// Original returned order within this source and capture. It is not a stable event ID.
    pub position: u32,
    /// CDC disable/re-enable and scan ID reuse are not covered by the process epoch.
    pub reset: crate::ResetMarker,
    pub data: MssqlCdcData,
}
impl MssqlCdcObservation {
    pub fn source(&self) -> &'static str {
        match self.data {
            MssqlCdcData::Status(_) => MSSQL_CDC_STATUS_SOURCE,
            MssqlCdcData::ScanSession(_) => MSSQL_CDC_SCANS_SOURCE,
            MssqlCdcData::Error(_) => MSSQL_CDC_ERRORS_SOURCE,
        }
    }
    pub fn key(&self) -> Result<String> {
        self.is_valid().then(|| format!("cdc:{}:{}", self.source(), self.position)).ok_or_else(invalid)
    }
    pub fn is_valid(&self) -> bool {
        change_capture_owner_is_qualified(&self.context)
            && (1..=i16::MAX as u32).contains(&self.database_id)
            && ObservationBounds::new(&self.context.observed_from, &self.context.observed_until)
                .is_some_and(|bounds| bounds.contains(&self.observed_at))
            && self.reset == crate::ResetMarker::Unknown
            && self.data.is_valid()
            && match self.data {
                MssqlCdcData::Status(_) => self.position == 1,
                MssqlCdcData::ScanSession(_) => (1..=MAX_MSSQL_CDC_SCAN_ROWS as u32).contains(&self.position),
                MssqlCdcData::Error(_) => (1..=MAX_MSSQL_CDC_ERROR_ROWS as u32).contains(&self.position),
            }
    }
    pub fn fits_source(&self, source: &CollectionDescription) -> bool {
        self.is_valid()
            && source.continuity == SourceContinuity::Unverified
            && source.target == self.context.target
            && source.partition == self.context.partition
            && source.observation.snapshot().is_some_and(|header| {
                cdc_header_is_valid(header)
                    && same_time(&header.observed_from, &self.context.observed_from)
                    && same_time(&header.observed_until, &self.context.observed_until)
            })
    }
}

pub fn cdc_header_is_valid(header: &StateSnapshotDescription) -> bool {
    MssqlChangeCaptureFamily::Cdc.valid_header(header)
}

fn invalid() -> Error {
    CollectionStop::InvalidPage.error()
}

#[cfg(test)]
mod tests;
