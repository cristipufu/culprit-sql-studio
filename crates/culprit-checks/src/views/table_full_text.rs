//! One catalog explanation serves scan tables and original retained table rows. It never diagnoses search health.
use culprit_core::{
    Engine, Metric, Result, Snapshot, SourceOutcome, SourceState, TableFullTextEntry, TableFullTextReport, TableStat,
    format, source_id, t, telemetry::*,
};

pub struct TableFullTextEvidence<'a> {
    pub description: &'a CollectionDescription,
    pub observations: &'a [&'a OperationalObservation],
    pub source: &'a SourceOutcome,
    pub read_complete: bool,
}

pub fn explain_snapshot(snapshot: &Snapshot) -> Result<Option<TableFullTextReport>> {
    let source = snapshot.sources.iter().find(|source| source.source == source_id::TABLES);
    let entries = snapshot
        .tables
        .iter()
        .filter_map(|table| qualified_table(snapshot.server.engine, source, table).transpose())
        .collect::<Result<Vec<_>>>()?;
    if entries.is_empty() {
        return Ok(None);
    }
    Ok(Some(report(entries, source, None)))
}

/// The source's original qualification accompanies one already selected table; labels never join captures.
pub fn explain_table(snapshot: &Snapshot, table: &TableStat) -> Result<Option<TableFullTextReport>> {
    let source = snapshot.sources.iter().find(|source| source.source == source_id::TABLES);
    let Some(entry) = qualified_table(snapshot.server.engine, source, table)? else { return Ok(None) };
    Ok(Some(report(vec![entry], source, None)))
}

fn qualified_table(
    engine: Engine,
    source: Option<&SourceOutcome>,
    table: &TableStat,
) -> Result<Option<TableFullTextEntry>> {
    let Some(state) = &table.mssql_full_text else { return Ok(None) };
    if engine != Engine::Mssql || !super::source_coverage::consistent_source(source, [source_id::TABLES]) {
        return Err(invalid());
    }
    entry(&table.schema, &table.name, None, state).map(Some)
}

pub fn explain(evidence: TableFullTextEvidence<'_>) -> Result<TableFullTextReport> {
    let native = evidence.description.observation.snapshot().ok_or_else(invalid)?;
    let context = NativeReadContext::from_snapshot(evidence.description)?;
    let [coverage] = native.sources.as_slice() else { return Err(invalid()) };
    if native.dataset != SnapshotDataset::Tables
        || context.target.engine() != Engine::Mssql
        || context.target.database().is_none()
        || !matches!(context.partition, SourcePartition::Target)
        || coverage.source != source_id::TABLES
        || evidence.source.source != coverage.source
        || evidence.observations.len() > coverage.records as usize
        || evidence.read_complete && evidence.observations.len() != coverage.records as usize
        || !super::source_coverage::consistent_source(
            Some(evidence.source),
            evidence.observations.iter().map(|row| row.source.as_str()),
        )
    {
        return Err(invalid());
    }
    let mut keys = std::collections::BTreeSet::new();
    let entries = evidence
        .observations
        .iter()
        .map(|row| {
            let OperationalFact::MssqlTable(table) = &row.fact else { return Err(invalid()) };
            if !row.is_valid() || !keys.insert(&row.key) {
                return Err(invalid());
            }
            entry(&table.schema, &table.name, Some(row.key.clone()), &table.full_text)
        })
        .collect::<Result<Vec<_>>>()?;
    let mut result = report(entries, Some(evidence.source), Some(evidence.read_complete));
    result.native_rows = Some(coverage.records);
    result.observed_from = Some(context.observed_from);
    result.observed_until = Some(context.observed_until);
    Ok(result)
}

fn entry(
    schema: &str,
    table: &str,
    observation_key: Option<String>,
    state: &MssqlTableFullText,
) -> Result<TableFullTextEntry> {
    if !state.is_valid() {
        return Err(invalid());
    }
    let summary = match state.availability {
        MssqlFullTextCatalogAvailability::Unsupported => t!("view.table_full_text.unsupported"),
        MssqlFullTextCatalogAvailability::PermissionDenied => t!("view.table_full_text.denied"),
        MssqlFullTextCatalogAvailability::Unknown => t!("view.table_full_text.unknown"),
        MssqlFullTextCatalogAvailability::Available if state.index.is_some() => t!("view.table_full_text.present"),
        MssqlFullTextCatalogAvailability::Available => t!("view.table_full_text.absent"),
    };
    let metrics = match &state.index {
        None => vec![],
        Some(index) => vec![
            Metric::said_text(culprit_core::key!("view.table_full_text.field.enabled"), flag(index.enabled)),
            Metric::said_text(
                culprit_core::key!("view.table_full_text.field.tracking"),
                match index.change_tracking {
                    Some(MssqlFullTextChangeTracking::Manual) => t!("view.table_full_text.tracking.manual"),
                    Some(MssqlFullTextChangeTracking::Automatic) => t!("view.table_full_text.tracking.automatic"),
                    Some(MssqlFullTextChangeTracking::Off) => t!("view.table_full_text.tracking.off"),
                    None => t!("view.database_properties.unknown"),
                },
            ),
            Metric::said_text(
                culprit_core::key!("view.table_full_text.field.last_crawl"),
                flag(index.last_crawl_completed),
            ),
        ],
    };
    Ok(TableFullTextEntry {
        schema: schema.into(),
        table: table.into(),
        observation_key,
        summary,
        metrics,
        qualification: t!("view.table_full_text.limit.catalog"),
        verify: t!("view.table_full_text.verify"),
    })
}

fn flag(value: Option<bool>) -> String {
    match value {
        Some(true) => t!("view.storage.yes"),
        Some(false) => t!("view.storage.no"),
        None => t!("view.database_properties.unknown"),
    }
}

fn report(
    entries: Vec<TableFullTextEntry>,
    source: Option<&SourceOutcome>,
    read_complete: Option<bool>,
) -> TableFullTextReport {
    let mut limitations = vec![];
    if source.is_none() {
        limitations.push(t!("view.table_full_text.limit.source_unknown"));
    }
    if source.is_some_and(|source| source.state == SourceState::Partial) {
        limitations.push(t!("view.table_full_text.limit.partial"));
    }
    if read_complete == Some(false) {
        limitations.push(t!("view.table_full_text.limit.read"));
    }
    TableFullTextReport {
        title: t!("view.table_full_text.title"),
        summary: t!("view.table_full_text.summary", count = format::digits(entries.len().to_string())),
        source_state: source.map(|source| source.state),
        source_reason: source.and_then(|source| {
            source.reason.clone().or_else(|| {
                (source.state == SourceState::Unavailable).then(|| t!("view.table_full_text.source.unavailable"))
            })
        }),
        observed_from: None,
        observed_until: None,
        native_rows: None,
        observed_rows: entries.len() as u32,
        read_complete,
        entries,
        limitations,
    }
}
fn invalid() -> culprit_core::Error {
    CollectionStop::InvalidPage.error()
}

#[cfg(test)]
mod tests;
