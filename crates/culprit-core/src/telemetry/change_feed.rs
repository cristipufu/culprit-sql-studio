//! Current native change-feed history caches, distinct from CDC and from a complete delivery/event feed.
use super::change_capture::{counter, local_time, lsn};
use super::*;

pub const MSSQL_CHANGE_FEED_STATUS_SOURCE: &str = "change feed availability";
pub const MSSQL_CHANGE_FEED_SCANS_SOURCE: &str = "change feed scan sessions";
pub const MSSQL_CHANGE_FEED_ERRORS_SOURCE: &str = "change feed processing errors";
// Client retention policies; the scan limit is not a claim about the native history-cache size.
pub const MAX_MSSQL_CHANGE_FEED_SCAN_ROWS: usize = 33;
pub const MAX_MSSQL_CHANGE_FEED_ERROR_ROWS: usize = 64;

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[ts(export)]
#[serde(deny_unknown_fields)]
pub struct MssqlChangeFeedAvailability {
    pub native_major: u32,
    #[ts(type = "number")]
    pub native_edition: i64,
    #[serde(deserialize_with = "Option::deserialize")]
    pub has_database_state: Option<bool>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub has_database_performance_state: Option<bool>,
    pub scan_layout_available: bool,
    pub error_layout_available: bool,
}
impl MssqlChangeFeedAvailability {
    pub fn has_permission(&self) -> Option<bool> {
        match (self.has_database_state, self.has_database_performance_state) {
            (Some(true), _) | (_, Some(true)) => Some(true),
            (Some(false), Some(false)) => Some(false),
            _ => None,
        }
    }
}
#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[ts(export)]
#[serde(deny_unknown_fields)]
pub struct MssqlChangeFeedScanSession {
    /// Zero is the retained native aggregate, never a SQL session or a reset witness.
    pub session_id: u32,
    #[serde(deserialize_with = "Option::deserialize")]
    pub started_without_offset: Option<String>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub ended_without_offset: Option<String>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub native_phase: Option<String>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub duration_seconds: Option<String>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub error_count: Option<String>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub transactions: Option<String>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub log_records: Option<String>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub commands: Option<String>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub schema_changes: Option<String>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub empty_scans: Option<String>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub failed_sessions: Option<String>,
    /// Native phase-7 value. The aggregate keeps its last nonzero value, not current end-to-end lag.
    #[serde(deserialize_with = "Option::deserialize")]
    pub latency_seconds: Option<String>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub processed_commit_without_offset: Option<String>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub batch_start_lsn: Option<String>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub currently_processed_lsn: Option<String>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub batch_end_lsn: Option<String>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub processed_commit_lsn: Option<String>,
}
#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[ts(export)]
#[serde(deny_unknown_fields)]
pub struct MssqlChangeFeedError {
    pub session_id: u32,
    /// Native task code (including unknown future codes), not a current worker/session identity.
    #[serde(deserialize_with = "Option::deserialize")]
    pub source_task: Option<u8>,
    #[ts(type = "number | null")]
    #[serde(deserialize_with = "Option::deserialize")]
    pub table_id: Option<i64>,
    /// Original native setup identifier. No current table name/incarnation is inferred.
    #[serde(deserialize_with = "Option::deserialize")]
    pub table_group_id: Option<String>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub capture_phase_number: Option<u32>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub entered_without_offset: Option<String>,
    pub error_number: u32,
    pub severity: u8,
    pub state: u8,
    #[serde(deserialize_with = "Option::deserialize")]
    pub command_id: Option<u32>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub batch_start_lsn: Option<String>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub batch_end_lsn: Option<String>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub transaction_begin_lsn: Option<String>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub transaction_commit_lsn: Option<String>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub sequence_value: Option<String>,
}
#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[serde(tag = "kind", content = "value", rename_all = "kebab-case")]
#[ts(export)]
pub enum MssqlChangeFeedData {
    Availability(MssqlChangeFeedAvailability),
    ScanSession(Box<MssqlChangeFeedScanSession>),
    Error(Box<MssqlChangeFeedError>),
}
impl MssqlChangeFeedData {
    pub fn is_valid(&self) -> bool {
        match self {
            Self::Availability(value) => value.native_major > 0 && matches!(value.native_edition, 2 | 3 | 4 | 5 | 8),
            Self::ScanSession(row) => {
                [&row.started_without_offset, &row.ended_without_offset, &row.processed_commit_without_offset]
                    .into_iter()
                    .all(local_time)
                    && row.native_phase.as_ref().is_none_or(|value| {
                        !value.is_empty() && value.chars().count() <= 200 && !value.chars().any(char::is_control)
                    })
                    && [
                        &row.duration_seconds,
                        &row.error_count,
                        &row.transactions,
                        &row.log_records,
                        &row.commands,
                        &row.schema_changes,
                        &row.empty_scans,
                        &row.failed_sessions,
                        &row.latency_seconds,
                    ]
                    .into_iter()
                    .all(counter)
                    && [
                        &row.batch_start_lsn,
                        &row.currently_processed_lsn,
                        &row.batch_end_lsn,
                        &row.processed_commit_lsn,
                    ]
                    .into_iter()
                    .all(lsn)
            }
            Self::Error(row) => {
                row.error_number > 0
                    && (1..=25).contains(&row.severity)
                    && row.table_id.is_none_or(|value| i32::try_from(value).is_ok())
                    && row.table_group_id.as_ref().is_none_or(|value| super::replication::native_guid(value))
                    && local_time(&row.entered_without_offset)
                    && [
                        &row.batch_start_lsn,
                        &row.batch_end_lsn,
                        &row.transaction_begin_lsn,
                        &row.transaction_commit_lsn,
                        &row.sequence_value,
                    ]
                    .into_iter()
                    .all(lsn)
            }
        }
    }
}
#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[ts(export)]
#[serde(deny_unknown_fields)]
pub struct MssqlChangeFeedObservation {
    pub context: NativeReadContext,
    pub database_id: u32,
    /// Query-native UTC point, independent of offset-less historical timestamps.
    pub observed_at: String,
    /// Original source/capture order only. Never an event ID or deduplication key across captures.
    pub position: u32,
    pub reset: crate::ResetMarker,
    pub data: MssqlChangeFeedData,
}
impl MssqlChangeFeedObservation {
    pub fn source(&self) -> &'static str {
        match self.data {
            MssqlChangeFeedData::Availability(_) => MSSQL_CHANGE_FEED_STATUS_SOURCE,
            MssqlChangeFeedData::ScanSession(_) => MSSQL_CHANGE_FEED_SCANS_SOURCE,
            MssqlChangeFeedData::Error(_) => MSSQL_CHANGE_FEED_ERRORS_SOURCE,
        }
    }
    pub fn key(&self) -> Result<String> {
        self.is_valid().then(|| format!("change-feed:{}:{}", self.source(), self.position)).ok_or_else(invalid)
    }
    pub fn is_valid(&self) -> bool {
        change_capture_owner_is_qualified(&self.context)
            && (1..=i16::MAX as u32).contains(&self.database_id)
            && ObservationBounds::new(&self.context.observed_from, &self.context.observed_until)
                .is_some_and(|bounds| bounds.contains(&self.observed_at))
            && self.reset == crate::ResetMarker::Unknown
            && self.data.is_valid()
            && match self.data {
                MssqlChangeFeedData::Availability(_) => self.position == 1,
                MssqlChangeFeedData::ScanSession(_) => {
                    (1..=MAX_MSSQL_CHANGE_FEED_SCAN_ROWS as u32).contains(&self.position)
                }
                MssqlChangeFeedData::Error(_) => (1..=MAX_MSSQL_CHANGE_FEED_ERROR_ROWS as u32).contains(&self.position),
            }
    }
    pub fn fits_source(&self, source: &CollectionDescription) -> bool {
        self.is_valid()
            && source.continuity == SourceContinuity::Unverified
            && source.target == self.context.target
            && source.partition == self.context.partition
            && source.observation.snapshot().is_some_and(|header| {
                MssqlChangeCaptureFamily::ChangeFeed.valid_header(header)
                    && same_time(&header.observed_from, &self.context.observed_from)
                    && same_time(&header.observed_until, &self.context.observed_until)
            })
    }
}
fn invalid() -> Error {
    CollectionStop::InvalidPage.error()
}
#[cfg(test)]
mod tests;
