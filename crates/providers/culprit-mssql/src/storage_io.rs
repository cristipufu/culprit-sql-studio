//! Targeted file statistics shared by interactive storage analysis and raw collection.
use crate::read::{
    NativeRead, ReadError, integer, invalid_data, optional_counter, optional_signed_integer, optional_text,
};
use crate::value::Rec;
use culprit_core::{
    FileIo, FileKind, FileScope, ResetMarker, Result,
    telemetry::{CollectionStop, FileIoCounters, FileSize, FileSizeLimit, OperationalFact, OperationalObservation},
};
use std::collections::BTreeSet;

mod context;
mod sql;

const MAX_NATIVE_FILES: usize = 10_000;
const MAX_DISPLAY_FILES: usize = 24;
const BYTES_PER_FILE_PAGE: u64 = 8192;
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Selection {
    Database,
    DatabaseAndSharedTempdb,
}
pub(crate) async fn read<R: NativeRead>(
    reader: &mut R,
    selection: Selection,
    limit: usize,
) -> Result<Vec<FileIoCounters>> {
    let limit = limit.min(MAX_NATIVE_FILES);
    let rows = reader.query(&sql::statement(selection), vec![], limit + 1).await.map_err(ReadError::into_error)?;
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
        queued_read_ms: optional_counter(row, "queued_read_ms")?,
        read_bytes: Some(integer(row, "read_bytes")?.to_string()),
        writes: integer(row, "writes")?.to_string(),
        write_ms: Some(integer(row, "write_ms")?.to_string()),
        queued_write_ms: optional_counter(row, "queued_write_ms")?,
        write_bytes: Some(integer(row, "write_bytes")?.to_string()),
        logical_size: logical_size(row)?,
        maximum_size: maximum_size(row)?,
        size_on_disk_bytes: Some(integer(row, "size_on_disk_bytes")?.to_string()),
        sample_uptime_ms: Some(integer(row, "sample_ms")?.to_string()),
    };
    if !counters.is_valid() {
        return Err(invalid_data());
    }
    Ok(counters)
}

fn logical_size(row: &Rec) -> Result<Option<FileSize>> {
    // This sentinel belongs specifically to sys.database_files.size. A generic measured zero remains bytes=0.
    // https://learn.microsoft.com/sql/relational-databases/system-catalog-views/sys-database-files-transact-sql
    match optional_signed_integer(row, "logical_pages")? {
        None => Ok(None),
        Some(0) => Ok(Some(FileSize::NotApplicable)),
        Some(pages) => Ok(Some(FileSize::Bytes { bytes: file_bytes(pages)? })),
    }
}

fn maximum_size(row: &Rec) -> Result<Option<FileSizeLimit>> {
    match optional_signed_integer(row, "maximum_pages")? {
        None => Ok(None),
        Some(-1) => Ok(Some(FileSizeLimit::Unlimited)),
        Some(0) => Ok(Some(FileSizeLimit::NoGrowth)),
        Some(pages) => Ok(Some(FileSizeLimit::Bytes { bytes: file_bytes(pages)? })),
    }
}

fn file_bytes(pages: i64) -> Result<String> {
    // The catalog exposes int pages, not bigint bytes. Reject malformed native values before multiplication.
    let pages = u64::try_from(i32::try_from(pages).map_err(|_| invalid_data())?).map_err(|_| invalid_data())?;
    pages.checked_mul(BYTES_PER_FILE_PAGE).map(|bytes| bytes.to_string()).ok_or_else(invalid_data)
}
pub(crate) fn observations(rows: Vec<FileIoCounters>) -> Vec<OperationalObservation> {
    rows.into_iter()
        .map(|row| OperationalObservation {
            source: culprit_core::source_id::FILE_LATENCY.into(),
            key: format!("{}:{}", row.native_database_id, row.file_id),
            fact: OperationalFact::FileIo(Box::new(row)),
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
            queued_read_ms: row.queued_read_ms.clone(),
            read_bytes: row.read_bytes.as_deref().map(number),
            writes: number(&row.writes),
            write_ms: row.write_ms.as_deref().map(number),
            queued_write_ms: row.queued_write_ms.clone(),
            write_bytes: row.write_bytes.as_deref().map(number),
            logical_size: row.logical_size.clone(),
            maximum_size: row.maximum_size.clone(),
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
