//! Allocations in PostgreSQL's main shared-memory segment (13+), not private backend or dynamic shared memory.
//! The native name/offset tuple is namespaced by the observed postmaster epoch; it is not a host or query owner.
//! Anonymous and free allocations retain their native NULLs rather than acquiring guessed component names.
//! Access is restricted by default to superusers/pg_read_all_stats; permission failures remain explicit.
//! https://www.postgresql.org/docs/current/view-pg-shmem-allocations.html
use super::*;
use culprit_core::{Error, ErrorCode, MeasurementScope, telemetry::*};
use std::collections::{BTreeMap, BTreeSet};

pub(crate) const SOURCE_NAME: &str = culprit_core::source_id::SHARED_MEMORY_ALLOCATIONS;
const NATIVE_OBJECT: &str = "pg_catalog.pg_shmem_allocations";
const MIN_VERSION: u32 = 130_000;
const MAX_ALLOCATIONS: usize = 4096;
const NAME_COLUMN: &str = "name";
const OFFSET_COLUMN: &str = "off";
const FIELDS: &[&str] = &["size", "allocated_size"];

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Access {
    Available,
    Restricted,
    Unsupported,
}

pub(crate) fn supported(version: u32) -> bool {
    version >= MIN_VERSION
}

pub(crate) async fn access<R: QueryRead>(reader: &mut R, version: u32) -> Result<Access> {
    if !supported(version) {
        return Ok(Access::Unsupported);
    }
    let row=one(reader,&format!("SELECT
        ((SELECT rolsuper FROM pg_catalog.pg_roles WHERE rolname=current_user)
         OR pg_catalog.pg_has_role(current_user,'pg_read_all_stats','USAGE'))
        AND pg_catalog.has_table_privilege(current_user,'{NATIVE_OBJECT}','SELECT')
        AND pg_catalog.has_function_privilege(current_user,'pg_catalog.pg_get_shmem_allocations()','EXECUTE') AS can_read")).await?;
    Ok(if flag(&row, "can_read")? { Access::Available } else { Access::Restricted })
}

pub(crate) fn permission_error() -> Error {
    Error::new(
        ErrorCode::PermissionDenied,
        culprit_core::t!("provider.postgres.shared_memory_allocations.permission_required"),
    )
}

/// One native projection serves the independent snapshot read and the InstanceOperational UNION.
pub(crate) fn branch_sql() -> String {
    let fields = FIELDS.iter().map(|column| format!("'{column}',m.{column}::text")).collect::<Vec<_>>().join(",");
    format!("SELECT '{SOURCE_NAME}'::text AS native_source,
        pg_catalog.jsonb_build_object('{NAME_COLUMN}',m.{NAME_COLUMN},'{OFFSET_COLUMN}',m.{OFFSET_COLUMN}::text,{fields})::text AS native_values
        FROM {NATIVE_OBJECT} m")
}
fn query(limit: usize) -> String {
    format!("SELECT * FROM ({}) native_memory LIMIT {limit}", branch_sql())
}

/// Reuse the caller's owned native session and shutdown policy. No new connection or instrumentation is created.
pub(crate) async fn read<R: QueryRead>(
    reader: &mut R,
    version: u32,
    started_at: &str,
    limit: usize,
) -> Result<Vec<NativeCounterSet>> {
    if !supported(version) {
        return Err(Error::new(ErrorCode::Unsupported, culprit_core::t!("collection.unsupported")));
    }
    let started_at = canonical_time(started_at)?;
    let limit = limit.min(MAX_ALLOCATIONS);
    if limit == 0 {
        return Err(CollectionStop::RowLimit.error());
    }
    let rows = reader.query(&query(limit + 1), limit + 1).await?;
    if rows.len() > limit {
        return Err(CollectionStop::RowLimit.error());
    }
    if rows.is_empty() {
        return Err(CollectionStop::SourceUnavailable.error());
    }
    let mut seen = BTreeSet::new();
    rows.iter()
        .map(|row| {
            let sample = decode_values(&text(row, "native_values")?, &started_at)?;
            if !seen.insert(sample.identity.clone()) {
                return Err(invalid());
            }
            Ok(sample)
        })
        .collect()
}

pub(crate) fn decode_values(values: &str, started_at: &str) -> Result<NativeCounterSet> {
    let values: serde_json::Value = serde_json::from_str(values).map_err(|_| invalid())?;
    let values = values.as_object().ok_or_else(invalid)?;
    let started_at = canonical_time(started_at)?;
    let name = optional(values, NAME_COLUMN)?;
    let offset = scalar(values, OFFSET_COLUMN)?;
    let tuple = serde_json::to_vec(&(&name, offset)).map_err(|_| invalid())?;
    let identity = NativeIdentity::new(digest(&tuple))
        .qualified("native_object", NATIVE_OBJECT)
        .qualified("postmaster_started_at", started_at);
    let mut properties = BTreeMap::new();
    if let Some(name) = name {
        properties.insert(NAME_COLUMN.into(), name);
    }
    if let Some(offset) = offset {
        properties.insert(OFFSET_COLUMN.into(), offset.to_string());
    }
    let counters = FIELDS
        .iter()
        .filter_map(|column| match scalar(values, column) {
            Ok(Some(value)) => Some(Ok(NativeCounterSample::gauge(
                column,
                value,
                CounterUnit::Bytes,
                MeasurementScope::Server,
                NATIVE_OBJECT,
                column,
            ))),
            Ok(None) => None,
            Err(error) => Some(Err(error)),
        })
        .collect::<Result<Vec<_>>>()?;
    let sample = NativeCounterSet { identity, scope: MeasurementScope::Server, counters, properties };
    if !sample.is_valid() {
        return Err(invalid());
    }
    Ok(sample)
}

fn optional(values: &serde_json::Map<String, serde_json::Value>, column: &str) -> Result<Option<String>> {
    match values.get(column) {
        Some(serde_json::Value::Null) => Ok(None),
        Some(serde_json::Value::String(value)) => Ok(Some(value.clone())),
        _ => Err(invalid()),
    }
}
fn scalar(values: &serde_json::Map<String, serde_json::Value>, column: &str) -> Result<Option<u64>> {
    optional(values, column)?.map(|value| value.parse::<u64>().map_err(|_| invalid())).transpose()
}

#[cfg(test)]
mod tests;
