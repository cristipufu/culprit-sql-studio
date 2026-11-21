//! One informational interpretation of current and independently authenticated original indexed-view catalogs.
use super::table_catalog::MssqlCatalogEvidence;
use culprit_core::{
    Engine, IndexedViewIndexEntry, IndexedViewsReport, Result, Snapshot, SourceOutcome, SourceState, source_id, t,
    telemetry::*,
};
const MAX_ENTRIES: usize = 64;
const MAX_COLUMNS: usize = 32;

pub fn explain_snapshot(snapshot: &Snapshot) -> Result<Option<IndexedViewsReport>> {
    let Some(catalog) = snapshot.operations.indexed_views.as_deref() else { return Ok(None) };
    let mut sources = snapshot.sources.iter().filter(|source| source.source == source_id::INDEXES);
    if snapshot.server.engine != Engine::Mssql
        || !catalog.is_valid()
        || sources.next() != Some(&catalog.source)
        || sources.next().is_some()
    {
        return Err(invalid());
    }
    let mut report = render(&catalog.indexes, catalog.source.clone(), None)?;
    report.native_index_rows = catalog.retained_index_rows;
    report.observed_index_rows = catalog.retained_index_rows.unwrap_or(0);
    if !catalog.parent_prefix_complete {
        report.limitations.push(t!("view.indexed_views.partial"));
    }
    report.limitations.push(t!("view.indexed_views.limit.scan"));
    Ok(Some(report))
}
pub fn explain(evidence: MssqlCatalogEvidence<'_>) -> Result<IndexedViewsReport> {
    let context = evidence.validate(SnapshotDataset::Indexes)?;
    let native = evidence.description.observation.snapshot().ok_or_else(invalid)?;
    let [coverage] = native.sources.as_slice() else { return Err(invalid()) };
    let indexes = evidence
        .observations
        .iter()
        .filter_map(|row| match &row.fact {
            OperationalFact::MssqlIndex(index) if index.parent_kind == MssqlIndexParentKind::IndexedView => {
                Some((index.as_ref(), Some(row.key.as_str())))
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    let mut report = render_entries(&indexes, evidence.source.clone(), Some(evidence.read_complete))?;
    report.context = Some(context);
    report.native_index_rows = Some(coverage.records);
    report.observed_index_rows = evidence.observations.len() as u32;
    Ok(report)
}
pub fn explain_captured(read: &CapturedOperationalRead) -> Result<IndexedViewsReport> {
    let source =
        super::table_catalog::original_outcome(read, culprit_core::message!("view.indexed_views.partial"), |code| {
            culprit_core::message!(
                "view.indexed_views.source_error",
                reason = match code {
                    culprit_core::ErrorCode::PermissionDenied => "permission",
                    culprit_core::ErrorCode::Unsupported => "unsupported",
                    _ => "read",
                }
            )
        })?;
    explain(MssqlCatalogEvidence {
        description: &read.description,
        observations: &read.observations.iter().collect::<Vec<_>>(),
        source: &source,
        read_complete: read.read_complete,
    })
}
pub fn explain_recorded(original: &RecordedOperationalRead) -> Result<IndexedViewsReport> {
    match original {
        RecordedOperationalRead::Captured { read } => explain_captured(read),
        RecordedOperationalRead::Unavailable { reason } => Ok(unavailable(reason)),
    }
}
pub fn unavailable(reason: &culprit_core::i18n::Message) -> IndexedViewsReport {
    let source = SourceOutcome {
        source: source_id::INDEXES.into(),
        state: SourceState::Unavailable,
        reason: Some(reason.render()),
        reason_message: None,
    };
    // An explicit withdrawn carrier has no retained native population or replacement current scope.
    report(source, Some(false), Vec::new(), 0)
}
fn render(
    indexes: &[MssqlIndexSnapshot],
    source: SourceOutcome,
    read_complete: Option<bool>,
) -> Result<IndexedViewsReport> {
    let rows = indexes.iter().map(|index| (index, None)).collect::<Vec<_>>();
    render_entries(&rows, source, read_complete)
}
fn render_entries(
    indexes: &[(&MssqlIndexSnapshot, Option<&str>)],
    source: SourceOutcome,
    read_complete: Option<bool>,
) -> Result<IndexedViewsReport> {
    let visible = !matches!(source.state, SourceState::Unavailable | SourceState::NotApplicable);
    let entries = if visible {
        indexes.iter().take(MAX_ENTRIES).map(|(index, key)| entry(index, *key)).collect::<Result<Vec<_>>>()?
    } else {
        Vec::new()
    };
    Ok(report(source, read_complete, entries, if visible { indexes.len() as u32 } else { 0 }))
}
fn report(
    source: SourceOutcome,
    read_complete: Option<bool>,
    entries: Vec<IndexedViewIndexEntry>,
    observed: u32,
) -> IndexedViewsReport {
    let mut limitations = vec![
        t!("view.indexed_views.limit.definition"),
        t!("view.indexed_views.limit.usage"),
        t!("view.indexed_views.limit.visibility"),
        t!("view.indexed_views.verify"),
        t!("view.index_configuration.limit.identity"),
        t!("view.index_configuration.limit.time"),
    ];
    if source.state == SourceState::Partial || read_complete == Some(false) {
        limitations.push(t!("view.indexed_views.partial"));
    }
    if entries.len() < observed as usize {
        limitations.push(t!("view.indexed_views.presentation_limited", shown = entries.len(), total = observed));
    }
    let summary = if matches!(source.state, SourceState::Unavailable | SourceState::NotApplicable) {
        t!("view.indexed_views.unavailable")
    } else {
        t!("view.indexed_views.summary", count = observed)
    };
    IndexedViewsReport {
        context: None,
        source,
        read_complete,
        native_index_rows: None,
        observed_index_rows: 0,
        observed_view_indexes: observed,
        entries_limited: entries.len() < observed as usize,
        title: t!("view.indexed_views.title"),
        summary,
        qualification: t!("view.indexed_views.qualification"),
        entries,
        limitations,
    }
}
fn entry(index: &MssqlIndexSnapshot, reference: Option<&str>) -> Result<IndexedViewIndexEntry> {
    if !index.is_valid() || index.parent_kind != MssqlIndexParentKind::IndexedView {
        return Err(invalid());
    }
    Ok(IndexedViewIndexEntry {
        observation_key: reference.map(str::to_owned),
        parent_object: index.parent_object.clone(),
        parent_schema: index.parent_schema.clone(),
        parent_name: index.parent_name.clone(),
        parent_modified_without_offset: index.parent_modified_without_offset.clone(),
        index_id: index.index_id,
        index_name: index.name.clone(),
        type_code: index.type_code,
        native_type: index.native_type.clone(),
        unique: index.unique,
        disabled: index.disabled,
        reserved_bytes: index.reserved_bytes.clone(),
        native_columns: index.columns.len() as u32,
        columns_limited: index.columns.len() > MAX_COLUMNS,
        columns: index.columns.iter().take(MAX_COLUMNS).cloned().collect(),
        usage: index.usage.clone(),
        configuration: index
            .configuration
            .as_deref()
            .map(|config| super::index_configuration::entry(config, reference).map(Box::new))
            .transpose()?,
    })
}
fn invalid() -> culprit_core::Error {
    CollectionStop::InvalidPage.error()
}
#[cfg(test)]
mod tests;
