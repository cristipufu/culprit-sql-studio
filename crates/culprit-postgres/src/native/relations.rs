//! Native table observations shared by scans and bounded background capture.
use super::*;
use culprit_core::{MeasurementScope, ResetMarker, TableStat, telemetry::*};
use serde_json::Value;
use std::collections::BTreeMap;

#[derive(Clone, Copy)]
pub(crate) enum SizeRead {
    Physical,
    CatalogEstimate,
}
pub(crate) fn table_query(version_num: u32, sizes: SizeRead, limit: Option<usize>) -> String {
    let exact = "pg_total_relation_size(s.relid) AS total_bytes, pg_table_size(s.relid) AS data_bytes, pg_indexes_size(s.relid) AS index_bytes,
                 pg_relation_size(s.relid) AS in_row_bytes,
                 CASE WHEN c.reltoastrelid = 0 THEN 0 ELSE pg_total_relation_size(c.reltoastrelid) END AS lob_bytes";
    let estimated = "(c.relpages::bigint + coalesce((SELECT sum(i.relpages) FROM pg_index x JOIN pg_class i ON i.oid = x.indexrelid WHERE x.indrelid = s.relid), 0)) * current_setting('block_size')::bigint AS total_bytes,
                     (c.relpages::bigint + coalesce((SELECT t.relpages FROM pg_class t WHERE t.oid = c.reltoastrelid), 0)) * current_setting('block_size')::bigint AS data_bytes,
                     coalesce((SELECT sum(i.relpages) FROM pg_index x JOIN pg_class i ON i.oid = x.indexrelid WHERE x.indrelid = s.relid), 0) * current_setting('block_size')::bigint AS index_bytes,
                     c.relpages::bigint * current_setting('block_size')::bigint AS in_row_bytes,
                     coalesce((SELECT t.relpages FROM pg_class t WHERE t.oid = c.reltoastrelid), 0)::bigint * current_setting('block_size')::bigint AS lob_bytes";
    let size_basis = match sizes {
        SizeRead::Physical => "physical",
        SizeRead::CatalogEstimate => "catalog-estimate",
    };
    let sizes = match sizes {
        SizeRead::Physical => exact,
        SizeRead::CatalogEstimate => estimated,
    };
    let sql = {
        format!(
            "SELECT s.schemaname AS schema_name, s.relname AS table_name,
                    {rows} AS rows,
                    {sizes},
                    EXISTS (SELECT 1 FROM pg_constraint k WHERE k.conrelid = s.relid AND k.contype = 'p') AS has_pk,
                    s.seq_scan, s.seq_tup_read, s.idx_scan, s.n_tup_ins, s.n_tup_upd, s.n_tup_del, s.n_dead_tup,
                    s.n_mod_since_analyze,
                    {vacuum} AS last_vacuum,
                    {analyze} AS last_analyze,
                    CASE WHEN c.relkind = 'p' THEN NULL ELSE age(c.relfrozenxid) END AS xid_age,
                    {partition_of} AS partition_of,
                    s.relid::text AS relation_id, c.relkind::text AS relation_kind,
                    pg_catalog.to_jsonb(s)::text AS native_stats,
                    (SELECT oid::text FROM pg_catalog.pg_database WHERE datname=pg_catalog.current_database()) AS database_oid,
                    '{size_basis}'::text AS size_basis
             FROM pg_stat_user_tables s JOIN pg_class c ON c.oid = s.relid",
            rows = ROWS_SQL,
            vacuum = iso("greatest(s.last_vacuum, s.last_autovacuum)"),
            analyze = iso("greatest(s.last_analyze, s.last_autoanalyze)"),
            partition_of = if version_num >= 100_000 { partition_parent("c") } else { "NULL::text".to_string() },
        )
    };
    match limit {
        Some(limit) => format!("{sql} LIMIT {limit}"),
        None => sql,
    }
}

/// A table's row count. `n_live_tup` is a statistics counter: after a reset (`pg_stat_reset()`, crash recovery, a
/// restore, a promoted standby) it counts only inserts minus deletes since, so a 10-million-row table reads as a few
/// thousand rows until a vacuum or analyze sets it again. Until one has (no vacuum or analyze time since the reset),
/// the planner's own count (`reltuples`, kept in the catalog) is the better one; a table never analyzed has only the
/// counter. Partitioned parents hold no rows of their own.
const ROWS_SQL: &str = "CASE WHEN c.relkind = 'p' THEN NULL
                             WHEN c.reltuples < 0 THEN s.n_live_tup
                             WHEN s.last_vacuum IS NULL AND s.last_autovacuum IS NULL AND s.last_analyze IS NULL
                                  AND s.last_autoanalyze IS NULL THEN greatest(c.reltuples, s.n_live_tup)
                             ELSE s.n_live_tup END";

/// The relation `alias` (a pg_class row) is a partition of, as `schema.name` (PostgreSQL 10+ for tables, 11+ for
/// indexes); NULL for one that isn't.
pub(crate) fn partition_parent(alias: &str) -> String {
    format!(
        "CASE WHEN {alias}.relispartition THEN (SELECT pn.nspname || '.' || pc.relname FROM pg_inherits ih
           JOIN pg_class pc ON pc.oid = ih.inhparent JOIN pg_namespace pn ON pn.oid = pc.relnamespace
          WHERE ih.inhrelid = {alias}.oid LIMIT 1) END"
    )
}

pub(crate) fn table(row: &Row, database_oid: &str) -> Result<NativeCounterSet> {
    let stats: BTreeMap<String, Value> = serde_json::from_str(&text(row, "native_stats")?).map_err(|_| invalid())?;
    let identity =
        NativeIdentity::new(integer(row, "relation_id")?.to_string()).qualified("database_oid", database_oid);
    let mut counters = Vec::new();
    for name in [
        "seq_scan",
        "seq_tup_read",
        "idx_scan",
        "idx_tup_fetch",
        "n_tup_ins",
        "n_tup_upd",
        "n_tup_del",
        "n_tup_hot_upd",
        "n_tup_newpage_upd",
        "vacuum_count",
        "autovacuum_count",
        "analyze_count",
        "autoanalyze_count",
        "n_live_tup",
        "n_dead_tup",
        "n_mod_since_analyze",
        "n_ins_since_vacuum",
        "total_vacuum_time",
        "total_autovacuum_time",
        "total_analyze_time",
        "total_autoanalyze_time",
    ] {
        let Some(value) = stats.get(name).filter(|value| !value.is_null()) else { continue };
        let value = if name.ends_with("_time") {
            MeasurementValue::Real(
                value.as_f64().filter(|value| value.is_finite() && *value >= 0.0).ok_or_else(invalid)?,
            )
        } else {
            MeasurementValue::Integer(value.as_u64().ok_or_else(invalid)?.to_string())
        };
        counters.push(NativeCounterSample {
            key: name.into(),
            value,
            unit: if name.ends_with("_time") { CounterUnit::Milliseconds } else { CounterUnit::Count },
            nature: if matches!(name, "n_live_tup" | "n_dead_tup" | "n_mod_since_analyze" | "n_ins_since_vacuum") {
                CounterNature::Gauge
            } else {
                CounterNature::Cumulative
            },
            scope: MeasurementScope::Database,
            reset: ResetMarker::Unknown,
            native_object: "pg_stat_user_tables".into(),
            native_name: name.into(),
            native_instance: None,
            native_type: None,
        });
    }
    for name in ["rows", "total_bytes", "data_bytes", "index_bytes", "in_row_bytes", "lob_bytes", "xid_age"] {
        let Some(raw) = row.text(name) else { continue };
        let value = if name == "rows" {
            MeasurementValue::Real(
                raw.parse::<f64>().ok().filter(|value| value.is_finite() && *value >= 0.0).ok_or_else(invalid)?,
            )
        } else {
            MeasurementValue::Integer(raw.parse::<u64>().map_err(|_| invalid())?.to_string())
        };
        counters.push(NativeCounterSample {
            key: name.into(),
            value,
            unit: if name.ends_with("_bytes") { CounterUnit::Bytes } else { CounterUnit::Count },
            nature: CounterNature::Gauge,
            scope: MeasurementScope::Database,
            reset: ResetMarker::Unknown,
            native_object: "pg_class".into(),
            native_name: name.into(),
            native_instance: None,
            native_type: None,
        });
    }
    let mut properties = BTreeMap::new();
    for name in [
        "schema_name",
        "table_name",
        "relation_kind",
        "has_pk",
        "last_vacuum",
        "last_analyze",
        "partition_of",
        "size_basis",
    ] {
        if let Some(value) = row.text(name) {
            properties.insert(name.into(), value);
        }
    }
    let set = NativeCounterSet { identity, scope: MeasurementScope::Database, counters, properties };
    if !set.is_valid() {
        return Err(invalid());
    }
    Ok(set)
}
fn number(set: &NativeCounterSet, key: &str) -> Option<f64> {
    match &set.counters.iter().find(|value| value.key == key)?.value {
        MeasurementValue::Integer(value) => value.parse().ok(),
        MeasurementValue::Real(value) => Some(*value),
    }
}
fn label(set: &NativeCounterSet, key: &str) -> String {
    set.properties.get(key).cloned().unwrap_or_default()
}
fn optional_label(set: &NativeCounterSet, key: &str) -> Option<String> {
    set.properties.get(key).cloned()
}
fn boolean(set: &NativeCounterSet, key: &str) -> bool {
    set.properties.get(key).is_some_and(|value| matches!(value.as_str(), "t" | "true"))
}
pub(crate) fn project(r: &NativeCounterSet) -> TableStat {
    TableStat {
        schema: label(r, "schema_name"),
        name: label(r, "table_name"),
        kind: None,
        definition: None,
        rows: number(r, "rows").map(|v| v.max(0.0)),
        total_bytes: number(r, "total_bytes"),
        data_bytes: number(r, "data_bytes"),
        index_bytes: number(r, "index_bytes"),
        has_primary_key: boolean(r, "has_pk"),
        is_heap: None,
        seq_scans: number(r, "seq_scan"),
        seq_rows_read: number(r, "seq_tup_read"),
        index_scans: number(r, "idx_scan"),
        inserts: number(r, "n_tup_ins"),
        updates: number(r, "n_tup_upd"),
        deletes: number(r, "n_tup_del"),
        dead_rows: number(r, "n_dead_tup"),
        modifications_since_analyze: number(r, "n_mod_since_analyze"),
        last_vacuum: optional_label(r, "last_vacuum"),
        last_analyze: optional_label(r, "last_analyze"),
        forwarded_records: None,
        forwarded_fetches: None,
        xid_age: number(r, "xid_age"),
        storage: None,
        in_row_bytes: number(r, "in_row_bytes"),
        lob_bytes: number(r, "lob_bytes"),
        overflow_bytes: None,
        partition_of: optional_label(r, "partition_of"),
        counters_since: None,
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rows_after_a_statistics_reset_come_from_the_planner() {
        // No vacuum or analyze since the counters started: n_live_tup counts only inserts minus deletes since.
        assert!(ROWS_SQL.contains("s.last_autoanalyze IS NULL THEN greatest(c.reltuples, s.n_live_tup)"));
        assert!(ROWS_SQL.contains("WHEN c.relkind = 'p' THEN NULL"));
        let parent = partition_parent("c");
        assert!(parent.contains("c.relispartition") && parent.contains("ih.inhrelid = c.oid"), "{parent}");
    }
}
