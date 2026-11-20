//! Original database backup metadata and an informational interpretation, independent of collector recovery.
use crate::{SourceState, telemetry::*};
use serde::{Deserialize, Serialize};
use ts_rs::TS;
#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[ts(export)]
pub struct BackupHistoryEntry {
    pub observation_key: String,
    pub observed_at: String,
    pub native: MssqlBackupRecord,
    pub title: String,
    pub explanation: String,
    pub investigation: String,
    pub verify: String,
}
#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[ts(export)]
pub struct BackupHistoryReport {
    pub context: NativeReadContext,
    pub scope: MssqlBackupHistoryScope,
    pub source: StateSourceCoverage,
    pub source_state: SourceState,
    pub read_complete: bool,
    pub title: String,
    pub summary: String,
    pub limitations: Vec<String>,
    pub entries: Vec<BackupHistoryEntry>,
}
