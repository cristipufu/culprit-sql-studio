use std::collections::{BTreeMap, HashMap, HashSet};

use culprit_core::{Coverage, PlanVariant, QueryStat, SnapshotOptions, WaitStat, Workload};

use crate::plan_xml;
use crate::read::NativeRead;

/// The heaviest queries get a trend line; the rest would only grow the report.
const TREND_QUERIES: usize = 40;

/// Query Store wait categories that are not the database working: idle sessions, WAITFOR, tracing.
const IGNORED_WAIT_CATEGORIES: &[&str] = &["Idle", "User Wait", "Tracing", "Service Broker", "Unknown"];

pub(crate) fn wait_category(query_store_category: &str) -> &'static str {
    match query_store_category {
        "CPU" | "Worker Thread" => "CPU",
        "Lock" => "Lock",
        "Latch" | "Buffer Latch" => "Latch",
        "Buffer IO" | "Other Disk IO" => "IO",
        "Tran Log IO" | "Log Rate Governor" => "Log",
        "Network IO" => "Network",
        "Parallelism" => "Parallelism",
        "Memory" => "Memory",
        "Compilation" => "Compilation",
        _ => "Other",
    }
}

#[derive(Clone, Debug, Default)]
struct PlanAgg {
    plan_id: String,
    executions: f64,
    total_ms: f64,
    max_ms: f64,
    /// The fastest run, when the source keeps it.
    min_ms: Option<f64>,
    cpu_ms: f64,
    reads: f64,
    physical_reads: f64,
    rows: f64,
    tempdb_pages: Option<f64>,
    /// Σ executions × (stdev² + mean²) in µs², so the pooled variance across intervals and plans can be recovered.
    sum_sq_us: f64,
    first_seen: Option<String>,
    last_seen: Option<String>,
    forced: bool,
}

impl PlanAgg {
    fn from_cache_summary(
        summary: &crate::cached_runtime::Summary,
        counts: &crate::cached_runtime::SummaryCounts,
    ) -> Self {
        Self {
            plan_id: summary.query_hash.clone(),
            executions: counts.executions as f64,
            total_ms: counts.duration_us as f64 / 1000.0,
            max_ms: counts.maximum_us as f64 / 1000.0,
            min_ms: Some(counts.minimum_us as f64 / 1000.0),
            cpu_ms: counts.worker_us as f64 / 1000.0,
            reads: counts.logical_reads as f64,
            physical_reads: counts.physical_reads as f64,
            rows: counts.rows as f64,
            first_seen: Some(summary.first_seen_utc_estimate.clone()),
            last_seen: Some(summary.last_seen_utc_estimate.clone()),
            ..Self::default()
        }
    }

    fn from_query_store(value: crate::query_store::runtime::PlanSummary) -> Self {
        use crate::query_store::runtime::milliseconds;
        let stats = value.moments;
        Self {
            plan_id: value.plan_id,
            executions: stats.executions as f64,
            total_ms: milliseconds(stats.duration_us),
            max_ms: milliseconds(stats.max_us),
            min_ms: Some(milliseconds(stats.min_us)),
            cpu_ms: milliseconds(stats.cpu_us),
            reads: stats.logical_reads,
            physical_reads: stats.physical_reads,
            rows: stats.rows,
            tempdb_pages: stats.tempdb_pages,
            sum_sq_us: stats.sum_sq_us,
            first_seen: value.first_seen,
            last_seen: value.last_seen,
            forced: value.forced,
        }
    }

    /// This plan's own spread, pooled across intervals like `QueryAgg::stddev_ms`.
    fn stddev_ms(&self) -> Option<f64> {
        if self.executions < 2.0 || self.sum_sq_us <= 0.0 {
            return None;
        }
        let mean_us = self.total_ms * 1_000.0 / self.executions;
        let variance = self.sum_sq_us / self.executions - mean_us * mean_us;
        Some(variance.max(0.0).sqrt() / 1_000.0)
    }
}

#[derive(Clone, Debug, Default)]
struct QueryAgg {
    query_id: String,
    plans: Vec<PlanAgg>,
}

impl QueryAgg {
    fn sum(&self, f: impl Fn(&PlanAgg) -> f64) -> f64 {
        self.plans.iter().map(f).sum()
    }

    fn executions(&self) -> f64 {
        self.sum(|p| p.executions)
    }

    fn total_ms(&self) -> f64 {
        self.sum(|p| p.total_ms)
    }

    fn dominant_plan(&self) -> Option<&PlanAgg> {
        self.plans.iter().max_by(|a, b| a.total_ms.total_cmp(&b.total_ms))
    }

    /// The plan the statement runs with now: the heaviest of the plans no other replaced. A plan is replaced when another
    /// first ran after it last ran (an index or a statistics update made SQL Server compile a new one), so a fix's old
    /// plan stops speaking for the statement. Plans that take turns (parameter sensitivity) are all current; the
    /// heaviest of them leads, as before. Without run times, the heaviest.
    fn current_plan(&self) -> Option<&PlanAgg> {
        let replaced = |p: &PlanAgg| {
            let Some(last) = p.last_seen.as_deref() else { return false };
            self.plans
                .iter()
                .any(|q| q.plan_id != p.plan_id && q.first_seen.as_deref().is_some_and(|first| first > last))
        };
        self.plans
            .iter()
            .filter(|p| !replaced(p))
            .max_by(|a, b| a.total_ms.total_cmp(&b.total_ms))
            .or_else(|| self.dominant_plan())
    }

    fn stddev_ms(&self) -> Option<f64> {
        let n = self.executions();
        if n < 2.0 {
            return None;
        }
        let mean_us = self.total_ms() * 1_000.0 / n;
        let variance = self.sum(|p| p.sum_sq_us) / n - mean_us * mean_us;
        Some(variance.max(0.0).sqrt() / 1_000.0)
    }

    fn min_ms(&self) -> Option<f64> {
        self.plans.iter().filter_map(|p| p.min_ms).reduce(f64::min)
    }

    fn first_seen(&self) -> Option<String> {
        self.plans.iter().filter_map(|p| p.first_seen.clone()).min()
    }

    fn last_seen(&self) -> Option<String> {
        self.plans.iter().filter_map(|p| p.last_seen.clone()).max()
    }
}

use crate::query_store::window_sql::window_sql;
#[cfg(test)]
use crate::{
    query_store::window_sql::CAPPED_END,
    sql_time::{utc_offset_iso, utc_second},
};

pub(crate) fn id_list(ids: &[String]) -> String {
    let numeric: Vec<&str> =
        ids.iter().map(String::as_str).filter(|id| id.chars().all(|c| c.is_ascii_digit())).collect();
    if numeric.is_empty() {
        return "NULL".into();
    }
    numeric.join(",")
}

pub(crate) struct WorkloadResult {
    pub(crate) workload: Workload,
    pub(crate) waits: Vec<WaitStat>,
    pub(crate) plans_parsed: usize,
    pub(crate) plan_errors: usize,
    pub(crate) coverage: Coverage,
}

/// At most this many of them join the workload beyond its top queries, the most run first.
const MAX_ASKERS: usize = 20;

/// What a login that can't read Query Store's statement waits needs.
pub(crate) fn waits_grant() -> String {
    culprit_core::t!("provider.mssql.query_waits.grant")
}

/// Query Store's statement waits read, without knowing they are captured: a missing category isn't a zero.
pub(crate) fn waits_not_captured() -> String {
    culprit_core::t!("provider.mssql.query_waits.not_captured")
}

/// Workload from Query Store over the scan's window. Query Store keeps runtime stats per interval (often an
/// hour), so the window reaches back to the start of the interval its edge falls in.
pub(crate) async fn from_query_store<R: crate::read::NativeRead>(
    reader: &mut crate::query_store::Reader<R>,
    options: &SnapshotOptions,
) -> culprit_core::Result<WorkloadResult> {
    let window_minutes = options.window_in_minutes();
    let rows = reader.summary(window_minutes).await?;
    let window = reader.read_rows(&window_sql(window_minutes)).await?.into_iter().next().unwrap_or_default();
    let mut by_query: BTreeMap<String, QueryAgg> = BTreeMap::new();
    for row in rows {
        let query_id = row.query_id.clone();
        let entry = by_query.entry(query_id.clone()).or_insert_with(|| QueryAgg { query_id, plans: Vec::new() });
        entry.plans.push(PlanAgg::from_query_store(row));
    }
    let all: Vec<QueryAgg> = by_query.into_values().collect();
    let total_time_ms: f64 = all.iter().map(QueryAgg::total_ms).sum();
    let total_cpu_ms: f64 = all.iter().map(|q| q.sum(|p| p.cpu_ms)).sum();
    let total_executions: f64 = all.iter().map(QueryAgg::executions).sum();

    let mut selected = select_top(&all, options.top_queries as usize);
    // The queries the optimizer asked for a missing index for, when SQL Server says which (2019+, Azure SQL), join the
    // top ones, so the missing-index finding can name them. A failure here leaves the finding without them.
    if let Ok(rows) = reader.read_rows(crate::query_store::catalog_sql::MISSING_INDEX_ASKERS_SQL).await {
        let askers: HashSet<String> = rows.iter().map(|r| r.string("query_id")).collect();
        let mut extra: Vec<&QueryAgg> = all
            .iter()
            .filter(|q| askers.contains(&q.query_id) && !selected.iter().any(|s| s.query_id == q.query_id))
            .collect();
        extra.sort_by(|a, b| b.executions().total_cmp(&a.executions()));
        selected.extend(extra.into_iter().take(MAX_ASKERS));
    }
    let ids: Vec<String> = selected.iter().map(|q| q.query_id.clone()).collect();

    let texts = reader
        .definitions(&ids, crate::query_store::definitions::Kind::Query, 256 * 1024)
        .await?
        .into_iter()
        .map(|row| (row.id.clone(), row))
        .collect::<HashMap<_, _>>();
    let plan_ids: Vec<String> = selected
        .iter()
        .take(options.plans as usize)
        .filter_map(|query| query.current_plan().map(|plan| plan.plan_id.clone()))
        .collect();
    let plans_xml = reader
        .definitions(&plan_ids, crate::query_store::definitions::Kind::Plan, 2 * 1024 * 1024)
        .await?
        .into_iter()
        .filter_map(|row| row.payload.text().map(|text| (row.id.clone(), text.to_owned())))
        .collect::<HashMap<_, _>>();

    let all_plan_ids: Vec<String> =
        selected.iter().flat_map(|query| query.plans.iter().map(|plan| plan.plan_id.clone())).collect();
    let mut coverage = Coverage::default();
    let wait_rows = coverage.read(
        &crate::sources::QUERY_WAITS,
        reader.waits(window_minutes, Some(&all_plan_ids)).await.map_err(|error| {
            if error.code == culprit_core::ErrorCode::PermissionDenied { error.with_hint(waits_grant()) } else { error }
        }),
    );
    // An empty category is zero only when the read succeeded and Query Store is capturing waits.
    // Older servers, disabled capture and failed reads must not manufacture zeroes.
    let capture_on = reader
        .read_rows("SELECT wait_stats_capture_mode FROM sys.database_query_store_options")
        .await
        .ok()
        .and_then(|rows| rows.first().and_then(|r| r.f64("wait_stats_capture_mode")))
        == Some(1.0);
    let waits_complete = wait_rows.is_some() && capture_on;
    if wait_rows.is_some() && !capture_on {
        coverage.partial(&crate::sources::QUERY_WAITS, waits_not_captured());
    }
    let mut plan_waits: HashMap<String, Vec<(String, f64)>> = HashMap::new();
    for row in wait_rows.into_iter().flatten() {
        if let Some(plan_id) = row.plan_id {
            plan_waits.entry(plan_id).or_default().push((row.category, row.total_ms as f64));
        }
    }
    let waits = match reader.waits(window_minutes, None).await {
        Ok(rows) => fold_waits(rows.into_iter().map(|row| (row.category, row.total_ms as f64))),
        Err(_) => Vec::new(),
    };

    let window_hours = window.f64("window_hours").unwrap_or(f64::from(window_minutes) / 60.0);
    let window_end = window.str("window_end");
    let trend_ids: Vec<String> = selected.iter().take(TREND_QUERIES).map(|q| q.query_id.clone()).collect();
    // Trends are a bonus: a failure here must not cost the scan its workload.
    let (timeline, mut trends) =
        match crate::timeline::collect(reader, window_minutes, window_hours, window_end.as_deref(), &trend_ids).await {
            Ok(Some((timeline, trends))) => (Some(timeline), trends),
            _ => (None, HashMap::new()),
        };

    let mut plans_parsed = 0;
    let mut plan_errors = 0;
    let mut queries: Vec<QueryStat> = selected
        .iter()
        .filter(|query| texts.get(&query.query_id).is_none_or(|definition| !definition.origin.is_culprit()))
        .map(|q| {
            let current = q.current_plan();
            let plan = current.and_then(|p| plans_xml.get(&p.plan_id)).and_then(|xml| match plan_xml::parse(xml) {
                Ok(plan) => {
                    plans_parsed += 1;
                    Some(plan)
                }
                Err(_) if plan_xml::is_empty(xml) => None,
                Err(_) => {
                    plan_errors += 1;
                    None
                }
            });
            let text = texts.get(&q.query_id);
            let query_waits = measured_query_waits(
                q.plans.iter().flat_map(|p| plan_waits.get(&p.plan_id).cloned().unwrap_or_default()),
                waits_complete,
            );
            let mut stat = to_query_stat(q, text, plan, query_waits);
            stat.plan_id = stat.plan.as_ref().and(current).map(|p| p.plan_id.clone());
            stat.trend = trends.remove(&q.query_id);
            stat
        })
        .collect();
    // Compiled values and SQL Server 2022 variants are a bonus as well.
    crate::spread::attach(reader, window_minutes, &mut queries).await;

    let workload = Workload {
        source_label: None,
        source_message: None,
        counter_set: None,
        history_inputs: Vec::new(),
        source: "Query Store".into(),
        kind: Some(culprit_core::WorkloadKind::QueryStore),
        windowed: true,
        window_start: window.str("window_start"),
        window_end,
        window_hours,
        total_time_ms,
        total_cpu_ms: Some(total_cpu_ms),
        total_executions,
        query_count: all.len() as f64,
        queries,
        timeline,
        untracked_time_ms: None,
        hourly_time_ms: None,
        min_logged_ms: None,
        log: None,
    };
    Ok(WorkloadResult { workload, waits, plans_parsed, plan_errors, coverage })
}

fn to_query_stat(
    q: &QueryAgg,
    text: Option<&crate::query_store::definitions::Definition>,
    plan: Option<culprit_core::Plan>,
    waits: Vec<WaitStat>,
) -> QueryStat {
    let executions = q.executions();
    let total_ms = q.total_ms();
    let mut variants: Vec<PlanVariant> = q
        .plans
        .iter()
        .map(|p| PlanVariant {
            id: p.plan_id.clone(),
            executions: p.executions,
            total_time_ms: p.total_ms,
            mean_time_ms: if p.executions > 0.0 { p.total_ms / p.executions } else { 0.0 },
            first_seen: p.first_seen.clone(),
            last_seen: p.last_seen.clone(),
            is_forced: p.forced,
            min_time_ms: p.min_ms,
            max_time_ms: Some(p.max_ms),
            stddev_time_ms: p.stddev_ms(),
            compiled: None,
            variant_query: None,
            cpu_time_ms: Some(p.cpu_ms),
            logical_reads: Some(p.reads),
        })
        .collect();
    variants.sort_by(|a, b| b.total_time_ms.total_cmp(&a.total_time_ms));
    let (parameters, sql) = split_parameters(text.and_then(|definition| definition.payload.text()).unwrap_or_default());
    QueryStat {
        plan_history: None,
        resolved_write_target: None,
        requests: None,
        id: q.query_id.clone(),
        query_hash: text
            .and_then(|definition| definition.query.as_ref().map(|query| format!("0x{}", query.query_hash))),
        text: sql,
        parameters,
        object_name: text.and_then(|definition| definition.object_name.clone()),
        executions,
        total_time_ms: total_ms,
        mean_time_ms: if executions > 0.0 { total_ms / executions } else { 0.0 },
        max_time_ms: Some(q.plans.iter().map(|p| p.max_ms).fold(0.0, f64::max)),
        stddev_time_ms: q.stddev_ms(),
        cpu_time_ms: Some(q.sum(|p| p.cpu_ms)),
        rows: Some(q.sum(|p| p.rows)),
        logical_reads: Some(q.sum(|p| p.reads)),
        physical_reads: Some(q.sum(|p| p.physical_reads)),
        temp_writes: q.plans.iter().map(|plan| plan.tempdb_pages).sum(),
        first_seen: q.first_seen(),
        last_seen: q.last_seen(),
        plans: variants,
        plan,
        waits,
        trend: None,
        spread: q.min_ms().map(|min_ms| culprit_core::Spread { min_ms: Some(min_ms), ..Default::default() }),
        costs: None,
        rows_affected: None,
        plan_id: None,
    }
}

/// Query Store keeps parameterized statements as `(@P1 nvarchar(4000),@P2 int)SELECT …`; split the declarations off.
pub(crate) fn split_parameters(text: &str) -> (Option<String>, String) {
    let trimmed = text.trim_start();
    if !trimmed.starts_with('(') || !trimmed.chars().take(200).any(|c| c == '@') {
        return (None, text.to_string());
    }
    let mut depth = 0usize;
    for (i, c) in trimmed.char_indices() {
        match c {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    let declarations = &trimmed[1..i];
                    if !declarations.trim_start().starts_with('@') {
                        return (None, text.to_string());
                    }
                    return (Some(declarations.trim().to_string()), trimmed[i + 1..].trim_start().to_string());
                }
            }
            _ => {}
        }
    }
    (None, text.to_string())
}

/// The heaviest queries by time, plus the heaviest by CPU, reads and executions so chatty and IO-bound queries are not crowded out.
fn select_top(all: &[QueryAgg], limit: usize) -> Vec<&QueryAgg> {
    if limit == 0 {
        return Vec::new();
    }
    let by = |f: &dyn Fn(&QueryAgg) -> f64| {
        let mut v: Vec<&QueryAgg> = all.iter().collect();
        v.sort_by(|a, b| f(b).total_cmp(&f(a)));
        v
    };
    let by_time = by(&|q| q.total_ms());
    let by_cpu = by(&|q| q.sum(|p| p.cpu_ms));
    let by_reads = by(&|q| q.sum(|p| p.reads));
    let by_executions = by(&|q| q.executions());

    let secondary = limit.div_ceil(6);
    let mut seen: HashSet<&str> = HashSet::new();
    let mut out: Vec<&QueryAgg> = Vec::new();
    let quotas: [(&[&QueryAgg], usize); 5] = [
        (&by_time, limit.div_ceil(2)),
        (&by_cpu, secondary),
        (&by_reads, secondary),
        (&by_executions, secondary),
        (&by_time, limit),
    ];
    for (list, quota) in quotas {
        for q in list.iter().take(quota) {
            if out.len() >= limit {
                break;
            }
            if seen.insert(q.query_id.as_str()) {
                out.push(q);
            }
        }
    }
    out.sort_by(|a, b| b.total_ms().total_cmp(&a.total_ms()));
    out
}

fn measured_query_waits(items: impl IntoIterator<Item = (String, f64)>, complete: bool) -> Vec<WaitStat> {
    let mut waits = fold_waits(items);
    if complete {
        for category in ["Lock", "Network"] {
            if !waits.iter().any(|w| w.category == category) {
                waits.push(WaitStat { category: category.into(), wait_type: None, wait_ms: 0.0, tasks: None });
            }
        }
    }
    waits
}

pub(crate) fn fold_waits(items: impl IntoIterator<Item = (String, f64)>) -> Vec<WaitStat> {
    let mut by_category: BTreeMap<&'static str, (f64, BTreeMap<String, f64>)> = BTreeMap::new();
    for (source, ms) in items {
        if IGNORED_WAIT_CATEGORIES.contains(&source.as_str()) || ms <= 0.0 {
            continue;
        }
        let entry = by_category.entry(wait_category(&source)).or_default();
        entry.0 += ms;
        *entry.1.entry(source).or_default() += ms;
    }
    let mut waits: Vec<WaitStat> = by_category
        .into_iter()
        .map(|(category, (ms, sources))| WaitStat {
            category: category.to_string(),
            wait_type: sources.into_iter().max_by(|a, b| a.1.total_cmp(&b.1)).map(|(s, _)| s),
            wait_ms: ms,
            tasks: None,
        })
        .collect();
    waits.sort_by(|a, b| b.wait_ms.total_cmp(&a.wait_ms));
    waits
}

/// Workload from the plan cache, for databases without Query Store. Covers only what is still cached.
pub(crate) async fn from_plan_cache(
    client: &mut impl NativeRead,
    options: &SnapshotOptions,
) -> culprit_core::Result<WorkloadResult> {
    let rows = crate::cached_runtime::summary(client).await?;
    let mut coverage = Coverage::default();
    if rows.iter().any(|summary| summary.counts.is_none()) {
        coverage.partial(
            &crate::sources::PLAN_CACHE,
            culprit_core::message!("provider.mssql.plan_cache.partial_statistics"),
        );
    }
    let rows: Vec<_> =
        rows.iter().filter_map(|summary| summary.counts.as_ref().map(|counts| (summary, counts))).collect();
    let all: Vec<QueryAgg> = rows
        .iter()
        .map(|(summary, counts)| QueryAgg {
            query_id: summary.query_hash.clone(),
            plans: vec![PlanAgg::from_cache_summary(summary, counts)],
        })
        .collect();
    let window_hours =
        rows.iter().map(|(summary, _)| summary.oldest_entry_age_seconds as f64 / 3600.0).fold(0.0, f64::max);
    let total_time_ms: f64 = all.iter().map(QueryAgg::total_ms).sum();
    let total_cpu_ms: f64 = all.iter().map(|q| q.sum(|p| p.cpu_ms)).sum();
    let total_executions: f64 = all.iter().map(QueryAgg::executions).sum();
    let plan_counts: HashMap<String, f64> =
        rows.iter().map(|(summary, counts)| (summary.query_hash.clone(), f64::from(counts.plans))).collect();

    let selected = select_top(&all, options.top_queries as usize);
    let hashes: Vec<String> = selected.iter().map(|q| q.query_id.clone()).collect();
    let details = crate::cached_statements::details(
        client,
        &hashes,
        options.plans as usize,
        &crate::read::ReadSession::application_budget(),
    )
    .await?;

    if hashes.iter().enumerate().any(|(rank, hash)| {
        details.get(hash).is_none_or(|detail| {
            detail.statement.text().is_none() || (rank < options.plans as usize && detail.plan.text().is_none())
        })
    }) {
        coverage
            .partial(&crate::sources::PLAN_CACHE, culprit_core::message!("provider.mssql.plan_cache.partial_payloads"));
    }
    let mut plans_parsed = 0;
    let mut plan_errors = 0;
    let queries = selected
        .iter()
        .enumerate()
        .map(|(rank, q)| {
            let detail = details.get(&q.query_id);
            let plan = if rank < options.plans as usize {
                detail.and_then(|d| d.plan.text()).and_then(|xml| match plan_xml::parse(xml) {
                    Ok(p) => {
                        plans_parsed += 1;
                        Some(p)
                    }
                    Err(_) if plan_xml::is_empty(xml) => None,
                    Err(_) => {
                        plan_errors += 1;
                        None
                    }
                })
            } else {
                None
            };
            let mut stat = to_query_stat(q, None, plan, Vec::new());
            let (parameters, sql) = split_parameters(detail.and_then(|d| d.statement.text()).unwrap_or_default());
            stat.query_hash = Some(q.query_id.clone());
            stat.parameters = parameters;
            stat.text = sql;
            stat.object_name = detail.and_then(|d| d.object_name.clone());
            stat.stddev_time_ms = None;
            stat.temp_writes = None;
            if plan_counts.get(&q.query_id).copied().unwrap_or(1.0) <= 1.0 {
                stat.plans.clear();
            }
            stat
        })
        .collect();

    // Each plan counts from its own compile: the time an hour of every entry's life, to weigh shares by (`share_of`).
    let window_end = rows.iter().map(|(summary, _)| summary.last_seen_utc_estimate.clone()).max();
    let end = window_end.as_deref().and_then(culprit_core::format::epoch_seconds);
    let hourly_time_ms: f64 = rows
        .iter()
        .filter_map(|(summary, counts)| {
            let first = culprit_core::format::epoch_seconds(&summary.first_seen_utc_estimate)?;
            let hours = ((end? - first) / 3600.0).max(1.0 / 60.0);
            Some(counts.duration_us as f64 / 1000.0 / hours)
        })
        .sum();
    let workload = Workload {
        source_label: None,
        source_message: None,
        counter_set: None,
        history_inputs: Vec::new(),
        source: "Plan cache".into(),
        kind: Some(culprit_core::WorkloadKind::PlanCache),
        windowed: false,
        window_start: rows.iter().map(|(summary, _)| summary.first_seen_utc_estimate.clone()).min(),
        window_end,
        window_hours,
        total_time_ms,
        total_cpu_ms: Some(total_cpu_ms),
        total_executions,
        query_count: all.len() as f64,
        queries,
        timeline: None,
        untracked_time_ms: None,
        hourly_time_ms: (hourly_time_ms > 0.0).then_some(hourly_time_ms),
        min_logged_ms: None,
        log: None,
    };
    Ok(WorkloadResult { workload, waits: Vec::new(), plans_parsed, plan_errors, coverage })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_waits_require_successful_collection_and_enabled_capture() {
        let waits = measured_query_waits([], true);
        assert_eq!(waits.len(), 2);
        assert!(waits.iter().all(|w| w.wait_ms == 0.0));
        assert!(measured_query_waits([], false).is_empty());
        let waits = measured_query_waits([("Lock".into(), 500.0)], true);
        assert_eq!(waits.iter().find(|w| w.category == "Lock").unwrap().wait_ms, 500.0);
        assert_eq!(waits.iter().find(|w| w.category == "Network").unwrap().wait_ms, 0.0);
        let partial = measured_query_waits([("Lock".into(), 500.0)], false);
        assert_eq!(partial.len(), 1, "an unrelated wait cannot establish a missing category");
    }

    fn agg(id: &str, ms: f64, cpu: f64, reads: f64, executions: f64) -> QueryAgg {
        QueryAgg {
            query_id: id.into(),
            plans: vec![PlanAgg {
                plan_id: id.into(),
                total_ms: ms,
                cpu_ms: cpu,
                reads,
                executions,
                ..PlanAgg::default()
            }],
        }
    }

    fn plan(id: &str, ms: f64, first: &str, last: &str) -> PlanAgg {
        PlanAgg {
            plan_id: id.into(),
            total_ms: ms,
            executions: 1.0,
            first_seen: Some(first.into()),
            last_seen: Some(last.into()),
            ..PlanAgg::default()
        }
    }

    #[test]
    fn the_plan_shown_is_the_one_still_in_use() {
        // The old plan ran most of the week; an index made SQL Server compile a new one, which has run since.
        let fixed = QueryAgg {
            query_id: "1".into(),
            plans: vec![
                plan("old", 9_000.0, "2026-10-01T00:00:00Z", "2026-10-06T10:00:00Z"),
                plan("new", 100.0, "2026-10-06T10:05:00Z", "2026-10-07T09:00:00Z"),
            ],
        };
        assert_eq!(fixed.dominant_plan().map(|p| p.plan_id.as_str()), Some("old"));
        assert_eq!(fixed.current_plan().map(|p| p.plan_id.as_str()), Some("new"));
        // Plans that take turns (parameter sensitivity) are both in use: the heaviest leads.
        let turns = QueryAgg {
            query_id: "2".into(),
            plans: vec![
                plan("a", 9_000.0, "2026-10-01T00:00:00Z", "2026-10-07T08:00:00Z"),
                plan("b", 100.0, "2026-10-02T00:00:00Z", "2026-10-07T09:00:00Z"),
            ],
        };
        assert_eq!(turns.current_plan().map(|p| p.plan_id.as_str()), Some("a"));
        // Without run times, the heaviest.
        let unknown = QueryAgg {
            query_id: "3".into(),
            plans: vec![
                PlanAgg { plan_id: "x".into(), total_ms: 5.0, ..PlanAgg::default() },
                PlanAgg { plan_id: "y".into(), total_ms: 50.0, ..PlanAgg::default() },
            ],
        };
        assert_eq!(unknown.current_plan().map(|p| p.plan_id.as_str()), Some("y"));
        let stat = to_query_stat(&fixed, None, None, Vec::new());
        assert_eq!(stat.plan_id, None, "no plan attached, no plan id");
    }

    #[test]
    fn the_plan_cache_answers_for_this_database_only() {
        assert!(crate::cached_runtime::summary_sql().contains(crate::cached_statements::DATABASE_PREDICATE));
    }

    #[test]
    fn the_open_interval_ends_now() {
        let sql = window_sql(60);
        assert!(sql.contains(&utc_offset_iso(CAPPED_END)), "{sql}");
        // The length is measured between the bounds the scan states, both rounded to the second alike.
        let length = format!(
            "DATEDIFF(second, {}, {}) / 3600.0 AS window_hours",
            utc_second("MIN(start_time)"),
            utc_second(CAPPED_END)
        );
        assert!(sql.contains(&length), "{sql}");
        assert!(sql.contains(&format!("{} AS window_start", utc_offset_iso("MIN(start_time)"))), "{sql}");
    }

    #[test]
    fn keeps_chatty_and_io_heavy_queries() {
        let mut all: Vec<QueryAgg> =
            (0..20).map(|i| agg(&format!("t{i}"), 1_000.0 - f64::from(i), 1.0, 1.0, 1.0)).collect();
        all.push(agg("chatty", 1.0, 1.0, 1.0, 1_000_000.0));
        all.push(agg("reader", 1.0, 1.0, 9e9, 1.0));
        let top = select_top(&all, 6);
        let ids: Vec<&str> = top.iter().map(|q| q.query_id.as_str()).collect();
        assert_eq!(ids.len(), 6);
        assert!(ids.contains(&"chatty"));
        assert!(ids.contains(&"reader"));
        assert_eq!(ids[0], "t0");
    }

    #[test]
    fn splits_parameter_declarations() {
        let (p, sql) = split_parameters("(@P1 nvarchar(4000),@P2 int)SELECT 1 WHERE a = @P1");
        assert_eq!(p.as_deref(), Some("@P1 nvarchar(4000),@P2 int"));
        assert_eq!(sql, "SELECT 1 WHERE a = @P1");
        let (p, sql) = split_parameters("(SELECT 1) UNION (SELECT 2)");
        assert!(p.is_none());
        assert_eq!(sql, "(SELECT 1) UNION (SELECT 2)");
    }

    #[test]
    fn statement_text_with_multibyte_characters_never_panics() {
        let wide = format!("({}é @P1 int)SELECT N'ünïcødé 🚀'", "x".repeat(198));
        let _ = split_parameters(&wide);
        let (p, sql) = split_parameters("(@P1 nvarchar(10))SELECT N'café 🚀' WHERE a = @P1");
        assert_eq!(p.as_deref(), Some("@P1 nvarchar(10)"));
        assert_eq!(sql, "SELECT N'café 🚀' WHERE a = @P1");
    }

    #[test]
    fn pooled_stddev_spans_plans() {
        let q = QueryAgg {
            query_id: "1".into(),
            plans: vec![
                PlanAgg { executions: 2.0, total_ms: 2.0, sum_sq_us: 2.0 * 1_000_000.0, ..PlanAgg::default() },
                PlanAgg { executions: 2.0, total_ms: 6.0, sum_sq_us: 2.0 * 9_000_000.0, ..PlanAgg::default() },
            ],
        };
        assert!((q.stddev_ms().unwrap() - 1.0).abs() < 1e-9);
    }

    #[test]
    fn folds_waits_into_categories() {
        let waits = fold_waits(vec![
            ("Buffer IO".to_string(), 10.0),
            ("Other Disk IO".to_string(), 5.0),
            ("CPU".to_string(), 20.0),
            ("Idle".to_string(), 1_000.0),
        ]);
        assert_eq!(waits[0].category, "CPU");
        assert_eq!(waits[1].category, "IO");
        assert_eq!(waits[1].wait_ms, 15.0);
        assert_eq!(waits[1].wait_type.as_deref(), Some("Buffer IO"));
        assert_eq!(waits.len(), 2);
    }
}
