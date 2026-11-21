//! One pure interpretation of persisted catalog values. Each native partition retains its own state and owner.
use super::table_catalog::MssqlCatalogEvidence;
use culprit_core::{
    Engine, IndexConfigurationEntry, IndexConfigurationField, IndexConfigurationReport, PartitionConfigurationEntry,
    Result, Snapshot, SourceOutcome, SourceState, format, key, source_id, t, telemetry::*,
};
const MAX_ENTRIES: usize = 64;
const MAX_PARTITIONS: usize = 16;

pub fn explain_snapshot(snapshot: &Snapshot, dataset: SnapshotDataset) -> Result<Option<IndexConfigurationReport>> {
    let expected_source = source_name(dataset)?;
    let source = snapshot.sources.iter().find(|source| source.source == expected_source);
    let mut configurations = Vec::new();
    match dataset {
        SnapshotDataset::Indexes => {
            for index in &snapshot.indexes {
                if let Some(configuration) = &index.mssql_configuration {
                    if !configuration.is_for_index(index) {
                        return Err(invalid());
                    }
                    configurations.push((configuration.as_ref(), None));
                }
            }
        }
        SnapshotDataset::Tables => {
            for table in &snapshot.tables {
                if let Some(configuration) = &table.mssql_heap_configuration {
                    if !configuration.is_for_table(table) {
                        return Err(invalid());
                    }
                    configurations.push((configuration.as_ref(), None));
                }
            }
        }
        _ => unreachable!(),
    }
    if configurations.is_empty() {
        return Ok(None);
    }
    if snapshot.server.engine != Engine::Mssql
        || !super::source_coverage::consistent_source(source, configurations.iter().map(|_| expected_source))
    {
        return Err(invalid());
    }
    let mut report = render(dataset, &configurations, source, None)?;
    report.observed_rows = configurations.len() as u32;
    Ok(Some(report))
}

pub fn explain_index(snapshot: &Snapshot, index: &culprit_core::IndexStat) -> Result<Option<IndexConfigurationReport>> {
    let Some(configuration) = index.mssql_configuration.as_deref() else { return Ok(None) };
    if !configuration.is_for_index(index) {
        return Err(invalid());
    }
    selected(snapshot, SnapshotDataset::Indexes, configuration).map(Some)
}
pub fn explain_heap(snapshot: &Snapshot, table: &culprit_core::TableStat) -> Result<Option<IndexConfigurationReport>> {
    let Some(configuration) = table.mssql_heap_configuration.as_deref() else { return Ok(None) };
    if !configuration.is_for_table(table) {
        return Err(invalid());
    }
    selected(snapshot, SnapshotDataset::Tables, configuration).map(Some)
}
fn selected(
    snapshot: &Snapshot,
    dataset: SnapshotDataset,
    configuration: &MssqlIndexConfiguration,
) -> Result<IndexConfigurationReport> {
    let source_id = source_name(dataset)?;
    let source = snapshot.sources.iter().find(|source| source.source == source_id);
    if snapshot.server.engine != Engine::Mssql || !super::source_coverage::consistent_source(source, [source_id]) {
        return Err(invalid());
    }
    render(dataset, &[(configuration, None)], source, None)
}

pub fn explain(evidence: MssqlCatalogEvidence<'_>) -> Result<IndexConfigurationReport> {
    let native = evidence.description.observation.snapshot().ok_or_else(invalid)?;
    let dataset = native.dataset;
    let context = evidence.validate(dataset)?;
    let [coverage] = native.sources.as_slice() else { return Err(invalid()) };
    let mut configurations = Vec::new();
    let mut missing = 0;
    for row in evidence.observations {
        let configuration = match &row.fact {
            OperationalFact::MssqlIndex(index) => index.configuration.as_deref(),
            OperationalFact::MssqlTable(table) if table.is_heap != Some(false) => table.heap_configuration.as_deref(),
            OperationalFact::MssqlTable(_) => continue,
            _ => return Err(invalid()),
        };
        if let Some(configuration) = configuration {
            configurations.push((configuration, Some(row.key.as_str())));
        } else {
            missing += 1;
        }
    }
    let mut source = evidence.source.clone();
    if missing > 0 {
        source.state = if configurations.is_empty() { SourceState::Unavailable } else { SourceState::Partial };
        source.reason = Some(t!("view.index_configuration.not_retained"));
    }
    let mut report = render(dataset, &configurations, Some(&source), Some(evidence.read_complete))?;
    report.context = Some(context);
    report.native_rows = Some(coverage.records);
    report.observed_rows = evidence.observations.len() as u32;
    Ok(report)
}
pub fn explain_captured(read: &CapturedOperationalRead) -> Result<IndexConfigurationReport> {
    if !read.is_valid() {
        return Err(invalid());
    }
    let native = read.description.observation.snapshot().ok_or_else(invalid)?;
    let [coverage] = native.sources.as_slice() else { return Err(invalid()) };
    let missing = !read.read_complete || read.observations.len() != coverage.records as usize;
    let source = SourceOutcome {
        source: coverage.source.clone(),
        state: if missing { SourceState::Partial } else { coverage.state },
        reason: if missing {
            Some(t!("view.index_configuration.partial"))
        } else {
            coverage.stopped.map(|stop| stop.error().message).or_else(|| {
                coverage.error_code.map(|code| {
                    t!(
                        "view.index_configuration.source_error",
                        reason = match code {
                            culprit_core::ErrorCode::PermissionDenied => "permission",
                            culprit_core::ErrorCode::Unsupported => "unsupported",
                            _ => "read",
                        }
                    )
                })
            })
        },
        reason_message: None,
    };
    explain(MssqlCatalogEvidence {
        description: &read.description,
        observations: &read.observations.iter().collect::<Vec<_>>(),
        source: &source,
        read_complete: read.read_complete,
    })
}
pub fn explain_recorded(original: &RecordedOperationalRead) -> Result<IndexConfigurationReport> {
    match original {
        RecordedOperationalRead::Captured { read } => explain_captured(read),
        RecordedOperationalRead::Unavailable { reason } => Ok(unavailable(reason)),
    }
}
pub fn unavailable(reason: &culprit_core::i18n::Message) -> IndexConfigurationReport {
    IndexConfigurationReport {
        dataset: None,
        context: None,
        title: t!("view.index_configuration.title"),
        summary: reason.render(),
        qualification: t!("view.index_configuration.qualification"),
        source_state: Some(SourceState::Unavailable),
        source_reason: Some(reason.render()),
        read_complete: Some(false),
        native_rows: None,
        observed_rows: 0,
        presented_rows: 0,
        entries_limited: false,
        entries: vec![],
        limitations: vec![t!("view.index_configuration.limit.original")],
    }
}
fn render(
    dataset: SnapshotDataset,
    configurations: &[(&MssqlIndexConfiguration, Option<&str>)],
    source: Option<&SourceOutcome>,
    read_complete: Option<bool>,
) -> Result<IndexConfigurationReport> {
    let mut entries = Vec::new();
    for (configuration, reference) in configurations.iter().take(MAX_ENTRIES) {
        entries.push(entry(configuration, *reference)?);
    }
    let mut limitations = vec![
        t!("view.index_configuration.limit.identity"),
        t!("view.index_configuration.limit.time"),
        t!("view.index_configuration.limit.scope"),
    ];
    if source.is_none() {
        limitations.push(t!("view.table_catalog.unknown_source"));
    }
    if source.is_some_and(|source| source.state == SourceState::Partial) || read_complete == Some(false) {
        limitations.push(t!("view.index_configuration.partial"));
    }
    if entries.len() < configurations.len() {
        limitations.push(t!(
            "view.index_configuration.presentation_limited",
            shown = entries.len(),
            total = configurations.len()
        ));
    }
    if read_complete.is_none() {
        limitations.push(t!("view.index_configuration.limit.original"));
    }
    Ok(IndexConfigurationReport {
        dataset: Some(dataset),
        context: None,
        title: t!("view.index_configuration.title"),
        summary: if source
            .is_some_and(|source| matches!(source.state, SourceState::Unavailable | SourceState::NotApplicable))
        {
            t!("view.index_configuration.unavailable")
        } else {
            t!("view.index_configuration.summary", count = configurations.len())
        },
        qualification: t!("view.index_configuration.qualification"),
        source_state: source.map(|source| source.state),
        source_reason: source.and_then(|source| source.reason.clone()),
        read_complete,
        native_rows: None,
        observed_rows: configurations.len() as u32,
        presented_rows: entries.len() as u32,
        entries_limited: entries.len() < configurations.len(),
        entries,
        limitations,
    })
}
fn entry(configuration: &MssqlIndexConfiguration, reference: Option<&str>) -> Result<IndexConfigurationEntry> {
    if !configuration.is_valid() {
        return Err(invalid());
    }
    let mut fields = Vec::new();
    if let Some(options) = &configuration.options {
        fields.push(field(
            key!("view.index_configuration.field.fill_factor"),
            options.fill_factor_availability,
            options.fill_factor.map(|value| {
                if value == 0 {
                    t!("view.index_configuration.native_default")
                } else {
                    format::digits(value.to_string())
                }
            }),
        ));
        for (key, state, value) in [
            (key!("view.index_configuration.field.padded"), options.padded_availability, options.padded),
            (
                key!("view.index_configuration.field.ignore_duplicate_key"),
                options.ignore_duplicate_key_availability,
                options.ignore_duplicate_key,
            ),
            (
                key!("view.index_configuration.field.allow_row_locks"),
                options.allow_row_locks_availability,
                options.allow_row_locks,
            ),
            (
                key!("view.index_configuration.field.allow_page_locks"),
                options.allow_page_locks_availability,
                options.allow_page_locks,
            ),
        ] {
            fields.push(field(key, state, value.map(bit)));
        }
    }
    let mut limitations = vec![t!("view.index_configuration.limit.configuration")];
    if matches!(configuration.type_code, 5 | 6) {
        limitations.push(t!("view.index_configuration.limit.columnstore"));
    }
    if configuration.partitions.is_empty() {
        limitations.push(t!("view.index_configuration.limit.no_partitions"));
    }
    if !configuration.partitions_complete {
        limitations.push(t!("view.index_configuration.limit.partitions"));
    }
    if configuration.partitions.len() > MAX_PARTITIONS {
        limitations.push(t!(
            "view.index_configuration.partition_presentation_limited",
            shown = MAX_PARTITIONS,
            total = configuration.partitions.len()
        ));
    }
    Ok(IndexConfigurationEntry {
        observation_key: reference.map(str::to_owned),
        native_table: configuration.table.clone(),
        schema: configuration.schema.clone(),
        table: configuration.table_name.clone(),
        table_modified_without_offset: configuration.table_modified_without_offset.clone(),
        index_id: configuration.index_id,
        index_name: configuration.name.clone(),
        type_code: configuration.type_code,
        fields,
        partition_identity_availability: configuration.partition_identity_availability,
        partitions_complete: configuration.partitions_complete,
        retained_partitions: configuration.partitions.len() as u32,
        presented_partitions: configuration.partitions.len().min(MAX_PARTITIONS) as u32,
        partitions_limited: configuration.partitions.len() > MAX_PARTITIONS,
        partitions: configuration
            .partitions
            .iter()
            .take(MAX_PARTITIONS)
            .map(|partition| PartitionConfigurationEntry {
                partition_id: partition.partition_id.clone(),
                partition_number: partition.partition_number,
                hobt_id: partition.hobt_id.clone(),
                fields: vec![
                    field(
                        key!("view.index_configuration.field.data_compression"),
                        configuration.data_compression_availability,
                        partition.data_compression.map(|value| {
                            t!(
                                "view.index_configuration.compression",
                                state = match value {
                                    0 => "none",
                                    1 => "row",
                                    2 => "page",
                                    3 => "columnstore",
                                    4 => "archive",
                                    _ => "unknown",
                                },
                                code = format::digits(value.to_string())
                            )
                        }),
                    ),
                    field(
                        key!("view.index_configuration.field.data_compression_description"),
                        configuration.data_compression_description_availability,
                        partition.data_compression_description.clone(),
                    ),
                    field(
                        key!("view.index_configuration.field.xml_compression"),
                        configuration.xml_compression_availability,
                        partition.xml_compression.map(bit),
                    ),
                    field(
                        key!("view.index_configuration.field.xml_compression_description"),
                        configuration.xml_compression_description_availability,
                        partition.xml_compression_description.clone(),
                    ),
                ],
            })
            .collect(),
        limitations,
    })
}
fn bit(value: bool) -> String {
    t!("view.index_configuration.native_bit", value = if value { "true" } else { "false" })
}
fn field(key: &str, availability: MssqlCatalogAvailability, value: Option<String>) -> IndexConfigurationField {
    let text = match availability {
        MssqlCatalogAvailability::Available => value.unwrap_or_else(|| t!("view.index_configuration.native_null")),
        _ => t!(
            "view.index_configuration.availability",
            state = match availability {
                MssqlCatalogAvailability::Unsupported => "unsupported",
                MssqlCatalogAvailability::PermissionDenied => "denied",
                _ => "unknown",
            }
        ),
    };
    IndexConfigurationField { key: key.into(), label: culprit_core::i18n::t(key, &[]), text, availability }
}
fn source_name(dataset: SnapshotDataset) -> Result<&'static str> {
    match dataset {
        SnapshotDataset::Tables => Ok(source_id::TABLES),
        SnapshotDataset::Indexes => Ok(source_id::INDEXES),
        _ => Err(invalid()),
    }
}
fn invalid() -> culprit_core::Error {
    CollectionStop::InvalidPage.error()
}

#[cfg(test)]
mod tests;
