//! The source's allocation populations and visible files remain separate, exact and bounded in every consumer.
use culprit_core::{
    Result, Snapshot, SourceOutcome, SourceState, StorageEntry, StorageField, StorageReport, format, t, telemetry::*,
};
pub const MAX_STORAGE_ENTRIES: usize = 256;
mod allocations;
pub(crate) use allocations::AllocationEvidence;

pub fn explain_snapshot(snapshot: &Snapshot) -> Result<Option<StorageReport>> {
    snapshot.operations.storage.as_deref().map(explain).transpose()
}
pub fn explain_captured(read: &CapturedOperationalRead) -> Result<StorageReport> {
    if read.description.observation.snapshot().is_none_or(|header| {
        header.dataset != SnapshotDataset::Operational
            || !header.sources.iter().any(|source| StorageRead::recognizes(&source.source))
    }) {
        return Err(DeliveryProblem::UnsupportedSchema.error());
    }
    if !read.is_valid() {
        return Err(CollectionStop::InvalidPage.error());
    }
    explain(&StorageRead::from_source(&read.description, read.observations.clone(), read.read_complete)?)
}
pub fn explain(read: &StorageRead) -> Result<StorageReport> {
    if !read.is_valid() {
        return Err(CollectionStop::InvalidPage.error());
    }
    let native_rows = read
        .sources
        .iter()
        .try_fold(0u32, |total, source| total.checked_add(source.records))
        .ok_or_else(|| CollectionStop::InvalidPage.error())?;
    let observed_rows = read.observations.len() as u32;
    let entries = read.observations.iter().take(MAX_STORAGE_ENTRIES).map(entry).collect::<Result<Vec<_>>>()?;
    let entries_limited = read.observations.len() > entries.len();
    let mut limitations =
        vec![t!("view.storage.limit.population"), t!("view.storage.limit.files"), t!("view.storage.limit.growth")];
    if !read.read_complete {
        limitations.push(t!("view.storage.limit.read"));
    }
    if entries_limited {
        limitations.push(t!("view.storage.limit.presentation"));
    }
    let state = if read.sources.iter().all(|source| source.state == SourceState::NotApplicable) {
        SourceState::NotApplicable
    } else if read.sources.iter().all(|source| source.state == SourceState::Unavailable) {
        SourceState::Unavailable
    } else if !read.read_complete
        || read.sources.iter().any(|source| matches!(source.state, SourceState::Partial | SourceState::Unavailable))
    {
        SourceState::Partial
    } else if observed_rows == 0 {
        SourceState::Empty
    } else {
        SourceState::Collected
    };
    Ok(StorageReport {
        target: read.target.clone(),
        context: read.context.clone(),
        native_sources: read.sources.clone(),
        sources: read
            .sources
            .iter()
            .map(|source| {
                let rows = read.observations.iter().filter(|row| row.source == source.source).count() as u32;
                let missing =
                    matches!(source.state, SourceState::Collected | SourceState::Empty | SourceState::Partial)
                        && (!read.read_complete || rows != source.records);
                SourceOutcome {
                    source: source.source.clone(),
                    state: if missing { SourceState::Partial } else { source.state },
                    reason: if missing {
                        Some(t!("view.storage.limit.read"))
                    } else if let Some(stop) = source.stopped {
                        Some(stop.error().message)
                    } else if source.error_code.is_some() {
                        Some(t!("view.storage.summary.unavailable"))
                    } else {
                        None
                    },
                    reason_message: None,
                }
            })
            .collect(),
        source_state: state,
        read_complete: read.read_complete,
        native_rows,
        observed_rows,
        presented_rows: entries.len() as u32,
        entries_limited,
        title: t!("view.storage.title"),
        summary: if observed_rows == 0 {
            t!("view.storage.summary.unavailable")
        } else {
            t!("view.storage.summary.recorded")
        },
        limitations,
        entries,
    })
}
fn field(key: &str, label: String, value: Option<String>) -> StorageField {
    StorageField { key: key.into(), label, value }
}
fn number(key: &str, label: String, value: &Option<String>) -> StorageField {
    field(key, label, value.clone().map(format::digits))
}
fn boolean(key: &str, label: String, value: Option<bool>) -> StorageField {
    field(key, label, value.map(|v| if v { t!("view.storage.yes") } else { t!("view.storage.no") }))
}
fn allocation_fields(space: &OperationalSpace) -> Vec<StorageField> {
    let mut fields = vec![
        field("observed_at", t!("view.storage.field.observed_at"), Some(space.observed_at.clone())),
        number("total_bytes", t!("view.storage.field.total_bytes"), &space.total_bytes),
        number("used_bytes", t!("view.storage.field.used_bytes"), &space.used_bytes),
        number("limit_bytes", t!("view.storage.field.limit_bytes"), &space.limit_bytes),
        boolean("growth_enabled", t!("view.storage.field.growth_enabled"), space.growth_enabled),
        field("reuse_wait", t!("view.storage.field.reuse_wait"), space.reuse_wait.clone()),
    ];
    if space.population == StoragePopulation::SharedTempdbDataFiles {
        for (kind, key, label) in [
            (SpaceAllocationKind::UserObjects, "user_bytes", t!("view.storage.field.user_bytes")),
            (SpaceAllocationKind::InternalObjects, "internal_bytes", t!("view.storage.field.internal_bytes")),
            (SpaceAllocationKind::VersionStore, "version_bytes", t!("view.storage.field.version_bytes")),
        ] {
            fields.push(number(
                key,
                label,
                &space.parts.iter().find(|part| part.kind == kind).and_then(|part| part.bytes.clone()),
            ));
        }
    }
    fields
}
fn entry(row: &OperationalObservation) -> Result<StorageEntry> {
    let (scope, title, explanation, fields) = match &row.fact {
        OperationalFact::Space(space) => (
            space.scope,
            space.native_name.clone().unwrap_or_else(|| population(space.population)),
            population(space.population),
            allocation_fields(space),
        ),
        OperationalFact::MssqlDatabaseFile(file) => (
            culprit_core::FileScope::Database,
            file.logical_name.clone(),
            basis(file.size_basis),
            vec![
                field("observed_at", t!("view.storage.field.observed_at"), Some(file.observed_at.clone())),
                field("file_id", t!("view.storage.field.file_id"), Some(file.file_id.clone())),
                field("native_type", t!("view.storage.field.native_type"), Some(file.native_type.clone())),
                field("native_state", t!("view.storage.field.native_state"), Some(file.native_state.clone())),
                field("type_description", t!("view.storage.field.type_description"), file.type_description.clone()),
                field("state_description", t!("view.storage.field.state_description"), file.state_description.clone()),
                number("size_pages", t!("view.storage.field.size_pages"), &Some(file.size_pages.clone())),
                field(
                    "page_size_bytes",
                    t!("view.storage.field.page_size_bytes"),
                    Some(format::digits(file.page_size_bytes.to_string())),
                ),
                number("used_pages", t!("view.storage.field.used_pages"), &file.used_pages),
                field(
                    "maximum_size_pages",
                    t!("view.storage.field.maximum_size_pages"),
                    Some(file.maximum_size_pages.clone()),
                ),
                field("growth", t!("view.storage.field.growth"), Some(file.growth.clone())),
                boolean("growth_is_percent", t!("view.storage.field.growth_is_percent"), file.growth_is_percent),
                boolean("is_read_only", t!("view.storage.field.is_read_only"), file.is_read_only),
                boolean("filegroup_read_only", t!("view.storage.field.filegroup_read_only"), file.filegroup_read_only),
                boolean("is_sparse", t!("view.storage.field.is_sparse"), file.is_sparse),
                boolean(
                    "database_is_snapshot",
                    t!("view.storage.field.database_is_snapshot"),
                    file.database_is_snapshot,
                ),
            ],
        ),
        _ => return Err(CollectionStop::InvalidPage.error()),
    };
    Ok(StorageEntry { source: row.source.clone(), observation_key: row.key.clone(), scope, title, explanation, fields })
}
fn population(population: StoragePopulation) -> String {
    match population {
        StoragePopulation::DatabaseTransactionLog => t!("view.storage.population.log"),
        StoragePopulation::WritableOnlineDataFilegroups => t!("view.storage.population.writable"),
        StoragePopulation::SharedTempdbDataFiles => t!("view.storage.population.temp"),
    }
}
fn basis(basis: StorageFileSizeBasis) -> String {
    match basis {
        StorageFileSizeBasis::Allocated => t!("view.storage.basis.allocated"),
        StorageFileSizeBasis::ContainerUsed => t!("view.storage.basis.container"),
        StorageFileSizeBasis::SnapshotMaximum => t!("view.storage.basis.snapshot"),
        StorageFileSizeBasis::Unknown => t!("view.storage.basis.unknown"),
    }
}

#[cfg(test)]
mod tests;
