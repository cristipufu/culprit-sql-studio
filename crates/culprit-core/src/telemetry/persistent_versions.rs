//! SQL Server's ADR statistics describe off-row storage and cleanup watermarks, not total row-version bytes.
use super::*;

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[ts(export)]
pub struct MssqlVersionCleanupSweep {
    pub started_without_offset: Option<String>,
    pub ended_without_offset: Option<String>,
}
impl MssqlVersionCleanupSweep {
    fn is_valid(&self) -> bool {
        [&self.started_without_offset, &self.ended_without_offset]
            .into_iter()
            .flatten()
            .all(|value| valid_key(value) && value.len() <= 64)
    }
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[ts(export)]
pub struct MssqlPersistentVersionStore {
    /// Columns present in the native result distinguish unavailable schema fields from native NULL values.
    pub available_columns: Vec<String>,
    pub filegroup_id: Option<u32>,
    /// Native off-row allocation; this does not include in-row versions.
    pub off_row_kibibytes: Option<String>,
    pub online_index_kibibytes: Option<String>,
    pub aborted_transactions: Option<String>,
    pub oldest_active_transaction_id: Option<String>,
    pub oldest_aborted_transaction_id: Option<String>,
    /// Native integer sequence timestamps, not clock times or elapsed durations.
    pub minimum_transaction_timestamp: Option<String>,
    pub online_index_minimum_timestamp: Option<String>,
    pub secondary_low_water_mark: Option<String>,
    pub off_row_cleanup: MssqlVersionCleanupSweep,
    pub aborted_cleanup: MssqlVersionCleanupSweep,
    pub pages_skipped_for_secondaries: Option<String>,
    pub pages_skipped_for_aborted_transactions: Option<String>,
}
impl MssqlPersistentVersionStore {
    pub fn is_valid(&self) -> bool {
        let mut columns = std::collections::BTreeSet::new();
        !self.available_columns.is_empty()
            && self.available_columns.len() <= 128
            && self.available_columns.iter().all(|value| valid_key(value) && columns.insert(value))
            && [
                &self.off_row_kibibytes,
                &self.online_index_kibibytes,
                &self.aborted_transactions,
                &self.pages_skipped_for_secondaries,
                &self.pages_skipped_for_aborted_transactions,
            ]
            .into_iter()
            .flatten()
            .all(|value| valid_counter(value))
            && [
                &self.oldest_active_transaction_id,
                &self.oldest_aborted_transaction_id,
                &self.minimum_transaction_timestamp,
                &self.online_index_minimum_timestamp,
                &self.secondary_low_water_mark,
            ]
            .into_iter()
            .flatten()
            .all(|value| value.len() <= 20 && value.parse::<i64>().is_ok())
            && self.off_row_cleanup.is_valid()
            && self.aborted_cleanup.is_valid()
    }
}
