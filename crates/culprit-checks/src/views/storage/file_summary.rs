//! Only disjoint native file rows from one complete original census contribute; overlapping groups do not.
use super::*;
use culprit_core::{Engine, FileKind, StorageFileSummary};

struct Census<'a> {
    files: Vec<&'a MssqlDatabaseFile>,
    observed_at: String,
    database_id: &'a str,
}
pub(super) fn explain(read: &StorageRead) -> Option<Box<StorageFileSummary>> {
    if read.target.engine() != Engine::Mssql || read.target.database().is_none() {
        return None;
    }
    let database = read.target.database()?;
    let source = read.sources.iter().find(|source| source.source == culprit_core::source_id::FILE_CAPACITY)?;
    let census = census(read, source);
    let mut result = StorageFileSummary {
        database: database.database.clone(),
        source: source.source.clone(),
        source_state: if census.is_some() {
            SourceState::Collected
        } else if source.state == SourceState::Unavailable || source.state == SourceState::NotApplicable {
            source.state
        } else {
            SourceState::Partial
        },
        observed_at: census.as_ref().map(|census| census.observed_at.clone()),
        native_database_id: census.as_ref().map(|census| census.database_id.to_owned()),
        data_file_count: None,
        log_file_count: None,
        other_file_count: None,
        data_allocated_bytes: None,
        data_used_bytes: None,
        log_catalog_allocated_bytes: None,
        title: t!("view.storage.file_summary.title"),
        summary: if census.is_some() {
            t!("view.storage.file_summary.recorded")
        } else {
            t!("view.storage.file_summary.unavailable")
        },
        fields: vec![],
        limitations: vec![
            t!("view.storage.file_summary.limit.types"),
            t!("view.storage.file_summary.limit.log"),
            t!("view.storage.file_summary.limit.overlap"),
            t!("view.storage.file_summary.limit.original"),
        ],
    };
    if let Some(census) = census {
        let data = census.files.iter().copied().filter(|file| file.kind == FileKind::Data).collect::<Vec<_>>();
        let log = census.files.iter().copied().filter(|file| file.kind == FileKind::Log).collect::<Vec<_>>();
        result.data_file_count = u32::try_from(data.len()).ok();
        result.log_file_count = u32::try_from(log.len()).ok();
        result.other_file_count = u32::try_from(census.files.len() - data.len() - log.len()).ok();
        result.data_allocated_bytes = allocated_bytes(&data);
        result.data_used_bytes = used_bytes(&data);
        result.log_catalog_allocated_bytes = allocated_bytes(&log);
        if result.data_allocated_bytes.is_none()
            || result.data_used_bytes.is_none()
            || result.log_catalog_allocated_bytes.is_none()
        {
            result.limitations.push(t!("view.storage.file_summary.limit.quantities"));
        }
    }
    if read.context.native().is_none() {
        result.limitations.push(t!("view.storage.file_summary.limit.collector_clock"));
    }
    result.fields = fields(&result);
    Some(Box::new(result))
}
fn census<'a>(read: &'a StorageRead, source: &StateSourceCoverage) -> Option<Census<'a>> {
    if source.state != SourceState::Collected
        || !source.complete_scope
        || source.error_code.is_some()
        || source.stopped.is_some()
    {
        return None;
    }
    if read.context.native().is_some_and(|context| {
        !matches!(&context.partition, SourcePartition::Target) && !context.has_qualified_local_owner()
    }) {
        return None;
    }
    let rows = read.observations.iter().filter(|row| row.source == source.source).collect::<Vec<_>>();
    if rows.len() != source.records as usize {
        return None;
    }
    let files = rows
        .iter()
        .filter_map(|row| match &row.fact {
            OperationalFact::MssqlDatabaseFile(file) => Some(file.as_ref()),
            _ => None,
        })
        .collect::<Vec<_>>();
    let first = *files.first()?;
    let observed_at = canonical_time(&first.observed_at).ok()?;
    for file in &files {
        if file.native_database_id != first.native_database_id
            || canonical_time(&file.observed_at).ok().as_deref() != Some(observed_at.as_str())
        {
            return None;
        }
    }
    for row in rows {
        match &row.fact {
            OperationalFact::MssqlDatabaseFile(_) => (),
            OperationalFact::Space(space)
                if space.population == StoragePopulation::WritableOnlineDataFilegroups
                    && canonical_time(&space.observed_at).ok().as_deref() == Some(observed_at.as_str()) =>
            {
                ()
            }
            _ => return None,
        }
    }
    Some(Census { files, observed_at, database_id: &first.native_database_id })
}
fn allocated_bytes(files: &[&MssqlDatabaseFile]) -> Option<String> {
    page_bytes(files.iter().map(|file| {
        if file.size_basis != StorageFileSizeBasis::Allocated {
            return None;
        }
        file.size_pages.parse::<u128>().ok().filter(|pages| *pages > 0)
    }))
}
fn used_bytes(files: &[&MssqlDatabaseFile]) -> Option<String> {
    page_bytes(files.iter().map(|file| {
        let used = file.used_pages.as_deref()?.parse::<u128>().ok()?;
        if file.size_basis == StorageFileSizeBasis::Allocated {
            let allocated = file.size_pages.parse::<u128>().ok()?;
            if allocated > 0 && used > allocated {
                return None;
            }
        }
        Some(used)
    }))
}
fn page_bytes(mut pages: impl Iterator<Item = Option<u128>>) -> Option<String> {
    pages
        .try_fold(0u128, |total, pages| total.checked_add(pages?))?
        .checked_mul(u128::from(MssqlDatabaseFile::PAGE_SIZE_BYTES))
        .map(|bytes| bytes.to_string())
}
fn fields(summary: &StorageFileSummary) -> Vec<StorageField> {
    vec![
        field("database", t!("view.storage.file_summary.field.database"), Some(summary.database.clone())),
        field("observed_at", t!("view.storage.file_summary.field.observed_at"), summary.observed_at.clone()),
        field(
            "native_database_id",
            t!("view.storage.file_summary.field.database_id"),
            summary.native_database_id.clone(),
        ),
        number(
            "data_file_count",
            t!("view.storage.file_summary.field.data_count"),
            &summary.data_file_count.map(|count| count.to_string()),
        ),
        number(
            "log_file_count",
            t!("view.storage.file_summary.field.log_count"),
            &summary.log_file_count.map(|count| count.to_string()),
        ),
        number(
            "other_file_count",
            t!("view.storage.file_summary.field.other_count"),
            &summary.other_file_count.map(|count| count.to_string()),
        ),
        number(
            "data_allocated_bytes",
            t!("view.storage.file_summary.field.data_allocated"),
            &summary.data_allocated_bytes,
        ),
        number("data_used_bytes", t!("view.storage.file_summary.field.data_used"), &summary.data_used_bytes),
        number(
            "log_catalog_allocated_bytes",
            t!("view.storage.file_summary.field.log_allocated"),
            &summary.log_catalog_allocated_bytes,
        ),
    ]
}
#[cfg(test)]
mod tests;
