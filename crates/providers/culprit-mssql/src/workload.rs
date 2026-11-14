use std::collections::{BTreeMap, HashMap, HashSet};

use culprit_core::{Coverage, PlanVariant, QueryStat, SnapshotOptions, StatementTemporaryWork, WaitStat, Workload};

use crate::plan_xml;
use crate::query_store::waits;
use crate::read::NativeRead;
use culprit_core::telemetry::{NativePlanIdentity, NativeQueryIdentity, WaitCaptureState};

#[cfg(test)]
#[path = "workload/identity_tests.rs"]
mod identity_tests;
#[cfg(test)]
#[path = "workload/native_tests.rs"]
mod native_tests;
#[cfg(test)]
#[path = "workload/parameter_tests.rs"]
mod parameter_tests;
#[cfg(test)]
#[path = "workload/variant_native_tests.rs"]
mod variant_native_tests;
#[cfg(test)]
#[path = "workload/window_tests.rs"]
mod window_tests;

/// The heaviest queries get a trend line; the rest would only grow the report.
const TREND_QUERIES: usize = 40;

#[derive(Clone, Debug, Default)]
struct PlanAgg {
    plan_id: String,
    identity: Option<NativePlanIdentity>,
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
    runtime_costs: Option<crate::query_store::runtime::ExecutionCosts>,
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
            identity: Some(value.plan_identity),
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
            runtime_costs: Some(value.costs),
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
    /// Query Store identity captured with the original runtime totals. Plan-cache summaries use their own guards.
    identity: Option<NativeQueryIdentity>,
    plans: Vec<PlanAgg>,
}

impl QueryAgg {
    fn runtime_costs(&self) -> Option<crate::query_store::runtime::ExecutionCosts> {
        if self.plans.iter().any(|plan| plan.runtime_costs.is_none()) {
            return None;
        }
        crate::query_store::runtime::ExecutionCosts::aggregate(
            self.plans.iter().filter_map(|plan| plan.runtime_costs.as_ref()),
        )
    }

    fn temporary_space_usage(&self) -> Option<StatementTemporaryWork> {
        if self.plans.is_empty() {
            return None;
        }
        let usage_pages_total = self.plans.iter().map(|plan| plan.tempdb_pages).sum::<Option<f64>>()?;
        Some(StatementTemporaryWork::SpaceUsage {
            source: "sys.query_store_runtime_stats.avg_tempdb_space_used".into(),
            bytes_per_page: crate::query_store::decode::SQL_SERVER_PAGE_BYTES,
            usage_pages_total,
        })
    }

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
    /// Earliest runtime identities remain private to this scan's later native attribution reads.
    pub(crate) query_identities: HashMap<String, NativeQueryIdentity>,
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
    let window = reader.window(window_minutes).await?;
    let mut by_query: BTreeMap<String, QueryAgg> = BTreeMap::new();
    for row in rows {
        let query_id = row.identity.query_id.clone();
        let entry = by_query.entry(query_id.clone()).or_insert_with(|| QueryAgg {
            query_id,
            identity: Some(row.identity.clone()),
            plans: Vec::new(),
        });
        if entry.identity.as_ref() != Some(&row.identity) || entry.plans.iter().any(|plan| plan.plan_id == row.plan_id)
        {
            return Err(reader.source_changed());
        }
        entry.plans.push(PlanAgg::from_query_store(row));
    }
    let all: Vec<QueryAgg> = by_query.into_values().collect();
    let total_time_ms: f64 = all.iter().map(QueryAgg::total_ms).sum();
    let total_cpu_ms: f64 = all.iter().map(|q| q.sum(|p| p.cpu_ms)).sum();
    let total_executions: f64 = all.iter().map(QueryAgg::executions).sum();
    let runtime_costs = all
        .iter()
        .filter_map(|query| query.runtime_costs().map(|costs| (query.query_id.clone(), costs)))
        .collect::<HashMap<_, _>>();
    let total_log_bytes = (runtime_costs.len() == all.len())
        .then(|| all.iter().map(|query| runtime_costs[&query.query_id].log_bytes).sum::<Option<f64>>())
        .flatten()
        .filter(|total| total.is_finite());
    let log_sum_unrepresentable =
        runtime_costs.values().all(|costs| costs.log_bytes.is_some()) && total_log_bytes.is_none();

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
    let query_identities = selected
        .iter()
        .filter_map(|query| query.identity.clone().map(|identity| (query.query_id.clone(), identity)))
        .collect::<HashMap<_, _>>();
    let plan_identities = selected
        .iter()
        .flat_map(|query| query.plans.iter())
        .filter_map(|plan| plan.identity.clone().map(|identity| (plan.plan_id.clone(), identity)))
        .collect::<HashMap<_, _>>();

    let texts = reader
        .definitions(&ids, crate::query_store::definitions::Kind::Query, 256 * 1024)
        .await?
        .into_iter()
        .map(|row| (row.id.clone(), row))
        .collect::<HashMap<_, _>>();
    if selected.iter().any(|query| {
        query.identity.is_none()
            || texts.get(&query.query_id).and_then(|definition| definition.query.as_ref()) != query.identity.as_ref()
    }) {
        return Err(reader.source_changed());
    }
    let plan_ids: Vec<String> = selected
        .iter()
        .take(options.plans as usize)
        .filter_map(|query| query.current_plan().map(|plan| plan.plan_id.clone()))
        .collect();
    let definitions =
        reader.definitions(&plan_ids, crate::query_store::definitions::Kind::Plan, 2 * 1024 * 1024).await?;
    if definitions.iter().any(|definition| {
        let expected = plan_identities.get(&definition.id);
        expected.is_none() || definition.plan.as_ref() != expected
    }) {
        return Err(reader.source_changed());
    }
    let plans_xml = definitions
        .into_iter()
        .filter_map(|row| row.payload.text().map(|text| (row.id.clone(), text.to_owned())))
        .collect::<HashMap<_, _>>();

    let all_plan_ids: Vec<String> =
        selected.iter().flat_map(|query| query.plans.iter().map(|plan| plan.plan_id.clone())).collect();
    let mut coverage = Coverage::default();
    if reader.variant_family_availability() == crate::query_store::variants_sql::Availability::Unavailable {
        coverage
            .partial(&crate::sources::QUERY_STORE, culprit_core::message!("provider.mssql.variant_family.unavailable"));
    }
    let wait_result = reader.waits(window_minutes, Some(&all_plan_ids)).await;
    if let Err(error) = &wait_result
        && crate::read::stops_remaining_reads(error)
    {
        return Err(error.clone());
    }
    let wait_rows = coverage.read(
        &crate::sources::QUERY_WAITS,
        wait_result.map_err(|error| {
            if error.code == culprit_core::ErrorCode::PermissionDenied { error.with_hint(waits_grant()) } else { error }
        }),
    );
    let known = waits::fold(wait_rows.as_deref().unwrap_or_default());
    if wait_rows.is_some() && window.capture != WaitCaptureState::Enabled {
        coverage.partial(&crate::sources::QUERY_WAITS, waits_not_captured());
    }
    if wait_rows.is_some() {
        // Query Store exposes its current mode, not capture continuity or independent wait cleanup over old intervals.
        coverage.partial(
            &crate::sources::QUERY_WAITS,
            culprit_core::message!("provider.mssql.query_waits.capture_history_unverified"),
        );
    }
    note_unknown_waits(&mut coverage, &known.unknown_categories);
    let mut plan_waits: HashMap<String, Vec<waits::Summary>> = HashMap::new();
    for row in wait_rows.into_iter().flatten() {
        if let Some(plan_id) = &row.plan_id {
            plan_waits.entry(plan_id.clone()).or_default().push(row);
        }
    }
    let waits = match reader.waits(window_minutes, None).await {
        Ok(rows) => {
            let folded = waits::fold(&rows);
            note_unknown_waits(&mut coverage, &folded.unknown_categories);
            folded.values
        }
        Err(error) if crate::read::stops_remaining_reads(&error) => return Err(error),
        Err(error) => {
            coverage.partial(&crate::sources::QUERY_WAITS, error.message_source());
            Vec::new()
        }
    };

    let window_hours = window.hours;
    let window_end = window.end.clone();
    let trend_ids: Vec<String> = selected.iter().take(TREND_QUERIES).map(|q| q.query_id.clone()).collect();
    // Trends are a bonus: a failure here must not cost the scan its workload.
    let (mut timeline, mut trends) =
        match crate::timeline::collect(reader, window_minutes, window_hours, window_end.as_deref(), &trend_ids).await {
            Ok(Some((timeline, trends))) => (Some(timeline), trends),
            Err(error) if crate::read::stops_remaining_reads(&error) => return Err(error),
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
            let query_waits =
                waits::fold(q.plans.iter().flat_map(|p| plan_waits.get(&p.plan_id).into_iter().flatten()));
            let mut stat = to_query_stat(q, text, plan, query_waits.values);
            stat.waits_coverage = Some(waits::coverage(false));
            stat.plan_id = stat.plan.as_ref().and(current).map(|p| p.plan_id.clone());
            stat.trend = trends.remove(&q.query_id);
            stat.costs = runtime_costs
                .get(&q.query_id)
                .map(|costs| costs.query_costs(total_log_bytes))
                .filter(|costs| !costs.is_empty());
            stat
        })
        .collect();
    // Compiled values and SQL Server 2022 variants are a bonus as well.
    crate::spread::attach(reader, &mut queries, &plan_identities, &mut coverage).await?;
    coverage.completed(&crate::sources::QUERY_COSTS, queries.len());
    if runtime_costs.len() != all.len()
        || log_sum_unrepresentable
        || runtime_costs.values().any(|costs| !costs.is_complete())
    {
        coverage.partial(
            &crate::sources::QUERY_COSTS,
            culprit_core::message!("provider.mssql.query_costs.partial_population"),
        );
    }
    verify_runtime_window(reader, &mut coverage).await?;
    let window = reader.window(window_minutes).await?;
    let window_hours = window.hours;
    let window_end = window.end.clone();
    if let Some(timeline) = &mut timeline {
        timeline.end = window_end.clone().ok_or_else(crate::read::invalid_data)?;
    }

    let workload = Workload {
        source_label: None,
        source_message: None,
        counter_set: None,
        history_inputs: Vec::new(),
        source: "Query Store".into(),
        kind: Some(culprit_core::WorkloadKind::QueryStore),
        windowed: true,
        window_start: window.start,
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
    Ok(WorkloadResult { workload, query_identities, waits, plans_parsed, plan_errors, coverage })
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
            cpu_time_ms: Some(p.cpu_ms),
            logical_reads: Some(p.reads),
        })
        .collect();
    variants.sort_by(|a, b| b.total_time_ms.total_cmp(&a.total_time_ms));
    let (parameters, sql) = split_parameters(text.and_then(|definition| definition.payload.text()).unwrap_or_default());
    QueryStat {
        variant_family: text.and_then(|definition| definition.variant_family.clone()),
        plan_history: None,
        resolved_write_target: None,
        requests: None,
        id: q.query_id.clone(),
        query_hash: text.and_then(|definition| {
            definition
                .query
                .as_ref()
                .and_then(|query| query.qualifiers.get("query_hash"))
                .map(|hash| format!("0x{hash}"))
        }),
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
        temporary_work: q.temporary_space_usage(),
        first_seen: q.first_seen(),
        last_seen: q.last_seen(),
        plans: variants,
        plan,
        waits,
        waits_coverage: None,
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

async fn verify_runtime_window<R: NativeRead>(
    reader: &mut crate::query_store::Reader<R>,
    coverage: &mut Coverage,
) -> culprit_core::Result<()> {
    use crate::query_store::window_sql::WindowVerification;
    match reader.verify_window().await? {
        WindowVerification::Unchanged => Ok(()),
        WindowVerification::WaitCaptureChanged => {
            coverage.partial(
                &crate::sources::QUERY_WAITS,
                culprit_core::telemetry::CollectionStop::SourceChanged.error().message_source(),
            );
            Ok(())
        }
        WindowVerification::RuntimePopulationChanged => {
            Err(culprit_core::telemetry::CollectionStop::SourceChanged.error())
        }
    }
}

fn note_unknown_waits(coverage: &mut Coverage, categories: &std::collections::BTreeSet<String>) {
    if !categories.is_empty() {
        coverage.partial(
            &crate::sources::QUERY_WAITS,
            culprit_core::message!(
                "provider.mssql.query_waits.unknown_categories",
                categories = categories.iter().map(String::as_str).collect::<Vec<_>>().join(", ")
            ),
        );
    }
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
            identity: None,
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
                detail.and_then(|d| d.plan.text().map(|xml| (d, xml))).and_then(|(detail, xml)| {
                    match plan_xml::parse_cached_statement(
                        xml,
                        &detail.context,
                        detail.module_parameters.as_ref(),
                        detail.statement.text(),
                    ) {
                        Ok(p) => {
                            plans_parsed += 1;
                            Some(p)
                        }
                        Err(_) if plan_xml::is_empty(xml) => None,
                        Err(_) => {
                            plan_errors += 1;
                            None
                        }
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
            stat.temporary_work = None;
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
    Ok(WorkloadResult {
        workload,
        query_identities: HashMap::new(),
        waits: Vec::new(),
        plans_parsed,
        plan_errors,
        coverage,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn agg(id: &str, ms: f64, cpu: f64, reads: f64, executions: f64) -> QueryAgg {
        QueryAgg {
            query_id: id.into(),
            identity: None,
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
    fn temporary_space_usage_requires_measured_pages_for_every_native_plan() {
        let mut query = QueryAgg::default();
        assert!(query.temporary_space_usage().is_none());
        query.plans.push(PlanAgg { tempdb_pages: Some(0.0), ..Default::default() });
        assert!(matches!(
            query.temporary_space_usage(),
            Some(StatementTemporaryWork::SpaceUsage { usage_pages_total: 0.0, bytes_per_page: 8192, .. })
        ));
        query.plans.push(PlanAgg::default());
        assert!(query.temporary_space_usage().is_none(), "an unmeasured plan is not zero temporary work");
        query.plans[1].tempdb_pages = Some(12.5);
        assert!(matches!(
            query.temporary_space_usage(),
            Some(StatementTemporaryWork::SpaceUsage { usage_pages_total: 12.5, .. })
        ));
    }

    #[test]
    fn the_plan_shown_is_the_one_still_in_use() {
        // The old plan ran most of the week; an index made SQL Server compile a new one, which has run since.
        let fixed = QueryAgg {
            query_id: "1".into(),
            identity: None,
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
            identity: None,
            plans: vec![
                plan("a", 9_000.0, "2026-10-01T00:00:00Z", "2026-10-07T08:00:00Z"),
                plan("b", 100.0, "2026-10-02T00:00:00Z", "2026-10-07T09:00:00Z"),
            ],
        };
        assert_eq!(turns.current_plan().map(|p| p.plan_id.as_str()), Some("a"));
        // Without run times, the heaviest.
        let unknown = QueryAgg {
            query_id: "3".into(),
            identity: None,
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
            identity: None,
            plans: vec![
                PlanAgg { executions: 2.0, total_ms: 2.0, sum_sq_us: 2.0 * 1_000_000.0, ..PlanAgg::default() },
                PlanAgg { executions: 2.0, total_ms: 6.0, sum_sq_us: 2.0 * 9_000_000.0, ..PlanAgg::default() },
            ],
        };
        assert!((q.stddev_ms().unwrap() - 1.0).abs() < 1e-9);
    }
}
