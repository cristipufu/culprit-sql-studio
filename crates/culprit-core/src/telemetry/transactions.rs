//! The database's oldest qualifying cleanup holder is one current statistic, not a history of all transactions.
use super::*;

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[ts(export)]
pub struct MssqlCleanupTransaction {
    pub session: NativeActivityIdentity,
    /// The generation is the engine's native begin time, with no invented UTC offset.
    pub transaction: NativeActivityIdentity,
    pub observed_time_without_offset: String,
    pub age_seconds: Option<String>,
    pub program: Option<String>,
    pub native_status: Option<String>,
    pub log_record_count: Option<String>,
    pub log_bytes_used: Option<String>,
    /// Native LSNs can exceed u64; decimal text preserves the entire value.
    pub begin_lsn: Option<String>,
    /// XSN membership includes transactions which make versions, independently of snapshot isolation.
    pub row_version_participant: bool,
    pub snapshot_isolation: Option<bool>,
    pub transaction_sequence_number: Option<String>,
    /// Time since the XSN was assigned, distinct from the transaction's age.
    pub versioning_age_seconds: Option<String>,
}
impl MssqlCleanupTransaction {
    pub fn has_database_evidence(&self) -> bool {
        self.snapshot_isolation == Some(true)
            || self.log_record_count.as_deref().is_some_and(|value| value.parse::<u64>().is_ok_and(|count| count > 0))
    }
    pub fn is_valid(&self) -> bool {
        let text = |value: &String| value.len() <= 2048;
        self.session.is_valid()
            && self.transaction.is_valid()
            && valid_key(&self.observed_time_without_offset)
            && [
                &self.age_seconds,
                &self.log_record_count,
                &self.log_bytes_used,
                &self.transaction_sequence_number,
                &self.versioning_age_seconds,
            ]
            .into_iter()
            .flatten()
            .all(|value| valid_counter(value))
            && [&self.program, &self.native_status].into_iter().flatten().all(text)
            && self.begin_lsn.as_deref().is_none_or(|value| {
                !value.is_empty() && value.len() <= 25 && value.bytes().all(|byte| byte.is_ascii_digit())
            })
            && (self.row_version_participant
                || self
                    .log_record_count
                    .as_deref()
                    .is_some_and(|value| value.parse::<u64>().is_ok_and(|count| count > 0)))
            && (self.row_version_participant
                || (self.snapshot_isolation.is_none()
                    && self.transaction_sequence_number.is_none()
                    && self.versioning_age_seconds.is_none()))
    }
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[ts(export)]
pub struct MssqlCleanupHorizon {
    /// The oldest transaction with logged work or an explicit snapshot in this database.
    /// None establishes no such transaction, rather than proving no cleanup pressure.
    /// Source failures never produce this empty measurement.
    pub oldest_confirmed: Option<Box<MssqlCleanupTransaction>>,
    /// Native row-version membership cannot attribute this transaction to this database.
    pub oldest_unattributed_versioning: Option<Box<MssqlCleanupTransaction>>,
}
impl MssqlCleanupHorizon {
    pub fn is_valid(&self) -> bool {
        self.oldest_confirmed.as_ref().is_none_or(|row| row.is_valid() && row.has_database_evidence())
            && self
                .oldest_unattributed_versioning
                .as_ref()
                .is_none_or(|row| row.is_valid() && row.row_version_participant && !row.has_database_evidence())
    }
}
