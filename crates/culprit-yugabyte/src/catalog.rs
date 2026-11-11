use crate::read::{QueryRead, Row, query};
use crate::{probe::Probe, sources};
use culprit_core::{
    ColumnInfo, ConstraintIssue, Coverage, ForeignKey, IndexStat, Result, Setting, Snapshot, TableStat,
    signals::{DocdbIndex, DocdbTable, TableStorage},
};
use serde_json::json;
use std::collections::BTreeMap;

use culprit_postgres::wire::catalog::{self as native_catalog, USER_SCHEMA as USER_RELATIONS};

fn decode<T: serde::de::DeserializeOwned>(value: serde_json::Value) -> Result<T> {
    serde_json::from_value(value)
        .map_err(|_| culprit_core::Error::invalid(culprit_core::t!("provider.yugabyte.error.incomplete_catalog")))
}
fn array(row: &Row, key: &str) -> Vec<String> {
    row.text(key).and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
}
pub(crate) async fn tables(client: &mut impl QueryRead) -> Result<Vec<TableStat>> {
    let rows = query(
        client,
        &format!(
            "SELECT n.nspname AS schema, c.relname AS name,
        CASE WHEN c.reltuples >= 0 THEN c.reltuples::float8 END AS rows,
        EXISTS(SELECT 1 FROM pg_index i WHERE i.indrelid=c.oid AND i.indisprimary) AS pk,
        (SELECT pn.nspname || '.' || p.relname FROM pg_inherits inh JOIN pg_class p ON p.oid=inh.inhparent
         JOIN pg_namespace pn ON pn.oid=p.relnamespace WHERE inh.inhrelid=c.oid LIMIT 1) AS parent,
        c.relkind::text AS kind FROM pg_class c JOIN pg_namespace n ON n.oid=c.relnamespace
        WHERE c.relkind IN ('r','p','m') AND {USER_RELATIONS} ORDER BY n.nspname,c.relname"
        ),
    )
    .await?;
    rows.iter().map(|r|decode(json!({"schema":r.string("schema"),"name":r.string("name"),"rows":r.number("rows"),
        "has_primary_key":r.flag("pk"),"kind":if r.string("kind")=="p" {"partitioned"} else {"docdb"},"partition_of":r.text("parent")}))).collect()
}
pub(crate) async fn indexes(client: &mut impl QueryRead, _probe: &Probe) -> Result<Vec<IndexStat>> {
    let rows = query(client,&format!("SELECT n.nspname AS schema,t.relname AS table,i.relname AS name,am.amname AS kind,
        x.indisprimary AS pk,x.indisunique AS unique,x.indisready AS ready,x.indisvalid AS usable,
        pg_get_indexdef(i.oid) AS definition,pg_get_expr(x.indpred,x.indrelid) AS filter,
        (SELECT pn.nspname || '.' || p.relname FROM pg_inherits inh JOIN pg_class p ON p.oid=inh.inhparent
         JOIN pg_namespace pn ON pn.oid=p.relnamespace WHERE inh.inhrelid=i.oid LIMIT 1) AS partition_of,
        array_to_json(ARRAY(SELECT COALESCE(a.attname,pg_get_indexdef(i.oid,k,true)) || CASE WHEN (x.indoption[k-1] & 1)=1 THEN ' DESC' ELSE '' END
          FROM generate_series(1,x.indnkeyatts) k LEFT JOIN pg_attribute a ON a.attrelid=t.oid AND a.attnum=x.indkey[k-1] ORDER BY k))::text AS keys,
        array_to_json(ARRAY(SELECT concat_ws(' ',
          CASE WHEN NOT opc.opcdefault THEN opc.opcname END,
          CASE WHEN x.indcollation[k-1] <> 0 AND x.indcollation[k-1] IS DISTINCT FROM COALESCE(
            (SELECT a.attcollation FROM pg_attribute a WHERE a.attrelid=x.indrelid AND a.attnum=x.indkey[k-1]),100)
            THEN 'COLLATE ' || quote_ident(coll.collname) END)
          FROM generate_series(1,x.indnkeyatts) k JOIN pg_opclass opc ON opc.oid=x.indclass[k-1]
          LEFT JOIN pg_collation coll ON coll.oid=x.indcollation[k-1] ORDER BY k))::text AS opclasses,
        array_to_json(ARRAY(SELECT a.attname FROM generate_series(x.indnkeyatts+1,x.indnatts) k
          JOIN pg_attribute a ON a.attrelid=t.oid AND a.attnum=x.indkey[k-1] ORDER BY k))::text AS included
        FROM pg_index x JOIN pg_class i ON i.oid=x.indexrelid JOIN pg_class t ON t.oid=x.indrelid
        JOIN pg_namespace n ON n.oid=t.relnamespace JOIN pg_am am ON am.oid=i.relam WHERE i.relkind='i' AND {USER_RELATIONS} ORDER BY 1,2,3")).await?;
    rows.iter().map(|r|decode(json!({"schema":r.string("schema"),"table":r.string("table"),"name":r.string("name"),"kind":r.string("kind"),
        "columns":array(r,"keys"),"opclasses":array(r,"opclasses"),"include":array(r,"included"),"filter":r.text("filter"),"is_primary_key":r.flag("pk"),
        "is_unique":r.flag("unique"),"is_clustered":r.flag("pk"),"is_disabled":!r.flag("usable"),"maintained":r.flag("ready"),"definition":r.text("definition"),"partition_of":r.text("partition_of")}))).collect()
}
pub(crate) async fn details(client: &mut impl QueryRead, snapshot: &mut Snapshot, coverage: &mut Coverage) {
    let props = query(
        client,
        &format!(
            "SELECT n.nspname AS schema,c.relname AS name,p.* FROM pg_class c
        JOIN pg_namespace n ON n.oid=c.relnamespace CROSS JOIN LATERAL yb_table_properties(c.oid) p
        WHERE c.relkind IN ('r','m','i') AND {USER_RELATIONS}"
        ),
    )
    .await;
    let properties: BTreeMap<_, _> = coverage
        .read_rows(&sources::STORAGE, props)
        .unwrap_or_default()
        .into_iter()
        .map(|r| ((r.string("schema"), r.string("name")), r))
        .collect();
    let mut table_properties = BTreeMap::new();
    for table in &mut snapshot.tables {
        let key = (table.schema.clone(), table.name.clone());
        let Some(row) = properties.get(&key) else { continue };
        let Some(colocated) = row.text("is_colocated").map(|_| row.flag("is_colocated")) else { continue };
        let native = DocdbTable {
            tablets: row.string("num_tablets").parse().ok(),
            hash_key_columns: row.string("num_hash_key_columns").parse().ok(),
            colocated,
            tablegroup: row.text("tablegroup_oid"),
        };
        table_properties.insert(key, native.clone());
        table.storage = Some(TableStorage { docdb: Some(native), ..Default::default() });
    }
    for index in &mut snapshot.indexes {
        // A primary key is stored in the base LSM table; its index OID has no independent table properties.
        let (hash_keys, tablets) = if index.is_primary_key {
            let Some(base) = table_properties.get(&(index.schema.clone(), index.table.clone())) else { continue };
            (base.hash_key_columns, base.tablets)
        } else {
            let Some(row) = properties.get(&(index.schema.clone(), index.name.clone())) else { continue };
            (row.string("num_hash_key_columns").parse().ok(), row.string("num_tablets").parse().ok())
        };
        if let Some(hash_keys) = hash_keys.filter(|n| *n as usize <= index.columns.len()) {
            index.docdb = Some(DocdbIndex {
                hash_columns: index.key_names().into_iter().take(hash_keys as usize).map(str::to_string).collect(),
                tablets,
            });
        }
    }
    let sizes = query(
        client,
        &format!(
            "SELECT n.nspname AS schema,c.relname AS name,c.relkind::text AS kind,
        pg_table_size(c.oid)::float8 AS data,
        CASE WHEN c.relkind <> 'i' THEN pg_indexes_size(c.oid)::float8 END AS indexes,
        CASE WHEN c.relkind <> 'i' THEN pg_total_relation_size(c.oid)::float8 END AS total
        FROM pg_class c JOIN pg_namespace n ON n.oid=c.relnamespace
        WHERE c.relkind IN ('r','m','i') AND {USER_RELATIONS}
          AND NOT EXISTS(SELECT 1 FROM pg_index x WHERE x.indexrelid=c.oid AND x.indisprimary)"
        ),
    )
    .await;
    let sizes: BTreeMap<_, _> = coverage
        .read_rows(&sources::SIZES, sizes)
        .unwrap_or_default()
        .into_iter()
        .map(|r| ((r.string("schema"), r.string("name")), r))
        .collect();
    for table in &mut snapshot.tables {
        if !table.storage.as_ref().and_then(|s| s.docdb.as_ref()).is_some_and(|d| !d.colocated) {
            continue;
        }
        if let Some(row) = sizes.get(&(table.schema.clone(), table.name.clone())) {
            table.data_bytes = row.number("data");
            table.index_bytes = row.number("indexes");
            table.total_bytes = row.number("total");
        }
    }
    for index in &mut snapshot.indexes {
        let key = (index.schema.clone(), index.name.clone());
        // Colocated tables/indexes share storage. Never charge their entire tablegroup to every logical object.
        let standalone =
            properties.get(&key).is_some_and(|r| r.text("is_colocated").is_some() && !r.flag("is_colocated"));
        if !index.is_primary_key && standalone {
            index.size_bytes = sizes.get(&key).and_then(|r| r.number("data"));
        }
    }
}
async fn readings(
    reader: &mut impl QueryRead,
    probe: &Probe,
    kind: native_catalog::Kind,
    deparse: bool,
) -> Result<Vec<native_catalog::CatalogReading>> {
    query(reader, &native_catalog::query(kind, probe.compatibility, deparse, 10_001))
        .await?
        .into_iter()
        .map(|row| {
            native_catalog::decode(
                kind,
                row,
                None,
                culprit_core::telemetry::MAX_CATALOG_EXPRESSION_BYTES,
                culprit_core::Engine::Yugabyte,
            )
        })
        .collect()
}
pub(crate) async fn columns(reader: &mut impl QueryRead, probe: &Probe) -> Result<Vec<ColumnInfo>> {
    readings(reader, probe, native_catalog::Kind::Columns, true)
        .await?
        .into_iter()
        .map(native_catalog::column)
        .collect()
}
pub(crate) async fn foreign_keys(reader: &mut impl QueryRead, probe: &Probe) -> Result<Vec<ForeignKey>> {
    readings(reader, probe, native_catalog::Kind::ForeignKeys, false)
        .await?
        .into_iter()
        .map(native_catalog::foreign_key)
        .collect()
}
pub(crate) async fn settings(client: &mut impl QueryRead) -> Result<Vec<Setting>> {
    Ok(query(client,"SELECT name,setting,boot_val FROM pg_settings WHERE name LIKE 'yb_%' OR name IN ('work_mem','statement_timeout','lock_timeout','default_statistics_target','idle_in_transaction_session_timeout','plan_cache_mode','pg_stat_statements.track_planning')").await?.iter()
        .map(|r|Setting{name:r.string("name"),value:r.string("setting"),default_value:r.text("boot_val"),scope:"YSQL node".into()}).collect())
}

pub(crate) async fn constraints(reader: &mut impl QueryRead, probe: &Probe) -> Result<(Vec<ConstraintIssue>, bool)> {
    let constraints = readings(reader, probe, native_catalog::Kind::UnvalidatedConstraints, false)
        .await?
        .into_iter()
        .filter_map(native_catalog::constraint)
        .map(|mut constraint| {
            // YSQL native validation says nothing about PostgreSQL heap inheritance/constraint-exclusion behavior.
            constraint.inheritance_child = false;
            constraint
        })
        .collect();
    Ok((constraints, false))
}
