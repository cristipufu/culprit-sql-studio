//! Digest layout is discovered once; counters never fetch statement bodies or application samples.
use super::*;
use culprit_core::{Engine, Error, ErrorCode, ResetMarker, telemetry::*};
use mysql_async::Value;
use std::collections::{BTreeMap, BTreeSet};

pub(crate) const SOURCE: &str = "performance_schema.events_statements_summary_by_digest";
pub(crate) const LOCK_TIME_COLUMN: &str = "SUM_LOCK_TIME";
pub(crate) const LOCK_TIME_METRIC: &str = "mysql.lock-time";

/// Keep the native subtotal, not a complete lock-wait breakdown or a particular lock resource.
/// MySQL 8.0.28 fixed TABLE/DATA lock accounting (Bug 33236909); that does not establish MariaDB's coverage.
/// https://dev.mysql.com/doc/relnotes/mysql/8.0/en/news-8-0-28.html
pub(crate) fn lock_time_ms(row: &Rec) -> Result<Option<f64>> {
    row.unsigned(LOCK_TIME_COLUMN).map(|value| value.map(|ps| ps as f64 / 1_000_000_000.0))
}

pub(crate) fn lock_waits(milliseconds: Option<f64>) -> Vec<culprit_core::WaitStat> {
    milliseconds
        .filter(|ms| *ms > 0.0)
        .map(|wait_ms| culprit_core::WaitStat {
            category: "Lock".into(),
            wait_type: Some(format!("{SOURCE}.{LOCK_TIME_COLUMN}")),
            wait_ms,
            tasks: None,
        })
        .into_iter()
        .collect()
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Layout {
    pub columns: BTreeSet<String>,
    pub cpu_enabled: bool,
    pub digest_enabled: bool,
    pub overflow_calls: u64,
}
impl Layout {
    pub(crate) async fn read<R: QueryRead>(reader: &mut R) -> Result<Self> {
        let columns = columns(reader).await?;
        let consumers = reader.read(
            "SELECT NAME AS name, ENABLED AS enabled FROM performance_schema.setup_consumers
              WHERE NAME IN ('global_instrumentation','thread_instrumentation','statements_digest','events_statements_cpu')",
            vec![], 5,
        ).await?;
        let mut enabled = BTreeSet::new();
        for row in consumers {
            let name = text(&row, "name")?;
            match text(&row, "enabled")?.as_str() {
                "YES" => {
                    enabled.insert(name);
                }
                "NO" => {}
                _ => return Err(invalid()),
            }
        }
        let overflow = reader.read(
            "SELECT COUNT_STAR AS calls FROM performance_schema.events_statements_summary_by_digest WHERE DIGEST IS NULL",
            vec![], 2,
        ).await?;
        if overflow.len() > 1 {
            return Err(invalid());
        }
        Ok(Self {
            columns,
            cpu_enabled: enabled.contains("events_statements_cpu"),
            digest_enabled: ["global_instrumentation", "thread_instrumentation", "statements_digest"]
                .iter()
                .all(|name| enabled.contains(*name)),
            overflow_calls: overflow.first().map(|row| integer(row, "calls")).transpose()?.unwrap_or(0),
        })
    }
    pub(crate) fn has(&self, name: &str) -> bool {
        self.columns.contains(name)
    }
    pub(crate) fn require(&self) -> Result<()> {
        if !self.digest_enabled {
            return Err(Error::new(ErrorCode::Unsupported, culprit_core::t!("collection.unsupported")));
        }
        if ["SCHEMA_NAME", "DIGEST", "DIGEST_TEXT", "COUNT_STAR", "SUM_TIMER_WAIT"].iter().all(|name| self.has(name)) {
            Ok(())
        } else {
            Err(invalid())
        }
    }
    pub(crate) fn query(&self, definitions: bool, row_limit: usize, sql_limit: usize) -> Result<String> {
        self.require()?;
        let mut columns = vec![
            "DIGEST AS digest".to_owned(),
            "SCHEMA_NAME AS schema_name".into(),
            "DATE_FORMAT(UTC_TIMESTAMP(6),'%Y-%m-%dT%H:%i:%s.%fZ') AS observed_at".into(),
        ];
        if definitions {
            columns.push(format!(
                "CASE WHEN OCTET_LENGTH(DIGEST_TEXT)<={sql_limit} THEN DIGEST_TEXT ELSE NULL END AS sql_text"
            ));
            columns.push("OCTET_LENGTH(DIGEST_TEXT) AS source_text_bytes".into());
        } else {
            columns.extend(
                metrics()
                    .into_iter()
                    .filter(|spec| self.has(spec.column) && (spec.column != "SUM_CPU_TIME" || self.cpu_enabled))
                    .map(|spec| spec.column.to_owned()),
            );
        }
        Ok(format!(
            "SELECT {} FROM {SOURCE} WHERE SCHEMA_NAME=? AND DIGEST IS NOT NULL LIMIT {row_limit}",
            columns.join(",")
        ))
    }
}

pub(crate) async fn columns<R: QueryRead>(reader: &mut R) -> Result<BTreeSet<String>> {
    reader
        .read(
            "SELECT COLUMN_NAME AS name FROM information_schema.COLUMNS
                  WHERE TABLE_SCHEMA='performance_schema' AND TABLE_NAME='events_statements_summary_by_digest'",
            vec![],
            128,
        )
        .await?
        .iter()
        .map(|row| text(row, "name").map(|name| name.to_ascii_uppercase()))
        .collect()
}

struct Metric {
    column: &'static str,
    key: RuntimeMetricKey,
    unit: RuntimeMetricUnit,
}
fn metrics() -> Vec<Metric> {
    use RuntimeMetricKey as Key;
    use RuntimeMetricUnit as Unit;
    let mut metrics = vec![
        Metric { column: "COUNT_STAR", key: Key::Executions, unit: Unit::Count },
        Metric { column: "SUM_TIMER_WAIT", key: Key::Duration, unit: Unit::Picoseconds },
        Metric { column: "SUM_CPU_TIME", key: Key::CpuTime, unit: Unit::Picoseconds },
        Metric { column: "SUM_ROWS_SENT", key: Key::Rows, unit: Unit::Count },
        Metric { column: LOCK_TIME_COLUMN, key: Key::Native(LOCK_TIME_METRIC.into()), unit: Unit::Picoseconds },
    ];
    for (column, name, unit) in [
        ("SUM_ROWS_EXAMINED", "rows-examined", Unit::Count),
        ("SUM_ROWS_AFFECTED", "rows-affected", Unit::Count),
        ("SUM_ERRORS", "errors", Unit::Count),
        ("SUM_WARNINGS", "warnings", Unit::Count),
        ("SUM_CREATED_TMP_DISK_TABLES", "temporary-disk-tables", Unit::Count),
        ("SUM_CREATED_TMP_TABLES", "temporary-tables", Unit::Count),
        ("SUM_SELECT_FULL_JOIN", "full-joins", Unit::Count),
        ("SUM_SELECT_SCAN", "table-scans", Unit::Count),
        ("SUM_SORT_MERGE_PASSES", "sort-merge-passes", Unit::Count),
        ("SUM_SORT_ROWS", "sorted-rows", Unit::Count),
        ("SUM_NO_INDEX_USED", "no-index-used", Unit::Count),
        ("SUM_NO_GOOD_INDEX_USED", "no-good-index-used", Unit::Count),
    ] {
        metrics.push(Metric { column, key: Key::Native(format!("mysql.{name}")), unit });
    }
    metrics
}
fn identity(row: &Rec, database: &str) -> Result<NativeQueryIdentity> {
    if text(row, "schema_name")? != database {
        return Err(invalid());
    }
    let digest = text(row, "digest")?;
    if !matches!(digest.len(), 32 | 64) || !digest.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(invalid());
    }
    // Neither FIRST_SEEN (second precision) nor server uptime proves an unreset digest generation.
    Ok(NativeQueryIdentity { query_id: digest, qualifiers: BTreeMap::from([("schema".into(), database.into())]) })
}
pub(crate) fn counters(row: &Rec, layout: &Layout, database: &str) -> Result<StatementCounterSample> {
    let totals = metrics()
        .into_iter()
        .filter(|spec| layout.has(spec.column) && (spec.column != "SUM_CPU_TIME" || layout.cpu_enabled))
        .map(|spec| {
            Ok(StatementCounter {
                key: spec.key,
                unit: spec.unit,
                total: MeasurementValue::Integer(integer(row, spec.column)?.to_string()),
            })
        })
        .collect::<Result<Vec<_>>>()?;
    let sample = StatementCounterSample {
        scope: culprit_core::MeasurementScope::Database,
        identity: identity(row, database)?,
        observed_at: text(row, "observed_at")?,
        reset: ResetMarker::Unknown,
        totals,
        distributions: vec![],
    };
    if sample.is_valid() { Ok(sample) } else { Err(invalid()) }
}
pub(crate) fn definition(
    row: &Rec,
    database: &str,
    source: &CollectionDescription,
    sql_limit: usize,
) -> Result<QueryDefinition> {
    let identity = identity(row, database)?;
    let bytes = row.unsigned("source_text_bytes")?;
    let sql = match row.native_text("sql_text")? {
        Some(text) if text.len() <= sql_limit => DefinitionPayload::sql(&text, Engine::Mysql),
        Some(_) => return Err(invalid()),
        None => DefinitionPayload::Unavailable {
            reason: if bytes.is_some_and(|bytes| bytes > sql_limit as u64) {
                PayloadUnavailable::Oversized
            } else {
                PayloadUnavailable::Missing
            },
        },
    };
    Ok(QueryDefinition {
        id: identity.definition_id(source)?,
        identity,
        sql,
        // Normalization merges sessions and removes comments; a digest cannot establish its owner.
        origin: StatementOrigin::Unknown,
        monitoring_template_id: None,
        context: BTreeMap::new(),
        context_available: false,
        source_text_bytes: bytes.map(|bytes| bytes.to_string()),
    })
}

pub(crate) fn parameters(database: &str) -> Vec<Value> {
    vec![Value::from(database)]
}
