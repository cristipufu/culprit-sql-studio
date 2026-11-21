//! Shared native database progress; identities and source scope are owned by the enclosing native family.
use super::*;
#[derive(Serialize, Deserialize, TS, Clone, Debug, Default, PartialEq, Eq)]
#[ts(export)]
pub struct MssqlReplicaDatabaseProgress {
    #[serde(deserialize_with = "Option::deserialize")]
    pub is_primary: Option<bool>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub state: Option<u8>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub native_state: Option<String>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub mode: Option<u8>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub native_mode: Option<String>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub is_commit_participant: Option<bool>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub database_state: Option<u8>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub native_database_state: Option<String>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub is_suspended: Option<bool>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub suspend_reason: Option<u8>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub synchronization_health: Option<u8>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub native_synchronization_health: Option<String>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub native_suspend_reason: Option<String>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub log_send_queue_kb: Option<String>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub redo_queue_kb: Option<String>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub log_send_rate_kb_per_second: Option<String>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub redo_rate_kb_per_second: Option<String>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub secondary_lag_seconds: Option<String>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub filestream_send_rate_kb_per_second: Option<String>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub low_water_mark_for_ghosts: Option<String>,
    /// Numeric(25,0) native log positions. Only last_redone_lsn/last_commit_lsn are actual LSNs;
    /// the others are padded log-block identifiers. None supports byte subtraction.
    #[serde(deserialize_with = "Option::deserialize")]
    pub recovery_lsn: Option<String>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub truncation_lsn: Option<String>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub last_sent_lsn: Option<String>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub last_received_lsn: Option<String>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub last_hardened_lsn: Option<String>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub last_redone_lsn: Option<String>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub end_of_log_lsn: Option<String>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub last_commit_lsn: Option<String>,
    /// Native SQL Server datetime values have no recorded UTC offset.
    #[serde(deserialize_with = "Option::deserialize")]
    pub last_sent_without_offset: Option<String>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub last_received_without_offset: Option<String>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub last_hardened_without_offset: Option<String>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub last_redone_without_offset: Option<String>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub last_commit_without_offset: Option<String>,
}
impl MssqlReplicaDatabaseProgress {
    pub fn is_valid(&self) -> bool {
        use super::replication::{local_time, log_position, native_name};
        [
            &self.native_state,
            &self.native_mode,
            &self.native_database_state,
            &self.native_synchronization_health,
            &self.native_suspend_reason,
        ]
        .into_iter()
        .flatten()
        .all(|value| native_name(value))
            && [
                &self.log_send_queue_kb,
                &self.redo_queue_kb,
                &self.log_send_rate_kb_per_second,
                &self.redo_rate_kb_per_second,
                &self.secondary_lag_seconds,
                &self.filestream_send_rate_kb_per_second,
                &self.low_water_mark_for_ghosts,
            ]
            .into_iter()
            .flatten()
            .all(|value| valid_counter(value))
            && [
                &self.recovery_lsn,
                &self.truncation_lsn,
                &self.last_sent_lsn,
                &self.last_received_lsn,
                &self.last_hardened_lsn,
                &self.last_redone_lsn,
                &self.end_of_log_lsn,
                &self.last_commit_lsn,
            ]
            .into_iter()
            .flatten()
            .all(|value| log_position(value))
            && [
                &self.last_sent_without_offset,
                &self.last_received_without_offset,
                &self.last_hardened_without_offset,
                &self.last_redone_without_offset,
                &self.last_commit_without_offset,
            ]
            .into_iter()
            .flatten()
            .all(|value| local_time(value))
    }
}
