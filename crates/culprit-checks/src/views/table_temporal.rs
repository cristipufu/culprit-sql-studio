//! The same original configuration interpreter serves retained table rows and saved scan details.
use super::table_catalog::MssqlCatalogEvidence;
use culprit_core::{
    Engine, Result, Snapshot, SourceOutcome, SourceState, TableStat, TableTemporalEntry, TableTemporalField,
    TableTemporalReport, format, key, source_id, t, telemetry::*,
};

pub fn explain_snapshot(snapshot: &Snapshot) -> Result<Option<TableTemporalReport>> {
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
pub fn explain_table(snapshot: &Snapshot, table: &TableStat) -> Result<Option<TableTemporalReport>> {
    let source = snapshot.sources.iter().find(|source| source.source == source_id::TABLES);
    let Some(entry) = qualified_table(snapshot.server.engine, source, table)? else { return Ok(None) };
    Ok(Some(report(vec![entry], source, None)))
}
fn qualified_table(
    engine: Engine,
    source: Option<&SourceOutcome>,
    table: &TableStat,
) -> Result<Option<TableTemporalEntry>> {
    let Some(configuration) = &table.mssql_temporal_configuration else { return Ok(None) };
    if engine != Engine::Mssql || !super::source_coverage::consistent_source(source, [source_id::TABLES]) {
        return Err(invalid());
    }
    entry(&table.schema, &table.name, None, configuration).map(Some)
}
pub fn explain(evidence: MssqlCatalogEvidence<'_>) -> Result<TableTemporalReport> {
    let context = evidence.validate(SnapshotDataset::Tables)?;
    let native = evidence.description.observation.snapshot().ok_or_else(invalid)?;
    let [coverage] = native.sources.as_slice() else { return Err(invalid()) };
    let entries = evidence
        .observations
        .iter()
        .map(|row| {
            let OperationalFact::MssqlTable(table) = &row.fact else { return Err(invalid()) };
            entry(&table.schema, &table.name, Some(row.key.clone()), &table.temporal_configuration)
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
    configuration: &MssqlTableTemporalConfiguration,
) -> Result<TableTemporalEntry> {
    Ok(TableTemporalEntry {
        schema: schema.into(),
        table: table.into(),
        observation_key,
        fields: configuration_fields(configuration)?,
    })
}

/// Format already supplied native catalog values. This grants no source, scope, owner or capture authority.
/// Both current-snapshot and authenticated-original entries call this same interpretation.
pub fn configuration_fields(configuration: &MssqlTableTemporalConfiguration) -> Result<Vec<TableTemporalField>> {
    if !configuration.is_valid() {
        return Err(invalid());
    }
    Ok(vec![
        field(
            key!("view.table_temporal.field.type"),
            configuration.temporal_type_availability,
            configuration.temporal_type.map(|value| format::digits(value.to_string())),
        ),
        field(
            key!("view.table_temporal.field.type_desc"),
            configuration.temporal_type_desc_availability,
            configuration.temporal_type_desc.clone(),
        ),
        field(
            key!("view.table_temporal.field.history_id"),
            configuration.history_table_id_availability,
            configuration.history_table_id.map(|value| format::digits(value.to_string())),
        ),
        field(
            key!("view.table_temporal.field.retention_period"),
            configuration.history_retention_period_availability,
            configuration.history_retention_period.map(|value| format::digits(value.to_string())),
        ),
        field(
            key!("view.table_temporal.field.retention_unit"),
            configuration.history_retention_period_unit_availability,
            configuration.history_retention_period_unit.map(|value| format::digits(value.to_string())),
        ),
        field(
            key!("view.table_temporal.field.retention_unit_desc"),
            configuration.history_retention_period_unit_desc_availability,
            configuration.history_retention_period_unit_desc.clone(),
        ),
    ])
}
fn field(key: &str, availability: MssqlCatalogAvailability, value: Option<String>) -> TableTemporalField {
    let text = match availability {
        MssqlCatalogAvailability::Available => value.unwrap_or_else(|| t!("view.table_temporal.native_null")),
        MssqlCatalogAvailability::Unsupported => t!("view.table_temporal.unsupported"),
        MssqlCatalogAvailability::PermissionDenied => t!("view.table_temporal.denied"),
        MssqlCatalogAvailability::Unknown => t!("view.table_temporal.unknown"),
    };
    TableTemporalField { key: key.into(), label: culprit_core::i18n::t(key, &[]), text, availability }
}
fn report(
    entries: Vec<TableTemporalEntry>,
    source: Option<&SourceOutcome>,
    read_complete: Option<bool>,
) -> TableTemporalReport {
    let mut limitations = vec![t!("view.table_temporal.limit.history")];
    if source.is_none() {
        limitations.push(t!("view.table_catalog.unknown_source"));
    }
    if source.is_some_and(|source| source.state == SourceState::Partial) {
        limitations.push(t!("view.table_catalog.partial"));
    }
    if read_complete == Some(false) {
        limitations.push(t!("view.table_temporal.limit.read"));
    }
    TableTemporalReport {
        title: t!("view.table_temporal.title"),
        summary: t!("view.table_temporal.summary", count = entries.len()),
        source_state: source.map(|source| source.state),
        source_reason: source.and_then(|source| source.reason.clone()),
        observed_from: None,
        observed_until: None,
        native_rows: None,
        observed_rows: entries.len() as u32,
        read_complete,
        qualification: t!("view.table_temporal.qualification"),
        entries,
        limitations,
    }
}
fn invalid() -> culprit_core::Error {
    CollectionStop::InvalidPage.error()
}
#[cfg(test)]
mod tests;
