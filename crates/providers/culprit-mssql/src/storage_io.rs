//! Targeted file statistics shared by interactive storage analysis and raw collection.
use crate::read::{NativeRead, ReadError, integer, invalid_data, optional_text};
use crate::value::Rec;
use culprit_core::{
    FileIo, FileKind, FileScope, ResetMarker, Result,
    telemetry::{CollectionStop, FileIoCounters, OperationalFact, OperationalObservation},
};
use std::collections::BTreeSet;

const MAX_NATIVE_FILES: usize = 10_000;
const MAX_DISPLAY_FILES: usize = 24;
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Selection {
    Database,
    DatabaseAndSharedTempdb,
}
const COUNTERS: &str = "vfs.database_id, vfs.file_id, vfs.num_of_reads AS reads,
vfs.io_stall_read_ms AS read_ms, vfs.num_of_bytes_read AS read_bytes,
vfs.num_of_writes AS writes, vfs.io_stall_write_ms AS write_ms, vfs.num_of_bytes_written AS write_bytes,
vfs.size_on_disk_bytes, vfs.sample_ms";

fn sql(selection: Selection) -> String {
    // Filter the native function itself: enumerating every database and filtering afterwards needlessly visits them.
    let current = format!(
        "SELECT {COUNTERS}, DB_NAME() AS database_name, df.name AS file_name,
CAST(df.type AS varchar(16)) AS native_type, CAST(0 AS int) AS shared
FROM sys.dm_io_virtual_file_stats(DB_ID(), NULL) vfs
LEFT JOIN sys.database_files df ON df.file_id = vfs.file_id
WHERE vfs.database_id = DB_ID()"
    );
    let tempdb = if selection == Selection::DatabaseAndSharedTempdb {
        format!(
            " UNION ALL SELECT {COUNTERS}, N'tempdb' AS database_name, mf.name AS file_name,
CAST(mf.type AS varchar(16)) AS native_type, CAST(1 AS int) AS shared
FROM sys.dm_io_virtual_file_stats(2, NULL) vfs
LEFT JOIN sys.master_files mf ON mf.database_id = 2 AND mf.file_id = vfs.file_id
WHERE vfs.database_id = 2 AND DB_ID() <> 2"
        )
    } else {
        String::new()
    };
    format!("{current}{tempdb} OPTION(MAXDOP 1)")
}

pub(crate) async fn read<R: NativeRead>(
    reader: &mut R,
    selection: Selection,
    limit: usize,
) -> Result<Vec<FileIoCounters>> {
    let limit = limit.min(MAX_NATIVE_FILES);
    let rows = reader.query(&sql(selection), vec![], limit + 1).await.map_err(ReadError::into_error)?;
    if rows.len() > limit {
        return Err(CollectionStop::RowLimit.error());
    }
    let mut identities = BTreeSet::new();
    rows.iter()
        .map(|row| {
            let counters = decode(row)?;
            if !identities.insert((counters.native_database_id.clone(), counters.file_id.clone())) {
                return Err(invalid_data());
            }
            Ok(counters)
        })
        .collect()
}
fn decode(row: &Rec) -> Result<FileIoCounters> {
    let native_type = optional_text(row, "native_type")?;
    if native_type.as_ref().is_some_and(|value| value.parse::<u32>().is_err()) {
        return Err(invalid_data());
    }
    let shared = match integer(row, "shared")? {
        0 => false,
        1 => true,
        _ => return Err(invalid_data()),
    };
    let kind = match (native_type.as_deref(), shared) {
        (Some("1"), _) => FileKind::Log,
        (Some("0"), true) => FileKind::Temp,
        (Some("0"), false) => FileKind::Data,
        _ => FileKind::Other,
    };
    let counters = FileIoCounters {
        native_database_id: integer(row, "database_id")?.to_string(),
        database: optional_text(row, "database_name")?,
        file_id: integer(row, "file_id")?.to_string(),
        native_name: optional_text(row, "file_name")?,
        native_type,
        kind,
        scope: if shared { FileScope::Server } else { FileScope::Database },
        reset: ResetMarker::Unknown,
        reads: integer(row, "reads")?.to_string(),
        read_ms: Some(integer(row, "read_ms")?.to_string()),
        read_bytes: Some(integer(row, "read_bytes")?.to_string()),
        writes: integer(row, "writes")?.to_string(),
        write_ms: Some(integer(row, "write_ms")?.to_string()),
        write_bytes: Some(integer(row, "write_bytes")?.to_string()),
        size_on_disk_bytes: Some(integer(row, "size_on_disk_bytes")?.to_string()),
        sample_uptime_ms: Some(integer(row, "sample_ms")?.to_string()),
    };
    if counters.native_database_id == "0" || counters.file_id == "0" {
        return Err(invalid_data());
    }
    if !counters.is_valid() {
        return Err(invalid_data());
    }
    Ok(counters)
}
pub(crate) fn observations(rows: Vec<FileIoCounters>) -> Vec<OperationalObservation> {
    rows.into_iter()
        .map(|row| OperationalObservation {
            source: culprit_core::source_id::FILE_LATENCY.into(),
            key: format!("{}:{}", row.native_database_id, row.file_id),
            fact: OperationalFact::FileIo(row),
        })
        .collect()
}
pub(crate) fn display(rows: &[FileIoCounters]) -> Vec<FileIo> {
    let number = |value: &str| value.parse::<u64>().expect("validated native file counter") as f64;
    let mut files: Vec<_> = rows
        .iter()
        .filter(|row| row.reads != "0" || row.writes != "0")
        .map(|row| FileIo {
            name: row.native_name.clone().unwrap_or_else(|| row.file_id.clone()),
            kind: row.kind,
            database: row.database.clone(),
            reads: number(&row.reads),
            read_ms: row.read_ms.as_deref().map(number),
            read_bytes: row.read_bytes.as_deref().map(number),
            writes: number(&row.writes),
            write_ms: row.write_ms.as_deref().map(number),
            write_bytes: row.write_bytes.as_deref().map(number),
            scope: row.database.as_ref().map(|_| row.scope),
            stats_reset: None,
            write_operation: None,
        })
        .collect();
    let stall = |file: &FileIo| file.read_ms.unwrap_or_default() + file.write_ms.unwrap_or_default();
    files.sort_by(|a, b| {
        stall(b).total_cmp(&stall(a)).then_with(|| a.database.cmp(&b.database)).then_with(|| a.name.cmp(&b.name))
    });
    files.truncate(MAX_DISPLAY_FILES);
    files
}

#[cfg(test)]
pub(crate) mod tests;
