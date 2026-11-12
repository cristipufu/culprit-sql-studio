//! Node-native cumulative counters, separate from QPM's independently evictable cache and from SQL definitions.
use super::metadata::Metadata;
use super::*;
use culprit_core::{Engine, Error, ErrorCode, MeasurementScope, ResetMarker};
use std::collections::{BTreeMap, BTreeSet};
pub(crate) const SOURCE: &str = "ysql-node-pg-stat-statements";
pub(crate) async fn columns(reader: &mut impl QueryRead, relation: &str) -> Result<BTreeSet<String>> {
    reader.query(&format!("SELECT attname FROM pg_catalog.pg_attribute WHERE attrelid='{}'::regclass AND attnum>0 AND NOT attisdropped",relation.replace('\'',"''")),128)
        .await?.iter().map(|row|text(row,"attname")).collect()
}
pub(crate) struct Layout {
    pub columns: BTreeSet<String>,
    pub function: String,
    pub total: &'static str,
}
impl Layout {
    pub(crate) async fn read(reader: &mut impl QueryRead, meta: &Metadata) -> Result<Self> {
        if meta.probe.statement_role.is_some() {
            return Err(Error::new(ErrorCode::PermissionDenied, culprit_core::t!("collection.invalid_source")));
        }
        let schema = meta
            .probe
            .statement_schema
            .as_deref()
            .ok_or_else(|| Error::new(ErrorCode::Unsupported, culprit_core::t!("collection.unsupported")))?;
        let function = Engine::Yugabyte.dialect().qualified(&[schema, "pg_stat_statements"]);
        let member = one(reader,&format!("SELECT EXISTS(SELECT 1 FROM pg_catalog.pg_proc p JOIN pg_catalog.pg_namespace n ON n.oid=p.pronamespace
            JOIN pg_catalog.pg_depend d ON d.classid='pg_catalog.pg_proc'::regclass AND d.objid=p.oid AND d.deptype='e'
            JOIN pg_catalog.pg_extension e ON e.oid=d.refobjid JOIN pg_catalog.pg_roles r ON r.oid=p.proowner
            WHERE e.extname='pg_stat_statements' AND n.nspname='{}' AND p.proname='pg_stat_statements'
            AND p.pronargs=1 AND p.proargtypes='16'::oidvector AND r.rolsuper AND NOT p.prosecdef) AS trusted",schema.replace('\'',"''"))).await?;
        if !flag(&member, "trusted")? {
            return Err(CollectionStop::SourceUnavailable.error());
        }
        let columns = columns(reader, &function).await?;
        let total = if columns.contains("total_exec_time") { "total_exec_time" } else { "total_time" };
        if !["userid", "dbid", "queryid", "query", "calls", "rows", total].iter().all(|name| columns.contains(*name)) {
            return Err(invalid());
        }
        Ok(Self { columns, function, total })
    }
    pub(crate) fn query(&self, meta: &Metadata, definitions: bool, limit: usize, sql_bytes: usize) -> String {
        let mut fields = vec![
            "s.userid::text AS userid".into(),
            "s.dbid::text AS dbid".into(),
            "s.queryid::text AS queryid".into(),
            format!("{} AS observed_at", crate::probe::timestamp("pg_catalog.clock_timestamp()")),
        ];
        if self.columns.contains("toplevel") {
            fields.push("s.toplevel::text AS toplevel".into());
        }
        if definitions {
            fields.push(format!(
                "CASE WHEN pg_catalog.octet_length(s.query)<={sql_bytes} THEN s.query ELSE NULL END AS sql"
            ));
            fields.push("pg_catalog.octet_length(s.query)::text AS source_text_bytes".into());
        } else {
            fields.extend(metrics(self, meta).iter().map(|metric| format!("s.{0}::text AS {0}", metric.column)));
        }
        // Global FDW views can still materialize query bodies internally. Fast polling uses the native false/text
        // switch on one node. Node identity and scope are exported; this never claims a cluster-wide total.
        format!(
            "SELECT {} FROM {}({definitions}) s WHERE s.dbid={} LIMIT {limit}",
            fields.join(","),
            self.function,
            meta.probe.oid
        )
    }
}
struct Metric {
    column: &'static str,
    key: RuntimeMetricKey,
    unit: RuntimeMetricUnit,
    integer: bool,
}
fn metrics(layout: &Layout, meta: &Metadata) -> Vec<Metric> {
    let mut metrics = vec![
        Metric { column: "calls", key: RuntimeMetricKey::Executions, unit: RuntimeMetricUnit::Count, integer: true },
        Metric { column: "rows", key: RuntimeMetricKey::Rows, unit: RuntimeMetricUnit::Count, integer: true },
        Metric {
            column: layout.total,
            key: RuntimeMetricKey::Duration,
            unit: RuntimeMetricUnit::Milliseconds,
            integer: false,
        },
    ];
    for (column, enabled, time) in [
        ("docdb_read_rpcs", meta.probe.rpc_metrics, false),
        ("docdb_write_rpcs", meta.probe.rpc_metrics, false),
        ("docdb_read_operations", meta.probe.rpc_metrics, false),
        ("docdb_write_operations", meta.probe.rpc_metrics, false),
        ("docdb_wait_time", meta.probe.rpc_metrics, true),
        ("catalog_wait_time", meta.probe.rpc_metrics, true),
        ("docdb_rows_scanned", meta.probe.metrics, false),
        ("docdb_rows_returned", meta.probe.metrics, false),
        ("docdb_obsolete_rows_scanned", meta.probe.metrics, false),
        ("docdb_seeks", meta.probe.metrics, false),
        ("docdb_nexts", meta.probe.metrics, false),
        ("docdb_prevs", meta.probe.metrics, false),
        ("docdb_read_time", meta.probe.metrics, true),
        ("docdb_write_time", meta.probe.metrics, true),
        ("total_retries", true, false),
        ("conflict_retries", true, false),
        ("read_restart_retries", true, false),
    ] {
        if enabled && layout.columns.contains(column) {
            metrics.push(Metric {
                column,
                key: RuntimeMetricKey::Native(format!("ysql.{column}")),
                unit: if time { RuntimeMetricUnit::Milliseconds } else { RuntimeMetricUnit::Count },
                integer: !time,
            });
        }
    }
    metrics
}
fn identity(row: &Row, meta: &Metadata) -> Result<NativeQueryIdentity> {
    let query_id = text(row, "queryid")?;
    query_id.parse::<i64>().map_err(|_| invalid())?;
    if integer(row, "dbid")? != u64::from(meta.probe.oid) {
        return Err(invalid());
    }
    let mut qualifiers = BTreeMap::from([
        ("database_oid".into(), meta.probe.oid.to_string()),
        ("node".into(), meta.node.clone()),
        ("userid".into(), integer(row, "userid")?.to_string()),
    ]);
    if let Some(value) = row.text("toplevel") {
        qualifiers.insert("toplevel".into(), value);
    }
    Ok(NativeQueryIdentity { query_id, qualifiers })
}
pub(crate) fn counters(row: &Row, layout: &Layout, meta: &Metadata) -> Result<StatementCounterSample> {
    let totals = metrics(layout, meta)
        .into_iter()
        .map(|spec| {
            Ok(StatementCounter {
                key: spec.key,
                unit: spec.unit,
                total: if spec.integer {
                    MeasurementValue::Integer(integer(row, spec.column)?.to_string())
                } else {
                    MeasurementValue::Real(number(row, spec.column)?)
                },
            })
        })
        .collect::<Result<Vec<_>>>()?;
    let sample = StatementCounterSample {
        identity: identity(row, meta)?,
        scope: MeasurementScope::Node,
        observed_at: text(row, "observed_at")?,
        reset: ResetMarker::Unknown,
        totals,
        distributions: vec![],
    };
    if sample.is_valid() { Ok(sample) } else { Err(invalid()) }
}
pub(crate) fn definition(
    row: &Row,
    meta: &Metadata,
    source: &CollectionDescription,
    sql_bytes: usize,
) -> Result<QueryDefinition> {
    let identity = identity(row, meta)?;
    let sql = row.text("sql");
    let (origin, monitoring_template_id) = StatementOrigin::from_sql(sql.as_deref(), Engine::Yugabyte);
    let source_text_bytes = row.text("source_text_bytes");
    let payload = match sql {
        Some(sql) if sql.len() <= sql_bytes => DefinitionPayload::sql(&sql, Engine::Yugabyte),
        Some(_) => return Err(invalid()),
        None => DefinitionPayload::Unavailable {
            reason: if source_text_bytes
                .as_deref()
                .and_then(|value| value.parse::<usize>().ok())
                .is_some_and(|bytes| bytes > sql_bytes)
            {
                PayloadUnavailable::Oversized
            } else {
                PayloadUnavailable::Missing
            },
        },
    };
    Ok(QueryDefinition {
        id: identity.definition_id(source)?,
        identity,
        origin,
        monitoring_template_id,
        context: BTreeMap::new(),
        context_available: false,
        source_text_bytes,
        sql: payload,
    })
}
