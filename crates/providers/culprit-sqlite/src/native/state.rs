//! Bounded native file recipes. No destination, scheduler or asynchronous worker ownership.
use super::*;
use crate::file::FileIdentity;
use culprit_core::{MeasurementScope, telemetry::*};
use culprit_read::SnapshotScope;
use rusqlite::Connection;
use std::{collections::BTreeMap, path::Path};
const FILE_SOURCE: &str = "sqlite-file-metadata";
const STAT_SOURCE: &str = "sqlite_stat1";
const SCHEMA_SOURCE: &str = "sqlite_schema";
use crate::native;
pub(crate) struct Captured {
    pub(crate) identity: FileIdentity,
    pub(crate) description: CollectionDescription,
    pub(crate) records: Vec<TelemetryRecord>,
    pub(crate) until: String,
    pub(crate) scope: SnapshotScope,
}
pub(crate) fn source_name(dataset: SnapshotDataset) -> &'static str {
    match dataset {
        SnapshotDataset::Settings | SnapshotDataset::Operational => FILE_SOURCE,
        SnapshotDataset::Statistics => STAT_SOURCE,
        SnapshotDataset::Sequences => "sqlite_sequence",
        _ => SCHEMA_SOURCE,
    }
}
fn clock(reader: &mut CatalogReader<'_>) -> Result<String> {
    let rows = reader.query("/* culprit */ SELECT strftime('%Y-%m-%dT%H:%M:%fZ','now')", [])?;
    rows.first()
        .and_then(|row| row.first())
        .and_then(Clone::clone)
        .filter(|time| canonical_time(time).is_ok())
        .ok_or_else(native::invalid)
}
pub(crate) fn capture(
    conn: &mut Connection,
    path: &Path,
    identity: FileIdentity,
    dataset: SnapshotDataset,
    budget: &CollectionBudget,
    accounting: crate::read::FileAccounting,
) -> Result<Captured> {
    identity.bound_to(conn, path)?;
    let tx = accounting.statement("BEGIN DEFERRED", &Default::default(), || {
        conn.transaction().map_err(|error| crate::map_error(&error))
    })?;
    let mut reader = CatalogReader::new(&tx, budget, accounting.clone());
    let result = (|| {
        let start = clock(&mut reader)?;
        let mut records = vec![];
        let applicable = match dataset {
            SnapshotDataset::Settings | SnapshotDataset::Operational => {
                settings(&mut reader, path, dataset, &mut records)?;
                true
            }
            SnapshotDataset::Statistics => statistics(&mut reader, &mut records)?,
            SnapshotDataset::Sequences => sequences(&mut reader, &mut records)?,
            _ => {
                schema(&mut reader, dataset, budget, &mut records)?;
                true
            }
        };
        let until = clock(&mut reader)?;
        identity.bound_to(&tx, path)?;
        Ok::<_, culprit_core::Error>((start, until, records, applicable))
    })();
    // Explicit rollback before file close and before the first destination-facing page.
    let rollback =
        accounting.cleanup("ROLLBACK", &Default::default(), || tx.rollback().map_err(|error| crate::map_error(&error)));
    let (start, until, records, applicable) = result?;
    rollback?;
    Ok(Captured {
        description: CollectionDescription {
            target: identity.database.clone().into(),
            source: source_name(dataset).into(),
            continuity: SourceContinuity::Unverified,
            observation: SourceObservation::PagedSnapshot(PagedSnapshotDescription {
                dataset,
                observed_from: start,
                sources: vec![source_name(dataset).into()],
            }),
        },
        identity,
        records,
        until,
        scope: if applicable {
            SnapshotScope::Available { source: source_name(dataset).into(), complete: true, error_code: None }
        } else {
            SnapshotScope::NotApplicable { source: source_name(dataset).into() }
        },
    })
}
fn settings(
    reader: &mut CatalogReader<'_>,
    path: &Path,
    dataset: SnapshotDataset,
    out: &mut Vec<TelemetryRecord>,
) -> Result<()> {
    let mut wal = false;
    for name in native::FILE_PRAGMAS {
        let rows = reader.query(&format!("/* culprit */ PRAGMA main.{name}"), [])?;
        let value = rows.first().and_then(|row| row.first()).and_then(Clone::clone).ok_or_else(native::invalid)?;
        if *name == "journal_mode" {
            wal = value.eq_ignore_ascii_case("wal");
        }
        if dataset == SnapshotDataset::Settings {
            out.push(TelemetryRecord::Setting(SettingObservation {
                source: FILE_SOURCE.into(),
                namespace: "main.file".into(),
                name: name.to_string(),
                scope: MeasurementScope::Database,
                nature: if matches!(*name, "page_count" | "freelist_count") {
                    SettingNature::Measurement
                } else {
                    SettingNature::Configuration
                },
                value: Some(value),
                value_in_use: None,
                value_for_secondary: None,
                unit: None,
                native_context: Some("file".into()),
                native_source: None,
                pending_restart: None,
                native_is_default: None,
                default_value: None,
                default_source: SettingDefaultSource::Unknown,
            }));
        } else if matches!(*name, "page_size" | "page_count" | "freelist_count") {
            let value = value.parse::<u64>().map_err(|_| native::invalid())?;
            out.push(counter(
                FILE_SOURCE,
                NativeIdentity::new(*name),
                vec![NativeCounterSample::gauge(
                    name,
                    value,
                    if *name == "page_size" { CounterUnit::Bytes } else { CounterUnit::Count },
                    MeasurementScope::Database,
                    FILE_SOURCE,
                    name,
                )],
            )?);
        }
    }
    if dataset == SnapshotDataset::Operational && wal {
        out.push(counter(
            FILE_SOURCE,
            NativeIdentity::new("wal-file"),
            vec![NativeCounterSample::gauge(
                "wal-file-bytes",
                native::wal_bytes(path)?,
                CounterUnit::Bytes,
                MeasurementScope::Database,
                "wal-file",
                "bytes",
            )],
        )?);
    }
    Ok(())
}
fn counter(source: &str, identity: NativeIdentity, counters: Vec<NativeCounterSample>) -> Result<TelemetryRecord> {
    let counters =
        NativeCounterSet { identity, scope: MeasurementScope::Database, counters, properties: BTreeMap::new() };
    Ok(TelemetryRecord::Operational(OperationalObservation {
        source: source.into(),
        key: counters.key()?,
        fact: OperationalFact::CounterSet(Box::new(counters)),
    }))
}
fn definition(
    kind: &str,
    id: &str,
    row: &NativeRow,
    fields: &[(usize, &str)],
    ddl: Option<&str>,
) -> NativeCatalogDefinition {
    NativeCatalogDefinition {
        identity: NativeIdentity::new(id).qualified("schema", "main"),
        object_type: kind.into(),
        properties: native::properties(row, fields),
        definition: ddl.map(|raw| CatalogExpression {
            native_sha256: Some(digest(raw.as_bytes())),
            payload: DefinitionPayload::Unavailable { reason: PayloadUnavailable::NotCollected },
        }),
        predicate: None,
    }
}
fn catalog_record(catalog: NativeCatalogDefinition) -> Result<TelemetryRecord> {
    Ok(TelemetryRecord::ObjectDefinition(ObjectDefinitionRecord::new(
        SCHEMA_SOURCE.into(),
        ObjectDefinitionFact::NativeCatalog(Box::new(catalog)),
    )?))
}
fn catalog_counters(catalog: NativeCatalogDefinition) -> Result<TelemetryRecord> {
    let counters = NativeCounterSet {
        identity: catalog.identity.clone(),
        scope: MeasurementScope::Database,
        counters: vec![],
        properties: BTreeMap::from([("object_type".into(), catalog.object_type.clone())]),
    };
    Ok(TelemetryRecord::Operational(OperationalObservation {
        source: SCHEMA_SOURCE.into(),
        key: catalog.key()?,
        fact: OperationalFact::CatalogCounters(Box::new(NativeCatalogCounters { catalog, counters })),
    }))
}
fn value(row: &NativeRow, index: usize) -> Result<&str> {
    row.get(index).and_then(Option::as_deref).ok_or_else(native::invalid)
}
fn schema(
    reader: &mut CatalogReader<'_>,
    dataset: SnapshotDataset,
    budget: &CollectionBudget,
    out: &mut Vec<TelemetryRecord>,
) -> Result<()> {
    let objects = reader.query(native::SCHEMA, [])?;
    for row in &objects {
        let kind = value(row, 0)?;
        let name = value(row, 1)?;
        let table = kind == "table";
        let selected = match dataset {
            SnapshotDataset::Tables => table,
            SnapshotDataset::Indexes => false,
            SnapshotDataset::Triggers => kind == "trigger",
            SnapshotDataset::Objects => true,
            _ => false,
        };
        if selected {
            // CHECK clauses stay in the hashed table definition; evaluating/deparsing SQL is never part of a poll.
            let catalog = definition(
                kind,
                name,
                row,
                &[(1, "name"), (3, "table_kind"), (4, "without_rowid"), (5, "table_name"), (6, "root_page")],
                row[2].as_deref(),
            );
            out.push(if dataset == SnapshotDataset::Tables {
                catalog_counters(catalog)?
            } else {
                catalog_record(catalog)?
            });
        }
        if table && dataset == SnapshotDataset::Indexes {
            for index in reader.query(native::INDEXES, [name])? {
                let index_name = value(&index, 0)?;
                let mut properties =
                    native::properties(&index, &[(0, "name"), (1, "unique"), (2, "origin"), (3, "partial")]);
                properties.insert("table_name".into(), name.into());
                let keys = reader.query(native::INDEX_KEYS, [index_name])?;
                properties.insert("keys".into(), serde_json::to_string(&keys).map_err(|_| native::invalid())?);
                if let Some(sql) = objects
                    .iter()
                    .find(|row| row[0].as_deref() == Some("index") && row[1].as_deref() == Some(index_name))
                    .and_then(|row| row[2].as_deref())
                {
                    properties.insert("definition_hash".into(), digest(sql.as_bytes()));
                }
                let catalog = NativeCatalogDefinition {
                    identity: NativeIdentity::new(index_name).qualified("schema", "main"),
                    object_type: "index".into(),
                    properties,
                    definition: None,
                    predicate: None,
                };
                out.push(catalog_counters(catalog)?);
            }
        }
        if table && matches!(dataset, SnapshotDataset::Columns | SnapshotDataset::ForeignKeys) {
            if row[3].as_deref() == Some("virtual") {
                return Err(CollectionStop::SourceUnavailable.error());
            }
            let (sql, fields, typ): (&str, &[(usize, &str)], &str) = if dataset == SnapshotDataset::Columns {
                (
                    native::COLUMNS,
                    &[
                        (0, "ordinal"),
                        (1, "name"),
                        (2, "declared_type"),
                        (3, "not_null"),
                        (5, "primary_key_order"),
                        (6, "hidden"),
                    ],
                    "column",
                )
            } else {
                (
                    native::FOREIGN_KEYS,
                    &[
                        (0, "id"),
                        (1, "sequence"),
                        (2, "parent_table"),
                        (3, "column"),
                        (4, "parent_column"),
                        (5, "on_delete"),
                        (6, "on_update"),
                        (7, "match"),
                    ],
                    "foreign-key-column",
                )
            };
            for item in reader.query(sql, [name])? {
                let id = serde_json::to_string(&(
                    name,
                    &item[0],
                    if dataset == SnapshotDataset::ForeignKeys { item.get(1) } else { None },
                ))
                .map_err(|_| native::invalid())?;
                out.push(catalog_record(definition(typ, &id, &item, fields, None))?);
            }
        }
        if out.len() > budget.max_snapshot_rows as usize {
            return Err(CollectionStop::RowLimit.error());
        }
    }
    Ok(())
}
fn exists(reader: &mut CatalogReader<'_>, name: &str) -> Result<bool> {
    Ok(!reader
        .query("/* culprit */ SELECT 1 FROM main.sqlite_schema WHERE type='table' AND name=?1", [name])?
        .is_empty())
}
fn statistics(reader: &mut CatalogReader<'_>, out: &mut Vec<TelemetryRecord>) -> Result<bool> {
    if !exists(reader, "sqlite_stat1")? {
        return Ok(false);
    }
    for row in reader.query(native::STATISTICS, [])? {
        let numbers = statistics_estimates(value(&row, 2)?)?;
        let id = serde_json::to_string(&(&row[0], &row[1])).map_err(|_| native::invalid())?;
        let counters = numbers
            .into_iter()
            .enumerate()
            .map(|(i, n)| {
                NativeCounterSample::gauge(
                    &format!("estimate-{i}"),
                    n,
                    CounterUnit::Count,
                    MeasurementScope::Database,
                    STAT_SOURCE,
                    "stat",
                )
            })
            .collect();
        out.push(counter(STAT_SOURCE, NativeIdentity::new(&id), counters)?);
    }
    Ok(true)
}
fn statistics_estimates(stat: &str) -> Result<Vec<u64>> {
    let mut numbers = vec![];
    let mut flags = false;
    for field in stat.split_whitespace() {
        if let Ok(value) = field.parse::<u64>() {
            if flags || numbers.len() >= 128 {
                return Err(native::invalid());
            }
            numbers.push(value);
        } else if matches!(field, "unordered" | "noskipscan")
            || field.strip_prefix("sz=").is_some_and(|value| value.parse::<u64>().is_ok())
        {
            flags = true;
        } else {
            return Err(native::invalid());
        }
    }
    if numbers.is_empty() {
        return Err(native::invalid());
    }
    Ok(numbers)
}

fn sequences(reader: &mut CatalogReader<'_>, out: &mut Vec<TelemetryRecord>) -> Result<bool> {
    if !exists(reader, "sqlite_sequence")? {
        return Ok(false);
    }
    for row in reader.query("/* culprit */ SELECT name,seq FROM main.sqlite_sequence", [])? {
        let name = value(&row, 0)?;
        let last = value(&row, 1)?.parse::<u64>().map_err(|_| native::invalid())?;
        out.push(counter(
            "sqlite_sequence",
            NativeIdentity::new(name),
            vec![NativeCounterSample::gauge(
                "last-rowid",
                last,
                CounterUnit::Count,
                MeasurementScope::Database,
                "sqlite_sequence",
                "seq",
            )],
        )?);
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn statistics_preserve_exact_estimates_and_reject_malformed_native_data() {
        assert_eq!(
            statistics_estimates("18446744073709551615 12 1 unordered noskipscan sz=30").unwrap(),
            vec![u64::MAX, 12, 1]
        );
        for invalid in ["", "12 -1", "12 18446744073709551616", "12 private-value", "12 unordered 4", "12 sz=no"] {
            assert!(statistics_estimates(invalid).is_err(), "{invalid}");
        }
    }
}
