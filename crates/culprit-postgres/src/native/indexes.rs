//! One index relation and decoder for scans and telemetry; background reads never deparse user expressions.
use super::relations::partition_parent;
use super::*;
use culprit_core::{IndexStat, MeasurementScope, telemetry::*};
use std::collections::BTreeMap;

pub(crate) struct IndexReading {
    pub native: NativeCatalogCounters,
    row: Row,
}
pub(crate) fn query(version_num: u32, timescale: bool, deparse: bool, limit: Option<usize>) -> String {
    let last_read = if version_num >= 160_000 { iso("s.last_idx_scan") } else { "NULL::text".to_string() };
    // An index being built or rebuilt by CREATE/REINDEX CONCURRENTLY has indisvalid = false until the build finishes;
    // pg_stat_progress_create_index (PostgreSQL 12+) distinguishes a live build from a failed leftover.
    let usable = if version_num >= 120_000 {
        "(x.indisvalid AND x.indisready) OR EXISTS (SELECT 1 FROM pg_stat_progress_create_index p WHERE p.index_relid = x.indexrelid OR p.relid = x.indrelid)"
    } else {
        "x.indisvalid AND x.indisready"
    };
    let usable = if timescale && version_num >= 120_000 {
        format!(
            "({usable}) OR EXISTS (SELECT 1 FROM pg_stat_progress_create_index p
           JOIN pg_class chunk ON chunk.oid = p.relid JOIN pg_namespace cn ON cn.oid = chunk.relnamespace
           JOIN timescaledb_information.chunks tc ON tc.chunk_schema = cn.nspname AND tc.chunk_name = chunk.relname
          WHERE tc.hypertable_schema = n.nspname AND tc.hypertable_name = t.relname)"
        )
    } else {
        usable.to_string()
    };
    let key_expression = if deparse { "pg_get_indexdef(x.indexrelid, k, true)" } else { "NULL::text" };
    let filter = if deparse { "pg_get_expr(x.indpred, x.indrelid, true)" } else { "NULL::text" };
    let definition = if deparse { "pg_get_indexdef(x.indexrelid)" } else { "NULL::text" };
    let sql = format!(
        "SELECT n.nspname AS schema_name, t.relname AS table_name, i.relname AS index_name, am.amname AS kind,
                x.indisunique, x.indisprimary, x.indisready, {usable} AS usable,
                (SELECT string_agg(
                    CASE WHEN x.indkey[k - 1] <> 0
                         THEN (SELECT a.attname FROM pg_attribute a WHERE a.attrelid = x.indrelid AND a.attnum = x.indkey[k - 1])
                         ELSE {key_expression} END
                    || CASE WHEN (x.indoption[k - 1] & 1) = 1 THEN ' DESC' ELSE '' END,
                    chr(31) ORDER BY k)
                 FROM generate_series(1, x.indnkeyatts) AS k) AS key_columns,
                (SELECT string_agg(
                    CASE WHEN x.indkey[k - 1] <> 0
                         THEN (SELECT a.attname FROM pg_attribute a WHERE a.attrelid = x.indrelid AND a.attnum = x.indkey[k - 1])
                         ELSE {key_expression} END,
                    chr(31) ORDER BY k)
                 FROM generate_series(x.indnkeyatts + 1, x.indnatts) AS k) AS include_columns,
                (SELECT string_agg(concat_ws(' ',
                    CASE WHEN NOT opc.opcdefault THEN opc.opcname END,
                    CASE WHEN x.indcollation[k - 1] <> 0
                          AND x.indcollation[k - 1] IS DISTINCT FROM coalesce(
                              (SELECT a.attcollation FROM pg_attribute a WHERE a.attrelid = x.indrelid AND a.attnum = x.indkey[k - 1]),
                              100)
                         THEN 'COLLATE ' || quote_ident(coll.collname) END), chr(31) ORDER BY k)
                 FROM generate_series(1, x.indnkeyatts) AS k
                 JOIN pg_opclass opc ON opc.oid = x.indclass[k - 1]
                 LEFT JOIN pg_collation coll ON coll.oid = x.indcollation[k - 1]) AS key_opclasses,
                {partition_of} AS partition_of,
                {filter} AS filter,
                i.relpages::bigint * current_setting('block_size')::bigint AS estimated_bytes,
                s.idx_scan, {last_read} AS last_read,
                {definition} AS definition,
                x.indexrelid::text AS index_oid, x.indrelid::text AS table_oid,
                x.indkey::text AS native_keys, x.indnkeyatts::text AS native_key_count,
                x.indclass::text AS native_opclasses, x.indcollation::text AS native_collations,
                x.indoption::text AS native_options, x.indisvalid AS native_valid, x.indislive AS native_live,
                (x.indpred IS NOT NULL) AS has_predicate,
                md5(x.indpred::text) AS predicate_md5, md5(x.indexprs::text) AS expressions_md5
         FROM pg_index x
         JOIN pg_class i ON i.oid = x.indexrelid
         JOIN pg_class t ON t.oid = x.indrelid
         JOIN pg_namespace n ON n.oid = t.relnamespace
         JOIN pg_am am ON am.oid = i.relam
         LEFT JOIN pg_stat_user_indexes s ON s.indexrelid = x.indexrelid
         WHERE n.nspname NOT IN ('pg_catalog', 'information_schema')
           AND n.nspname NOT LIKE 'pg\\_toast%' AND n.nspname NOT LIKE 'pg\\_temp%'
           AND t.relkind IN ('r', 'm', 'p')",
        partition_of = partition_parent("i"),
    );
    match limit {
        Some(limit) => format!("{sql} LIMIT {limit}"),
        None => sql,
    }
}

fn expression(raw: Option<String>, present: bool) -> Option<CatalogExpression> {
    if let Some(raw) = raw {
        Some(CatalogExpression::sql(&raw, culprit_core::Engine::Postgres, MAX_CATALOG_EXPRESSION_BYTES))
    } else if present {
        Some(CatalogExpression {
            native_sha256: None,
            payload: DefinitionPayload::Unavailable { reason: PayloadUnavailable::NotCollected },
        })
    } else {
        None
    }
}
pub(crate) fn decode(row: Row, database_oid: Option<&str>) -> Result<IndexReading> {
    let index_oid = integer(&row, "index_oid")?.to_string();
    let table_oid = integer(&row, "table_oid")?.to_string();
    let mut identity = NativeIdentity::new(index_oid).qualified("table_oid", table_oid);
    if let Some(database_oid) = database_oid {
        identity = identity.qualified("database_oid", database_oid);
    }
    let mut properties = BTreeMap::new();
    for name in [
        "schema_name",
        "table_name",
        "index_name",
        "kind",
        "indisunique",
        "indisprimary",
        "indisready",
        "native_valid",
        "native_live",
        "native_keys",
        "native_key_count",
        "native_opclasses",
        "native_collations",
        "native_options",
        "partition_of",
        "predicate_md5",
        "expressions_md5",
    ] {
        if let Some(value) = row.text(name) {
            properties.insert(name.into(), value);
        }
    }
    let catalog = NativeCatalogDefinition {
        identity: identity.clone(),
        object_type: "pg_index".into(),
        properties,
        definition: expression(row.text("definition"), true),
        predicate: expression(row.text("filter"), flag(&row, "has_predicate")?),
    };
    let mut counters = vec![];
    for (column, unit) in [("idx_scan", CounterUnit::Count), ("estimated_bytes", CounterUnit::Bytes)] {
        if let Some(raw) = row.text(column) {
            let value = raw.parse::<u64>().map_err(|_| invalid())?;
            let mut sample = NativeCounterSample::gauge(
                column,
                value,
                unit,
                MeasurementScope::Database,
                "pg_stat_user_indexes",
                column,
            );
            if column == "idx_scan" {
                sample.nature = CounterNature::Cumulative;
            }
            counters.push(sample);
        }
    }
    let stats = NativeCounterSet {
        identity,
        scope: MeasurementScope::Database,
        counters,
        properties: BTreeMap::from([
            ("index_name".into(), text(&row, "index_name")?),
            ("size_basis".into(), "catalog-estimate".into()),
        ]),
    };
    let native = NativeCatalogCounters { catalog, counters: stats };
    if !native.is_valid() {
        return Err(invalid());
    }
    Ok(IndexReading { native, row })
}
pub(crate) fn project(reading: &IndexReading) -> IndexStat {
    let r = &reading.row;
    IndexStat {
        docdb: None,
        filter_facts: None,
        schema: r.string("schema_name"),
        table: r.string("table_name"),
        name: r.string("index_name"),
        kind: r.string("kind"),
        columns: r.list("key_columns"),
        include: r.list("include_columns"),
        filter: r.text("filter"),
        is_unique: r.flag("indisunique"),
        is_primary_key: r.flag("indisprimary"),
        is_clustered: false,
        is_disabled: !r.flag("usable"),
        state: None,
        maintained: Some(r.flag("indisready")),
        has_statistics: None,
        size_bytes: r.number("estimated_bytes"),
        reads: r.number("idx_scan"),
        writes: None,
        seeks: None,
        scans: r.number("idx_scan"),
        lookups: None,
        last_read: r.text("last_read"),
        definition: r.text("definition"),
        contention: None,
        created_at_latest: None,
        partition_of: r.text("partition_of"),
        opclasses: opclasses(r.text("key_opclasses").as_deref()),
        counters_since: None,
    }
}

/// Per key, its operator class and collation when they aren't the defaults (`key_opclasses`, chr(31)-separated, empty
/// for a key on the defaults). `None` when every key uses the defaults.
fn opclasses(list: Option<&str>) -> Option<Vec<String>> {
    let keys: Vec<String> = list?.split(crate::rows::LIST_SEPARATOR).map(|k| k.trim().to_string()).collect();
    keys.iter().any(|k| !k.is_empty()).then_some(keys)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn operator_classes_and_collations_per_key() {
        assert_eq!(opclasses(None), None);
        assert_eq!(opclasses(Some("")), None);
        assert_eq!(opclasses(Some("\u{1f}")), None, "every key on the defaults");
        assert_eq!(
            opclasses(Some("text_pattern_ops\u{1f}")),
            Some(vec!["text_pattern_ops".to_string(), String::new()])
        );
        assert_eq!(opclasses(Some("COLLATE \"C\"")), Some(vec!["COLLATE \"C\"".to_string()]));
    }
}
