//! Current native health counters. Shared server views stay explicitly server-scoped.
use super::metadata::Metadata;
use super::*;
use culprit_core::{MeasurementScope, ResetMarker, telemetry::*};
use serde_json::Value;
use std::collections::BTreeMap;

pub(crate) fn sources(meta: &Metadata) -> Vec<&'static str> {
    let mut sources =
        vec!["pg_stat_database", "pg_stat_archiver", "pg_stat_bgwriter", "pg_database", "pg_stat_database_conflicts"];
    if meta.statements.as_ref().is_some_and(|catalog| catalog.info) {
        sources.push("pg_stat_statements_info");
    }
    if meta.version >= 140_000 {
        sources.push("pg_stat_wal");
    }
    if super::memory::supported(meta.version) {
        sources.push(super::memory::SOURCE_NAME);
    }
    if meta.version >= 150_000 {
        sources.push("pg_stat_subscription_stats");
    }
    if meta.version >= 160_000 {
        sources.push("pg_stat_io");
    }
    if meta.version >= 170_000 {
        sources.push("pg_stat_checkpointer");
    }
    sources
}

#[derive(Clone, Copy)]
pub(crate) enum Selection {
    Database,
    Instance,
}
impl Selection {
    fn includes(self, source: &str) -> bool {
        match self {
            Self::Database => source_scope(source) == MeasurementScope::Database,
            Self::Instance => source_scope(source) == MeasurementScope::Server,
        }
    }
}
pub(crate) fn selected_sources(meta: &Metadata, selection: Selection) -> Vec<&'static str> {
    sources(meta).into_iter().filter(|source| selection.includes(source)).collect()
}
fn source_scope(source: &str) -> MeasurementScope {
    if matches!(
        source,
        "pg_stat_database" | "pg_stat_database_conflicts" | "pg_database" | "pg_stat_subscription_stats"
    ) {
        MeasurementScope::Database
    } else {
        MeasurementScope::Server
    }
}
pub(crate) fn selected_query(
    meta: &Metadata,
    limit: usize,
    selection: Selection,
    memory_access: super::memory::Access,
) -> String {
    let branches=selected_sources(meta, selection).iter().filter(|name| **name != super::memory::SOURCE_NAME || memory_access==super::memory::Access::Available).map(|name| {
        if *name==super::memory::SOURCE_NAME { return super::memory::branch_sql(); }
        let values=if *name=="pg_database" {
            "pg_catalog.jsonb_build_object('datname',s.datname,'xid_age',pg_catalog.age(s.datfrozenxid),'mxid_age',pg_catalog.mxid_age(s.datminmxid))".to_owned()
        } else {"pg_catalog.to_jsonb(s)".to_owned()};
        let filter=match *name {
            "pg_stat_database"|"pg_stat_database_conflicts"=>format!(" WHERE s.datid={}",meta.database_oid),
            "pg_database"=>format!(" WHERE s.oid={}",meta.database_oid),
            "pg_stat_subscription_stats"=>format!(" WHERE s.subid IN (SELECT oid FROM pg_catalog.pg_subscription WHERE subdbid={})",meta.database_oid),
            _=>String::new(),
        };
        let relation=if *name=="pg_stat_statements_info" {format!("{}.pg_stat_statements_info",crate::rows::quote_ident(&meta.statements.as_ref().expect("declared statistics source").schema))}else{format!("pg_catalog.{name}")};
        format!("SELECT '{name}'::text AS native_source, ({values})::text AS native_values FROM {relation} s{filter}")
    }).collect::<Vec<_>>();
    format!("SELECT * FROM ({}) native_health LIMIT {limit}", branches.join(" UNION ALL "))
}
pub(crate) fn decode(row: &Row, meta: &Metadata, selection: Selection) -> Result<TelemetryRecord> {
    let source = text(row, "native_source")?;
    if !sources(meta).contains(&source.as_str()) || !selection.includes(&source) {
        return Err(invalid());
    }
    if source == super::memory::SOURCE_NAME {
        let counters = super::memory::decode_values(&text(row, "native_values")?, &meta.started_at)?;
        return OperationalObservation::from_counters(&source, counters).map(TelemetryRecord::Operational);
    }
    let values: Value = serde_json::from_str(&text(row, "native_values")?).map_err(|_| invalid())?;
    let fields = values.as_object().ok_or_else(invalid)?;
    let scope = source_scope(&source);
    let reset = values
        .get("stats_reset")
        .and_then(Value::as_str)
        .map(|value| canonical_time(value).map(ResetMarker::At))
        .transpose()?
        .unwrap_or(ResetMarker::Unknown);
    let mut identity = NativeIdentity::new(&source).qualified("postmaster_started_at", &meta.started_at);
    let mut properties = BTreeMap::new();
    for name in [
        "datname",
        "backend_type",
        "object",
        "context",
        "subname",
        "last_archived_wal",
        "last_failed_wal",
        "last_archived_time",
        "last_failed_time",
        "checksum_last_failure",
    ] {
        if let Some(value) = values.get(name).and_then(Value::as_str) {
            properties.insert(name.into(), value.into());
            if matches!(name, "backend_type" | "object" | "context") {
                identity = identity.qualified(name, value);
            }
        }
    }
    if scope == MeasurementScope::Database {
        identity = identity.qualified("dbid", &meta.database_oid);
    }
    if let Some(id) = values.get("subid").and_then(Value::as_u64) {
        identity = identity.qualified("subid", id.to_string());
    }
    let mut counters = vec![];
    for (name, value) in fields {
        let Some((mut unit, nature)) = spec(&source, name) else { continue };
        if matches!(
            name.as_str(),
            "blks_read"
                | "blks_hit"
                | "buffers_checkpoint"
                | "buffers_clean"
                | "buffers_backend"
                | "buffers_alloc"
                | "buffers_written"
        ) {
            unit = CounterUnit::Pages { bytes_per_page: meta.block_bytes };
        }
        if value.is_null() {
            continue;
        }
        let value = if let Some(value) = value.as_u64() {
            MeasurementValue::Integer(value.to_string())
        } else if let Some(value) = value.as_str() {
            MeasurementValue::Integer(value.parse::<u64>().map_err(|_| invalid())?.to_string())
        } else {
            MeasurementValue::Real(
                value.as_f64().filter(|value| value.is_finite() && *value >= 0.0).ok_or_else(invalid)?,
            )
        };
        counters.push(NativeCounterSample {
            key: name.clone(),
            value,
            unit,
            nature,
            scope,
            reset: if nature == CounterNature::Cumulative { reset.clone() } else { ResetMarker::Unknown },
            native_object: source.clone(),
            native_name: name.clone(),
            native_instance: None,
            native_type: None,
        });
    }
    let set = NativeCounterSet { identity, scope, counters, properties };
    let observation =
        OperationalObservation { source, key: set.key()?, fact: OperationalFact::CounterSet(Box::new(set)) };
    if !observation.is_valid() {
        return Err(invalid());
    }
    Ok(TelemetryRecord::Operational(observation))
}

#[cfg(test)]
mod tests;
fn spec(source: &str, name: &str) -> Option<(CounterUnit, CounterNature)> {
    use CounterNature::{Cumulative, Gauge};
    use CounterUnit::{Bytes, Count, Milliseconds};
    if matches!(name, "numbackends" | "op_bytes" | "xid_age" | "mxid_age") {
        return Some((if name == "op_bytes" { Bytes } else { Count }, Gauge));
    }
    if matches!(name, "temp_bytes" | "wal_bytes" | "read_bytes" | "write_bytes" | "extend_bytes") {
        return Some((Bytes, Cumulative));
    }
    if matches!(
        name,
        "blk_read_time"
            | "blk_write_time"
            | "session_time"
            | "active_time"
            | "idle_in_transaction_time"
            | "wal_write_time"
            | "wal_sync_time"
            | "read_time"
            | "write_time"
            | "writeback_time"
            | "extend_time"
            | "fsync_time"
            | "sync_time"
            | "checkpoint_write_time"
            | "checkpoint_sync_time"
    ) {
        return Some((Milliseconds, Cumulative));
    }
    let count = match source {
        "pg_database" => false,
        "pg_stat_statements_info" => name == "dealloc",
        "pg_stat_database_conflicts" => matches!(
            name,
            "confl_tablespace"
                | "confl_lock"
                | "confl_snapshot"
                | "confl_bufferpin"
                | "confl_deadlock"
                | "confl_active_logicalslot"
        ),
        "pg_stat_subscription_stats" => matches!(name, "apply_error_count" | "sync_error_count"),
        "pg_stat_database" => matches!(
            name,
            "xact_commit"
                | "xact_rollback"
                | "blks_read"
                | "blks_hit"
                | "tup_returned"
                | "tup_fetched"
                | "tup_inserted"
                | "tup_updated"
                | "tup_deleted"
                | "conflicts"
                | "temp_files"
                | "deadlocks"
                | "checksum_failures"
                | "sessions"
                | "sessions_abandoned"
                | "sessions_fatal"
                | "sessions_killed"
                | "parallel_workers_to_launch"
                | "parallel_workers_launched"
        ),
        "pg_stat_archiver" => matches!(name, "archived_count" | "failed_count"),
        "pg_stat_wal" => matches!(name, "wal_records" | "wal_fpi" | "wal_buffers_full" | "wal_write" | "wal_sync"),
        "pg_stat_bgwriter" => matches!(
            name,
            "checkpoints_timed"
                | "checkpoints_req"
                | "buffers_checkpoint"
                | "buffers_clean"
                | "maxwritten_clean"
                | "buffers_backend"
                | "buffers_backend_fsync"
                | "buffers_alloc"
        ),
        "pg_stat_checkpointer" => matches!(
            name,
            "num_timed"
                | "num_requested"
                | "num_done"
                | "restartpoints_timed"
                | "restartpoints_req"
                | "restartpoints_done"
                | "buffers_written"
        ),
        "pg_stat_io" => {
            matches!(name, "reads" | "writes" | "writebacks" | "extends" | "hits" | "evictions" | "reuses" | "fsyncs")
        }
        _ => false,
    };
    count.then_some((Count, Cumulative))
}
