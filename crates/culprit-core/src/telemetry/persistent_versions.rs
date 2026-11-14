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
    /// Native minimum-threshold cleanup context; this is not evidence that aborted transactions retain PVS.
    pub pages_skipped_for_aborted_transactions: Option<String>,
    pub pages_skipped_for_oldest_active_transaction: Option<String>,
    pub pages_skipped_for_long_snapshot_scan: Option<String>,
    /// The native column is named `pvs_off_row_page_skipped_oldest_snapshot`.
    pub pages_skipped_for_online_index_rebuild: Option<String>,
    pub pages_skipped_for_oldest_aborted_transaction: Option<String>,
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
                &self.pages_skipped_for_oldest_active_transaction,
                &self.pages_skipped_for_long_snapshot_scan,
                &self.pages_skipped_for_online_index_rebuild,
                &self.pages_skipped_for_oldest_aborted_transaction,
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
            && self.values_have_native_columns()
    }

    fn values_have_native_columns(&self) -> bool {
        let present = |column: &str| self.available_columns.iter().any(|value| value == column);
        (self.filegroup_id.is_none() || present("pvs_filegroup_id"))
            && [
                (&self.off_row_kibibytes, "persistent_version_store_size_kb"),
                (&self.online_index_kibibytes, "online_index_version_store_size_kb"),
                (&self.aborted_transactions, "current_aborted_transaction_count"),
                (&self.oldest_active_transaction_id, "oldest_active_transaction_id"),
                (&self.oldest_aborted_transaction_id, "oldest_aborted_transaction_id"),
                (&self.minimum_transaction_timestamp, "min_transaction_timestamp"),
                (&self.online_index_minimum_timestamp, "online_index_min_transaction_timestamp"),
                (&self.secondary_low_water_mark, "secondary_low_water_mark"),
                (&self.off_row_cleanup.started_without_offset, "offrow_version_cleaner_start_time"),
                (&self.off_row_cleanup.ended_without_offset, "offrow_version_cleaner_end_time"),
                (&self.aborted_cleanup.started_without_offset, "aborted_version_cleaner_start_time"),
                (&self.aborted_cleanup.ended_without_offset, "aborted_version_cleaner_end_time"),
                (&self.pages_skipped_for_secondaries, "pvs_off_row_page_skipped_low_water_mark"),
                (&self.pages_skipped_for_aborted_transactions, "pvs_off_row_page_skipped_transaction_not_cleaned"),
                (&self.pages_skipped_for_oldest_active_transaction, "pvs_off_row_page_skipped_oldest_active_xdesid"),
                (&self.pages_skipped_for_long_snapshot_scan, "pvs_off_row_page_skipped_min_useful_xts"),
                (&self.pages_skipped_for_online_index_rebuild, "pvs_off_row_page_skipped_oldest_snapshot"),
                (&self.pages_skipped_for_oldest_aborted_transaction, "pvs_off_row_page_skipped_oldest_aborted_xdesid"),
            ]
            .into_iter()
            .all(|(value, column)| value.is_none() || present(column))
    }
}

#[cfg(test)]
mod tests;
