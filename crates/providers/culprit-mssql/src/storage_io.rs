//! Targeted file statistics shared by interactive storage analysis and raw collection.
use crate::read::{NativeRead, ReadError, integer, invalid_data, optional_text};
use crate::value::Rec;
use culprit_core::{
    FileIo, FileKind, FileScope, ResetMarker, Result,
    telemetry::{CollectionStop, FileIoCounters, OperationalFact, OperationalObservation},
};
use std::collections::BTreeSet;

mod context;

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
CAST(df.type AS varchar(16)) AS native_type, CAST(0 AS int) AS shared, CAST(0 AS int) AS cache,
CASE WHEN @engine_edition IN (2,3,4,8) AND DB_ID() = 2 THEN 1 ELSE 0 END AS temporary,
@engine_edition AS engine_edition, @database_edition AS database_edition,
@replica_id AS replica_id, @updateability AS updateability
FROM sys.dm_io_virtual_file_stats(DB_ID(), NULL) vfs
LEFT JOIN sys.database_files df ON df.file_id = vfs.file_id
WHERE vfs.database_id = DB_ID()"
    );
    let tempdb = if selection == Selection::DatabaseAndSharedTempdb {
        format!(
            " UNION ALL SELECT {COUNTERS}, N'tempdb' AS database_name, mf.name AS file_name,
CAST(mf.type AS varchar(16)) AS native_type, CAST(1 AS int) AS shared, CAST(0 AS int) AS cache,
CAST(1 AS int) AS temporary, @engine_edition AS engine_edition, @database_edition AS database_edition,
@replica_id AS replica_id, @updateability AS updateability
FROM sys.dm_io_virtual_file_stats(2, NULL) vfs
LEFT JOIN sys.master_files mf ON mf.database_id = 2 AND mf.file_id = vfs.file_id
WHERE vfs.database_id = 2 AND DB_ID() <> 2"
        )
    } else {
        String::new()
    };
    // database_id=0 is a Hyperscale-specific interface. On conventional SQL Server it must never be called on the
    // assumption that a WHERE predicate will prevent execution. Use a control-flow branch qualified by native
    // properties; unknown editions retain only the explicitly targeted database counters.
    let cache = format!(
        "SELECT {COUNTERS}, CAST(NULL AS nvarchar(128)) AS database_name,
CAST(NULL AS nvarchar(128)) AS file_name, CAST(NULL AS varchar(16)) AS native_type,
CAST(0 AS int) AS shared, CAST(1 AS int) AS cache, CAST(0 AS int) AS temporary,
@engine_edition AS engine_edition, @database_edition AS database_edition,
@replica_id AS replica_id, @updateability AS updateability
FROM sys.dm_io_virtual_file_stats(0, NULL) vfs WHERE vfs.database_id = 0"
    );
    format!(
        "DECLARE @engine_edition int = CONVERT(int, SERVERPROPERTY('EngineEdition'));
DECLARE @database_edition nvarchar(64) = CONVERT(nvarchar(64), DATABASEPROPERTYEX(DB_NAME(), 'Edition'));
DECLARE @replica_id nvarchar(36) = CONVERT(nvarchar(36), TRY_CONVERT(uniqueidentifier,
  CONVERT(nvarchar(128), DATABASEPROPERTYEX(DB_NAME(), 'ReplicaID'))));
DECLARE @updateability nvarchar(60) = CONVERT(nvarchar(60), DATABASEPROPERTYEX(DB_NAME(), 'Updateability'));
IF @engine_edition = 5 AND (@database_edition = N'Hyperscale' OR @replica_id IS NOT NULL)
BEGIN
{current} UNION ALL {cache} OPTION(MAXDOP 1);
END
ELSE
BEGIN
{current}{tempdb} OPTION(MAXDOP 1);
END"
    )
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
    let mut native_context = None;
    rows.iter()
        .map(|row| {
            let context = context::NativeContext::decode(row)?;
            if native_context.as_ref().is_some_and(|previous| previous != &context) {
                return Err(invalid_data());
            }
            let counters = decode(row, &context)?;
            native_context = Some(context);
            if !identities.insert((counters.native_database_id.clone(), counters.file_id.clone())) {
                return Err(invalid_data());
            }
            Ok(counters)
        })
        .collect()
}
fn decode(row: &Rec, context: &context::NativeContext) -> Result<FileIoCounters> {
    let native_type = optional_text(row, "native_type")?;
    if native_type.as_ref().is_some_and(|value| value.parse::<u32>().is_err()) {
        return Err(invalid_data());
    }
    let shared = match integer(row, "shared")? {
        0 => false,
        1 => true,
        _ => return Err(invalid_data()),
    };
    let temporary = crate::read::boolean(row, "temporary")?;
    let cache = crate::read::boolean(row, "cache")?;
    let kind = match (native_type.as_deref(), temporary) {
        (Some("1"), _) => FileKind::Log,
        (Some("0"), true) => FileKind::Temp,
        (Some("0"), false) => FileKind::Data,
        _ => FileKind::Other,
    };
    let counters = FileIoCounters {
        context: context.accounting(row, kind)?,
        native_database_id: integer(row, "database_id")?.to_string(),
        database: optional_text(row, "database_name")?,
        file_id: integer(row, "file_id")?.to_string(),
        native_name: optional_text(row, "file_name")?,
        native_type,
        kind,
        scope: if shared || cache { FileScope::Server } else { FileScope::Database },
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
            context: row.context.clone(),
            name: row.native_name.clone().unwrap_or_else(|| row.file_id.clone()),
            kind: row.kind,
            database: row.database.clone(),
            reads: number(&row.reads),
            read_ms: row.read_ms.as_deref().map(number),
            read_bytes: row.read_bytes.as_deref().map(number),
            writes: number(&row.writes),
            write_ms: row.write_ms.as_deref().map(number),
            write_bytes: row.write_bytes.as_deref().map(number),
            scope: Some(row.scope),
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
