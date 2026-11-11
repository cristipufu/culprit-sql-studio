//! PostgreSQL statement identities, counters and distributions. Layout and units are native facts, not SQL Server defaults.
use super::metadata::{Metadata, StatementCatalog};
use super::*;
use culprit_core::{Engine, Error, ErrorCode, ResetMarker, telemetry::*};
use std::collections::BTreeMap;

pub(crate) struct Layout {
    pub total: &'static str,
    pub mean: &'static str,
    pub max: &'static str,
    pub stddev: &'static str,
    pub min: &'static str,
    pub io: &'static str,
    pub since: Option<&'static str>,
    pub toplevel: bool,
}
impl Layout {
    pub fn for_version(version: u32) -> Self {
        if version < 130_000 {
            return Self::legacy();
        }
        Self {
            total: "total_exec_time",
            mean: "mean_exec_time",
            max: "max_exec_time",
            stddev: "stddev_exec_time",
            min: "min_exec_time",
            io: crate::pressure::statements_io_column(version),
            since: (version >= 170_000).then_some("stats_since"),
            toplevel: version >= 140_000,
        }
    }
    pub fn legacy() -> Self {
        Self {
            total: "total_time",
            mean: "mean_time",
            max: "max_time",
            stddev: "stddev_time",
            min: "min_time",
            io: "(blk_read_time + blk_write_time)",
            since: None,
            toplevel: false,
        }
    }
    pub fn detect(catalog: &StatementCatalog, version: u32) -> Result<Self> {
        let mut layout = if catalog.columns.contains("total_exec_time") {
            Self::for_version(version.max(130_000))
        } else {
            Self::legacy()
        };
        layout.since = catalog.columns.contains("stats_since").then_some("stats_since");
        layout.toplevel = catalog.columns.contains("toplevel");
        if [
            "userid",
            "dbid",
            "queryid",
            "query",
            "calls",
            "rows",
            layout.total,
            layout.mean,
            layout.max,
            layout.stddev,
            layout.min,
        ]
        .iter()
        .all(|column| catalog.columns.contains(*column))
        {
            Ok(layout)
        } else {
            Err(invalid())
        }
    }
}
struct MetricSpec {
    column: &'static str,
    key: RuntimeMetricKey,
    unit: RuntimeMetricUnit,
    integer: bool,
}
fn metrics(meta: &Metadata, layout: &Layout) -> Vec<MetricSpec> {
    use RuntimeMetricKey as Key;
    use RuntimeMetricUnit as Unit;
    let pages = Unit::Pages { bytes_per_page: meta.block_bytes };
    let mut specs = vec![
        MetricSpec { column: "calls", key: Key::Executions, unit: Unit::Count, integer: true },
        MetricSpec { column: "rows", key: Key::Rows, unit: Unit::Count, integer: true },
        MetricSpec { column: layout.total, key: Key::Duration, unit: Unit::Milliseconds, integer: false },
        MetricSpec { column: "wal_bytes", key: Key::LogBytes, unit: Unit::Bytes, integer: true },
    ];
    for (column, key, unit, integer) in [
        ("shared_blks_hit", "shared-block-hits", pages, true),
        ("shared_blks_read", "shared-block-reads", pages, true),
        ("shared_blks_dirtied", "shared-blocks-dirtied", pages, true),
        ("shared_blks_written", "shared-blocks-written", pages, true),
        ("local_blks_hit", "local-block-hits", pages, true),
        ("local_blks_read", "local-block-reads", pages, true),
        ("local_blks_dirtied", "local-blocks-dirtied", pages, true),
        ("local_blks_written", "local-blocks-written", pages, true),
        ("temp_blks_read", "temporary-block-reads", pages, true),
        ("temp_blks_written", "temporary-block-writes", pages, true),
        ("plans", "plans", Unit::Count, true),
        ("total_plan_time", "planning-duration", Unit::Milliseconds, false),
        ("wal_records", "wal-records", Unit::Count, true),
        ("wal_fpi", "wal-full-page-images", Unit::Count, true),
        ("blk_read_time", "io-read-time", Unit::Milliseconds, false),
        ("blk_write_time", "io-write-time", Unit::Milliseconds, false),
        ("shared_blk_read_time", "shared-io-read-time", Unit::Milliseconds, false),
        ("shared_blk_write_time", "shared-io-write-time", Unit::Milliseconds, false),
        ("local_blk_read_time", "local-io-read-time", Unit::Milliseconds, false),
        ("local_blk_write_time", "local-io-write-time", Unit::Milliseconds, false),
        ("temp_blk_read_time", "temporary-io-read-time", Unit::Milliseconds, false),
        ("temp_blk_write_time", "temporary-io-write-time", Unit::Milliseconds, false),
        ("jit_functions", "jit-functions", Unit::Count, true),
        ("jit_generation_time", "jit-generation-time", Unit::Milliseconds, false),
        ("jit_inlining_count", "jit-inlining-count", Unit::Count, true),
        ("jit_inlining_time", "jit-inlining-time", Unit::Milliseconds, false),
        ("jit_optimization_count", "jit-optimization-count", Unit::Count, true),
        ("jit_optimization_time", "jit-optimization-time", Unit::Milliseconds, false),
        ("jit_emission_count", "jit-emission-count", Unit::Count, true),
        ("jit_emission_time", "jit-emission-time", Unit::Milliseconds, false),
        ("jit_deform_count", "jit-deform-count", Unit::Count, true),
        ("jit_deform_time", "jit-deform-time", Unit::Milliseconds, false),
        ("parallel_workers_to_launch", "parallel-workers-requested", Unit::Count, true),
        ("parallel_workers_launched", "parallel-workers-launched", Unit::Count, true),
    ] {
        specs.push(MetricSpec { column, key: Key::Native(format!("postgres.{key}")), unit, integer });
    }
    specs
}
fn catalog(meta: &Metadata) -> Result<&StatementCatalog> {
    if !meta.full_visibility {
        return Err(Error::new(ErrorCode::PermissionDenied, culprit_core::t!("collection.invalid_source")));
    }
    meta.statements
        .as_ref()
        .ok_or_else(|| Error::new(ErrorCode::Unsupported, culprit_core::t!("collection.unsupported")))
}
pub(crate) fn query(meta: &Metadata, definitions: bool, limit: usize, max_sql_bytes: usize) -> Result<String> {
    let catalog = catalog(meta)?;
    let layout = Layout::detect(catalog, meta.version)?;
    let mut columns =
        vec!["s.userid::text AS userid".into(), "s.dbid::text AS dbid".into(), "s.queryid::text AS queryid".into()];
    columns.push(format!("{} AS observed_at", iso("pg_catalog.clock_timestamp()")));
    if layout.toplevel {
        columns.push("s.toplevel::text AS toplevel".into());
    }
    for name in ["stats_since", "minmax_stats_since"] {
        if catalog.columns.contains(name) {
            columns.push(format!("{} AS {name}", iso(&format!("s.{name}"))));
        }
    }
    if definitions {
        columns.push(format!(
            "CASE WHEN pg_catalog.octet_length(s.query)<={max_sql_bytes} THEN s.query ELSE NULL END AS query"
        ));
        columns.push("pg_catalog.octet_length(s.query)::text AS source_text_bytes".into());
    } else {
        for spec in metrics(meta, &layout) {
            if catalog.columns.contains(spec.column) {
                columns.push(format!("s.{0}::text AS {0}", spec.column));
            }
        }
        for name in [
            layout.mean,
            layout.min,
            layout.max,
            layout.stddev,
            "mean_plan_time",
            "min_plan_time",
            "max_plan_time",
            "stddev_plan_time",
        ] {
            if catalog.columns.contains(name) {
                columns.push(format!("s.{name}::text AS {name}"));
            }
        }
    }
    Ok(format!(
        "SELECT {} FROM {}.pg_stat_statements({definitions}) s WHERE s.dbid={} LIMIT {limit}",
        columns.join(","),
        crate::rows::quote_ident(&catalog.schema),
        meta.database_oid
    ))
}
fn identity(row: &Row, meta: &Metadata) -> Result<NativeQueryIdentity> {
    let query_id = text(row, "queryid")?;
    query_id.parse::<i64>().map_err(|_| invalid())?;
    let dbid = integer(row, "dbid")?.to_string();
    if dbid != meta.database_oid {
        return Err(invalid());
    }
    let mut qualifiers = BTreeMap::from([
        ("userid".into(), integer(row, "userid")?.to_string()),
        ("dbid".into(), dbid),
        ("postmaster_started_at".into(), meta.started_at.clone()),
    ]);
    if row.value("toplevel").is_some() {
        qualifiers.insert("toplevel".into(), flag(row, "toplevel")?.to_string());
    }
    if let Some(value) = row.text("stats_since") {
        qualifiers.insert("stats_since".into(), canonical_time(&value)?);
    }
    Ok(NativeQueryIdentity { query_id, qualifiers })
}
fn reset_marker(row: &Row, name: &str) -> Result<ResetMarker> {
    row.text(name)
        .map(|value| canonical_time(&value).map(ResetMarker::At))
        .transpose()
        .map(|value| value.unwrap_or(ResetMarker::Unknown))
}
fn number(row: &Row, name: &str) -> Result<Option<f64>> {
    row.text(name)
        .map(|value| {
            value.parse::<f64>().ok().filter(|number| number.is_finite() && *number >= 0.0).ok_or_else(invalid)
        })
        .transpose()
}
pub(crate) fn counters(row: &Row, meta: &Metadata) -> Result<StatementCounterSample> {
    let layout = Layout::detect(catalog(meta)?, meta.version)?;
    let mut totals = vec![];
    for spec in metrics(meta, &layout) {
        let Some(value) = row.text(spec.column) else { continue };
        let total = if spec.integer {
            MeasurementValue::Integer(value.parse::<u64>().map_err(|_| invalid())?.to_string())
        } else {
            MeasurementValue::Real(number(row, spec.column)?.ok_or_else(invalid)?)
        };
        totals.push(StatementCounter { key: spec.key, unit: spec.unit, total });
    }
    let reset = reset_marker(row, "stats_since")?;
    let extrema_reset = reset_marker(row, "minmax_stats_since")?;
    let mut distributions = vec![];
    for (key, mean, min, max, stddev) in [
        (RuntimeMetricKey::Duration, layout.mean, layout.min, layout.max, layout.stddev),
        (
            RuntimeMetricKey::Native("postgres.planning-duration".into()),
            "mean_plan_time",
            "min_plan_time",
            "max_plan_time",
            "stddev_plan_time",
        ),
    ] {
        let count_column = if key == RuntimeMetricKey::Duration { "calls" } else { "plans" };
        if row.text(count_column).is_some_and(|value| value == "0") {
            continue;
        }
        if let Some(average) = number(row, mean)? {
            distributions.push(StatementDistribution {
                statistics: RuntimeMetric {
                    minimum: number(row, min)?,
                    maximum: number(row, max)?,
                    standard_deviation: number(row, stddev)?,
                    ..RuntimeMetric::average(key, RuntimeMetricUnit::Milliseconds, average)
                },
                reset: reset.clone(),
                extrema_reset: extrema_reset.clone(),
            });
        }
    }
    let sample = StatementCounterSample {
        identity: identity(row, meta)?,
        observed_at: text(row, "observed_at")?,
        reset,
        totals,
        distributions,
    };
    if !sample.is_valid() {
        return Err(invalid());
    }
    Ok(sample)
}
pub(crate) fn definition(
    row: &Row,
    meta: &Metadata,
    source: &CollectionDescription,
    max_sql_bytes: usize,
) -> Result<QueryDefinition> {
    let identity = identity(row, meta)?;
    let source_text_bytes = row.text("source_text_bytes");
    let text = row.text("query");
    let (origin, monitoring_template_id) = provenance(text.as_deref());
    let sql = match &text {
        Some(text) => DefinitionPayload::sql(text, Engine::Postgres),
        None => DefinitionPayload::Unavailable {
            reason: if source_text_bytes
                .as_deref()
                .and_then(|value| value.parse::<usize>().ok())
                .is_some_and(|bytes| bytes > max_sql_bytes)
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
        context: Default::default(),
        context_available: false,
        source_text_bytes,
        sql,
    })
}
fn provenance(text: Option<&str>) -> (StatementOrigin, Option<String>) {
    let Some(text) = text else { return (StatementOrigin::Unknown, None) };
    let Ok(Some(comments)) = culprit_core::sql::block_comments(text, Engine::Postgres) else {
        return (StatementOrigin::Unknown, None);
    };
    let origin = if comments.contains(&"/* culprit:monitor */") {
        StatementOrigin::CulpritMonitoring
    } else if comments.contains(&crate::rows::TAG) {
        StatementOrigin::Culprit
    } else {
        StatementOrigin::Application
    };
    let template = (origin == StatementOrigin::CulpritMonitoring)
        .then(|| {
            comments.iter().find_map(|comment| {
                comment
                    .strip_prefix("/* culprit:template:")
                    .and_then(|value| value.strip_suffix(" */"))
                    .filter(|value| valid_digest(value))
                    .map(str::to_owned)
            })
        })
        .flatten();
    (origin, template)
}

pub(crate) fn aggregated_base(relation: &str, c: &Layout) -> String {
    let toplevel = if c.toplevel { " AND toplevel" } else { "" };
    let first_seen =
        c.since.map(|since| crate::rows::iso(&format!("min({since})"))).unwrap_or_else(|| "NULL::text".to_string());
    format!(
        "SELECT queryid::text AS id, (array_agg(query ORDER BY {total} DESC))[1] AS query, sum(calls) AS calls,
                sum({total}) AS total_ms, sum({total}) / nullif(sum(calls), 0)::float8 AS mean_ms,
                max({max}) AS max_ms, min({min}) AS min_ms,
                sqrt(greatest(sum(calls * ({stddev} ^ 2 + {mean} ^ 2)) / nullif(sum(calls), 0)::float8
                              - (sum({total}) / nullif(sum(calls), 0)::float8) ^ 2, 0)) AS stddev_ms,
                sum(rows) AS rows, sum(shared_blks_hit + shared_blks_read) AS reads, sum(shared_blks_read) AS phys,
                sum(temp_blks_written) AS temp, sum({io}) AS io_ms, {first_seen} AS first_seen,
                count(*) AS entries,
                (array_agg(query ORDER BY {total} DESC)
                   FILTER (WHERE pg_get_userbyid(userid) = session_user AND current_user = session_user))[1] AS own_query
         FROM {relation}
         WHERE dbid = (SELECT oid FROM pg_database WHERE datname = current_database()){toplevel}
           AND query NOT LIKE '%{tag}'
         GROUP BY queryid",
        tag=crate::rows::TAG,
        total = c.total,
        mean = c.mean,
        max = c.max,
        stddev = c.stddev,
        min = c.min,
        io = c.io,
    )
}

pub(crate) fn statements_base(relation: &str, version_num: u32) -> String {
    aggregated_base(relation, &Layout::for_version(version_num))
}
pub(crate) fn statements_base_legacy(relation: &str) -> String {
    aggregated_base(relation, &Layout::legacy())
}
