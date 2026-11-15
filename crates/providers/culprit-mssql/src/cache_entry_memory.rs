//! Explicit statement selectors observe whole native cache entries through the caller's one bounded reader.
mod sql;
use crate::{
    cached_statements,
    read::{NativeRead, Parameter, ReadError, integer, invalid_data, optional_text, text},
    value::Rec,
};
use culprit_core::{Result, telemetry::*};
use std::collections::{BTreeMap, BTreeSet};

pub(crate) const MAX_QUERIES: u32 = 9;
const STATEMENT_TIMEOUT_MS: u32 = 1_000;
const TOTAL_TIMEOUT_MS: u32 = 5_000;
const MAX_SQL_BYTES: u32 = 16 * 1024;
const ATTRIBUTE_WITNESS: usize = MAX_CACHE_KEY_ATTRIBUTES + 3;

pub(crate) fn bounded_budget(requested: &CollectionBudget) -> CollectionBudget {
    CollectionBudget {
        page_rows: requested.page_rows.min(MAX_CACHE_ENTRY_STATEMENTS as u32),
        max_snapshot_rows: requested.max_snapshot_rows.min(MAX_CACHE_ENTRY_STATEMENTS as u32),
        max_queries: requested.max_queries.min(MAX_QUERIES),
        max_sql_bytes: requested.max_sql_bytes.min(MAX_SQL_BYTES),
        statement_timeout_ms: requested.statement_timeout_ms.min(STATEMENT_TIMEOUT_MS),
        total_timeout_ms: requested.total_timeout_ms.min(TOTAL_TIMEOUT_MS),
        ..requested.clone()
    }
}

pub(crate) struct Point {
    pub at: String,
    pub entries: Vec<MssqlCacheEntryMemory>,
}

pub(crate) fn validate(focus: &CacheEntryMemoryFocus, budget: &CollectionBudget) -> Result<()> {
    if !focus.is_valid() {
        return Err(invalid_data());
    }
    budget.validate()?;
    let handles = focus
        .statements
        .iter()
        .map(|statement| statement.identity.plan_handle.to_ascii_lowercase())
        .collect::<BTreeSet<_>>();
    if handles.len() > budget.max_snapshot_rows as usize {
        return Err(CollectionStop::RowLimit.error());
    }
    Ok(())
}

pub(crate) async fn read<R: NativeRead>(
    reader: &mut R,
    focus: &CacheEntryMemoryFocus,
    budget: &CollectionBudget,
) -> Result<Point> {
    validate(focus, budget)?;
    let rows = reader
        .query(
            &sql::selected(),
            vec![parameter(focus)?, Parameter::Integer(ATTRIBUTE_WITNESS as i64)],
            MAX_CACHE_ENTRY_STATEMENTS * (MAX_CACHE_KEY_ATTRIBUTES + 2) + 1,
        )
        .await
        .map_err(ReadError::into_error)?;
    decode(rows, focus, budget)
}

fn parameter(focus: &CacheEntryMemoryFocus) -> Result<Parameter> {
    if !focus.is_valid() {
        return Err(invalid_data());
    }
    let mut xml = String::from("<statements>");
    for statement in &focus.statements {
        let identity = &statement.identity;
        if statement.database_id > i32::MAX as u32 || identity.plan_generation.parse::<i64>().is_err() {
            return Err(invalid_data());
        }
        xml.push_str(&format!(
            "<statement plan=\"{}\" sql=\"{}\" start=\"{}\" end=\"{}\" created=\"{}\" generation=\"{}\" database=\"{}\"/>",
            identity.plan_handle,
            identity.sql_handle,
            identity.statement_start_bytes,
            identity.statement_end_bytes,
            identity.created_without_offset,
            identity.plan_generation,
            statement.database_id,
        ));
    }
    xml.push_str("</statements>");
    Ok(Parameter::Text(xml))
}

#[derive(Default)]
struct Entry {
    metadata: Option<MssqlCacheEntryMemory>,
    statements: Vec<CacheEntryStatement>,
    attributes: BTreeMap<String, CacheKeyAttribute>,
}

fn decode(rows: Vec<Rec>, focus: &CacheEntryMemoryFocus, budget: &CollectionBudget) -> Result<Point> {
    let mut entries = BTreeMap::<String, Entry>::new();
    let mut seen_statements = BTreeSet::new();
    let mut at = None;
    let mut clock_seen = false;
    for row in rows {
        let kind = text(&row, "row_kind")?;
        if kind == "clock" {
            if clock_seen {
                return Err(invalid_data());
            }
            clock_seen = true;
            point_clock(&row, &mut at)?;
            continue;
        }
        let plan = text(&row, "plan_handle")?.to_ascii_lowercase();
        if !focus.statements.iter().any(|statement| statement.identity.plan_handle.eq_ignore_ascii_case(&plan)) {
            return Err(invalid_data());
        }
        let entry = entries.entry(plan).or_default();
        match kind.as_str() {
            "statement" => {
                point_clock(&row, &mut at)?;
                let context = cached_statements::context(&row)?;
                let key = context.identity.key()?;
                if !seen_statements.insert(key.clone()) {
                    return Err(invalid_data());
                }
                let expected = focus
                    .statements
                    .iter()
                    .find(|expected| expected.identity.key().is_ok_and(|expected| expected == key))
                    .ok_or_else(|| CollectionStop::SourceChanged.error())?;
                if !matches_context(expected, &context) {
                    return Err(CollectionStop::SourceChanged.error());
                }
                let mut statement = if context.source_text_bytes.as_deref().is_some_and(|bytes| {
                    bytes.parse::<u64>().is_ok_and(|bytes| bytes > u64::from(budget.max_sql_bytes))
                }) {
                    DefinitionPayload::Unavailable { reason: PayloadUnavailable::Oversized }
                } else {
                    cached_statements::statement_from_prefix(&context, optional_text(&row, "text_prefix")?.as_deref())
                };
                if let DefinitionPayload::Available { text, .. } = &statement
                    && text.len() > budget.max_sql_bytes as usize
                {
                    statement = DefinitionPayload::Unavailable { reason: PayloadUnavailable::Oversized };
                }
                entry.statements.push(CacheEntryStatement { context, statement });
            }
            "entry" => {
                point_clock(&row, &mut at)?;
                if entry.metadata.is_some() {
                    return Err(invalid_data());
                }
                entry.metadata = Some(MssqlCacheEntryMemory {
                    observed_at: text(&row, "observed_at")?,
                    plan_handle: text(&row, "plan_handle")?,
                    sql_handle: text(&row, "sql_handle")?,
                    size_bytes: integer(&row, "size_in_bytes")?.to_string(),
                    cache_object_type: text(&row, "cacheobjtype")?,
                    object_type: text(&row, "objtype")?,
                    lookup_count: integer(&row, "usecounts")?.to_string(),
                    reference_count: integer(&row, "refcounts")?.to_string(),
                    cache_keys: vec![],
                    statements: vec![],
                });
            }
            "key" => {
                let attribute = CacheKeyAttribute {
                    name: text(&row, "attribute_name")?,
                    native_type: optional_text(&row, "native_type")?,
                    value: optional_text(&row, "attribute_value")?,
                };
                if !attribute.is_valid() || entry.attributes.insert(attribute.name.clone(), attribute).is_some() {
                    return Err(invalid_data());
                }
                if entry.attributes.len() > MAX_CACHE_KEY_ATTRIBUTES {
                    return Err(CollectionStop::RowLimit.error());
                }
            }
            _ => return Err(invalid_data()),
        }
    }
    if !clock_seen {
        return Err(invalid_data());
    }
    let at = at.ok_or_else(|| CollectionStop::SourceChanged.error())?;
    let mut observations = Vec::new();
    for entry in entries.into_values() {
        if entry.statements.is_empty() {
            continue;
        }
        let mut metadata = entry.metadata.ok_or_else(|| CollectionStop::SourceChanged.error())?;
        metadata.statements = entry.statements;
        metadata.statements.sort_by_key(|statement| statement.context.identity.statement_start_bytes);
        metadata.cache_keys = entry.attributes.into_values().collect();
        if !metadata.is_valid() {
            return Err(CollectionStop::SourceChanged.error());
        }
        if !focus.accepts_entry(&metadata) {
            return Err(CollectionStop::SourceChanged.error());
        }
        observations.push(metadata);
    }
    if observations.len() > budget.max_snapshot_rows as usize {
        return Err(CollectionStop::RowLimit.error());
    }
    Ok(Point { at, entries: observations })
}

fn matches_context(expected: &CachedStatementContext, current: &CachedStatementContext) -> bool {
    expected.database_id == current.database_id
        && expected.source_text_bytes.as_deref().is_none_or(|bytes| {
            current
                .source_text_bytes
                .as_deref()
                .is_some_and(|current| bytes.parse::<u64>().ok() == current.parse::<u64>().ok())
        })
        && expected
            .query_hash
            .as_ref()
            .is_none_or(|hash| current.query_hash.as_ref().is_some_and(|current| current.eq_ignore_ascii_case(hash)))
}

fn point_clock(row: &Rec, at: &mut Option<String>) -> Result<()> {
    let current = canonical_time(&text(row, "observed_at")?)?;
    if at.as_ref().is_some_and(|at| at != &current) {
        return Err(CollectionStop::SourceChanged.error());
    }
    *at = Some(current);
    Ok(())
}

pub(crate) fn same_bindings(before: &Point, after: &Point) -> bool {
    before.entries.len() == after.entries.len()
        && before.entries.iter().all(|before| {
            after.entries.iter().find(|after| before.plan_handle.eq_ignore_ascii_case(&after.plan_handle)).is_some_and(
                |after| {
                    before.sql_handle.eq_ignore_ascii_case(&after.sql_handle)
                        && before.cache_object_type == after.cache_object_type
                        && before.object_type == after.object_type
                        && !before.cache_keys.is_empty()
                        && before
                            .cache_keys
                            .iter()
                            .all(|attribute| attribute.native_type.is_some() && attribute.value.is_some())
                        && before.cache_keys == after.cache_keys
                        && before.statements.len() == after.statements.len()
                        && before.statements.iter().all(|statement| {
                            after
                                .statements
                                .iter()
                                .find(|other| {
                                    statement.context.identity.key().ok() == other.context.identity.key().ok()
                                })
                                .is_some_and(|other| {
                                    matches_context(&statement.context, &other.context)
                                        && matches_context(&other.context, &statement.context)
                                })
                        })
                },
            )
        })
}

#[cfg(test)]
pub(crate) mod tests;
