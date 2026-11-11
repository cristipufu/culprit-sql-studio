//! Bounded ADR metadata; missing capability, disabled ADR and unreadable data have different outcomes.
use crate::{
    read::{NativeRead, ReadError, boolean, integer, invalid_data, optional_counter, optional_integer, optional_text},
    sql_time::native_time,
    value::{Rec, Val},
};
use culprit_core::{Result, telemetry::*};

const LAYOUT: &str = r#"SELECT CAST(SERVERPROPERTY('EngineEdition') AS int) AS edition,
 CAST(SERVERPROPERTY('ProductMajorVersion') AS int) AS major,
 CASE WHEN OBJECT_ID(N'sys.dm_tran_persistent_version_store_stats') IS NOT NULL THEN 1 ELSE 0 END AS native_view,
 CASE WHEN EXISTS(SELECT 1 FROM sys.all_columns WHERE object_id=OBJECT_ID(N'sys.databases')
 AND name=N'is_accelerated_database_recovery_on') THEN 1 ELSE 0 END AS adr_column"#;
const ADR: &str = "SELECT CAST(is_accelerated_database_recovery_on AS int) AS adr FROM sys.databases WHERE database_id=DB_ID() OPTION(MAXDOP 1)";
const STATISTICS: &str =
    "SELECT * FROM sys.dm_tran_persistent_version_store_stats WHERE database_id=DB_ID() OPTION(MAXDOP 1)";

/// None is a known unsupported feature or disabled ADR; errors never become absence or a zero size.
pub(crate) async fn read<R: NativeRead>(reader: &mut R) -> Result<Option<MssqlPersistentVersionStore>> {
    let layout = reader.query(LAYOUT, vec![], 2).await.map_err(ReadError::into_error)?;
    let [layout] = layout.as_slice() else { return Err(invalid_data()) };
    let edition = integer(layout, "edition")?;
    let major = optional_integer(layout, "major")?;
    if edition == 0 || major == Some(0) {
        return Err(invalid_data());
    }
    let cloud = matches!(edition, 5 | 8);
    if !cloud && major.is_some_and(|major| major < 15) {
        return Ok(None);
    }
    if !boolean(layout, "native_view")? || !boolean(layout, "adr_column")? {
        return Err(CollectionStop::SourceUnavailable.error());
    }
    let adr = reader.query(ADR, vec![], 2).await.map_err(ReadError::into_error)?;
    let [adr] = adr.as_slice() else { return Err(invalid_data()) };
    if !boolean(adr, "adr")? {
        return Ok(None);
    }
    let rows = reader.query(STATISTICS, vec![], 2).await.map_err(ReadError::into_error)?;
    let [row] = rows.as_slice() else { return Err(invalid_data()) };
    let sample = decode(row)?;
    if !sample.is_valid() {
        return Err(invalid_data());
    }
    Ok(Some(sample))
}
fn has(row: &Rec, name: &str) -> bool {
    row.columns().any(|(column, _)| column == name)
}
fn counter(row: &Rec, name: &str) -> Result<Option<String>> {
    if has(row, name) { optional_counter(row, name) } else { Ok(None) }
}
fn native_sequence(row: &Rec, name: &str) -> Result<Option<String>> {
    match row.columns().find(|(column, _)| *column == name).map(|(_, value)| value) {
        None | Some(Val::Null) => Ok(None),
        Some(Val::Int(value)) => Ok(Some(value.to_string())),
        _ => Err(invalid_data()),
    }
}
fn local_time(row: &Rec, name: &str) -> Result<Option<String>> {
    if !has(row, name) {
        return Ok(None);
    }
    optional_text(row, name)?.map(|value| native_time(&value)).transpose()
}
fn cleanup(row: &Rec, start: &str, end: &str) -> Result<MssqlVersionCleanupSweep> {
    Ok(MssqlVersionCleanupSweep {
        started_without_offset: local_time(row, start)?,
        ended_without_offset: local_time(row, end)?,
    })
}
fn decode(row: &Rec) -> Result<MssqlPersistentVersionStore> {
    let mut columns = row.columns().map(|(name, _)| name.to_string()).collect::<Vec<_>>();
    columns.sort();
    Ok(MssqlPersistentVersionStore {
        available_columns: columns,
        filegroup_id: optional_integer(row, "pvs_filegroup_id")?
            .map(|value| value.try_into().map_err(|_| invalid_data()))
            .transpose()?,
        off_row_kibibytes: optional_counter(row, "persistent_version_store_size_kb")?,
        online_index_kibibytes: counter(row, "online_index_version_store_size_kb")?,
        aborted_transactions: counter(row, "current_aborted_transaction_count")?,
        oldest_active_transaction_id: native_sequence(row, "oldest_active_transaction_id")?,
        oldest_aborted_transaction_id: native_sequence(row, "oldest_aborted_transaction_id")?,
        minimum_transaction_timestamp: native_sequence(row, "min_transaction_timestamp")?,
        online_index_minimum_timestamp: native_sequence(row, "online_index_min_transaction_timestamp")?,
        secondary_low_water_mark: native_sequence(row, "secondary_low_water_mark")?,
        off_row_cleanup: cleanup(row, "offrow_version_cleaner_start_time", "offrow_version_cleaner_end_time")?,
        aborted_cleanup: cleanup(row, "aborted_version_cleaner_start_time", "aborted_version_cleaner_end_time")?,
        pages_skipped_for_secondaries: counter(row, "pvs_off_row_page_skipped_low_water_mark")?,
        pages_skipped_for_aborted_transactions: counter(row, "pvs_off_row_page_skipped_transaction_not_cleaned")?,
    })
}
pub(crate) fn observation(sample: MssqlPersistentVersionStore) -> OperationalObservation {
    OperationalObservation {
        source: crate::sources::PERSISTENT_VERSION_STORE.name.into(),
        key: "persistent-version-store".into(),
        fact: OperationalFact::MssqlPersistentVersionStore(Box::new(sample)),
    }
}
pub(crate) fn signal(sample: &MssqlPersistentVersionStore) -> Option<culprit_core::Signal> {
    if !sample.is_valid() {
        return None;
    }
    let bytes = sample.off_row_kibibytes.as_ref()?.parse::<u64>().ok()? as f64 * 1024.0;
    // Storage alone has no universal pressure threshold; this is a measurement, never an independent alarm.
    let mut signal = culprit_core::Signal::new(
        culprit_core::signal_id::PERSISTENT_VERSION_STORE,
        culprit_core::message!("provider.mssql.persistent_version_store.off_row"),
        bytes,
        culprit_core::SignalUnit::Bytes,
        culprit_core::SignalScope::Now,
    );
    signal.measurement_scope = Some(culprit_core::MeasurementScope::Database);
    Some(signal)
}

#[cfg(test)]
mod native_tests;
#[cfg(test)]
pub(crate) mod tests;
