use super::*;
use culprit_core::{
    Plan, PlanHistory, PlanHistoryKind, PlanVariant, QueryStat, Spread, collected_analysis::CollectedQueryInput,
    telemetry::*,
};
use std::collections::{BTreeMap, BTreeSet};

pub(super) const MAX_EXACT_COUNT: u64 = 9_007_199_254_740_992;

pub(super) struct CapturedTotals {
    pub executions: u64,
    pub time_ms: f64,
    pub cpu_ms: Option<f64>,
    pub queries: usize,
}

/// Missing definitions do not erase their known time from the denominator. Missing native runtime itself cannot
/// establish any denominator; shares of the remaining rows would silently turn a lower bound into the whole.
pub(super) fn captured_totals(
    capture: &EvidenceCapture,
    definitions: &validation::Definitions<'_>,
    rows: &super::populations::NativeRows<'_>,
) -> Result<Option<CapturedTotals>> {
    if capture.gaps().iter().any(|gap| gap.stream.is_none_or(|stream| stream == TelemetryStream::QueryRuntime)) {
        return Ok(None);
    }
    let interval = &capture.source().query_history().ok_or_else(invalid)?.interval;
    let mut totals = CapturedTotals { executions: 0, time_ms: 0.0, cpu_ms: Some(0.0), queries: 0 };
    let mut queries = std::collections::BTreeSet::new();
    for &(_, row) in &rows.runtime {
        if inclusion(definitions.join(&row.key.references)?.query) == QueryInclusion::Excluded {
            continue;
        }
        let Some(n) = exact_count(&row.executions) else { return Ok(None) };
        let Some(count) = totals.executions.checked_add(n).filter(|total| *total <= MAX_EXACT_COUNT) else {
            return Ok(None);
        };
        totals.executions = count;
        if n == 0 {
            continue;
        }
        let Some(metric) = row.metric(&RuntimeMetricKey::Duration) else { return Ok(None) };
        let Some(mean) = milliseconds(metric.average, metric.unit) else { return Ok(None) };
        if !valid_bounds(row, interval)? {
            return Ok(None);
        }
        totals.time_ms += mean * n as f64;
        if !totals.time_ms.is_finite() {
            return Ok(None);
        }
        totals.cpu_ms = optional_total(totals.cpu_ms, row.metric(&RuntimeMetricKey::CpuTime), n, milliseconds);
        let Some(query_id) = row.key.references.query.as_ref().and_then(|query| query.definition_id.as_deref()) else {
            return Ok(None);
        };
        queries.insert(query_id);
    }
    totals.queries = queries.len();
    Ok(Some(totals))
}

struct NativeGroup<'a> {
    rows: Vec<(&'a ExportedRecord, &'a RuntimeMeasurement)>,
    query: &'a QueryDefinition,
    plan: Option<&'a PlanDefinition>,
    references: StatementReferences,
}

#[derive(PartialEq, Eq)]
enum QueryInclusion {
    Application,
    Unresolved,
    Excluded,
}

/// Unknown or missing definitions retain their measured cost in the denominator. Only a proven non-application
/// origin excludes a statement from both application analysis and its totals.
fn inclusion(query: Option<&QueryDefinition>) -> QueryInclusion {
    match query.map(|query| query.origin) {
        Some(StatementOrigin::Application) => QueryInclusion::Application,
        None | Some(StatementOrigin::Unknown) => QueryInclusion::Unresolved,
        Some(StatementOrigin::Culprit | StatementOrigin::CulpritMonitoring | StatementOrigin::EngineInternal) => {
            QueryInclusion::Excluded
        }
    }
}

pub(super) fn project(
    capture: &EvidenceCapture,
    definitions: &validation::Definitions<'_>,
    population: &CollectedQueryPopulation,
    rows: &super::populations::NativeRows<'_>,
    limitations: &mut Vec<CollectedAnalysisLimitation>,
) -> Result<(Vec<QueryStat>, Vec<CollectedQueryInput>, bool)> {
    let mut groups = BTreeMap::<String, BTreeMap<String, NativeGroup<'_>>>::new();
    let mut partial = !capture.is_read_complete();
    for &(record, runtime) in &rows.runtime {
        let joined = definitions.join(&runtime.key.references)?;
        let Some(query) = joined.query else {
            omit(record, CollectedAnalysisLimit::MissingQueryDefinition, limitations, &mut partial);
            continue;
        };
        match inclusion(Some(query)) {
            QueryInclusion::Application => (),
            QueryInclusion::Unresolved => {
                omit(record, CollectedAnalysisLimit::UnknownStatementOrigin, limitations, &mut partial);
                continue;
            }
            QueryInclusion::Excluded => continue,
        }
        if !query.sql.is_available() {
            omit(record, CollectedAnalysisLimit::QueryTextUnavailable, limitations, &mut partial);
            continue;
        }
        if runtime.key.references.plan.is_some() && joined.plan.is_none() {
            limitations.push(limit(record.record_id.clone(), CollectedAnalysisLimit::MissingPlanDefinition));
        }
        let key = group_key(capture, &runtime.key, population)?;
        groups
            .entry(key)
            .or_default()
            .entry(plan_key(capture, &runtime.key.references)?)
            .or_insert_with(|| NativeGroup {
                rows: vec![],
                query,
                plan: joined.plan,
                references: runtime.key.references.clone(),
            })
            .rows
            .push((record, runtime));
    }
    let mut queries = BTreeMap::new();
    let mut inputs = vec![];
    let mut runtime_plans = BTreeSet::new();
    for (id, plans) in groups {
        let groups = plans.values().collect::<Vec<_>>();
        match query(capture, &id, &groups, limitations)? {
            Some(query) => {
                inputs.push(CollectedQueryInput {
                    query_id: id.clone(),
                    references: common_references(&groups)?,
                    measurements: groups.iter().flat_map(|group| &group.rows).map(|(_, row)| row.key.clone()).collect(),
                });
                for (plan, group) in &plans {
                    if group.rows.iter().any(|(_, row)| exact_count(&row.executions).is_some_and(|count| count > 0)) {
                        runtime_plans.insert((id.clone(), plan.clone()));
                    }
                }
                queries.insert(id, query);
            }
            None => partial = true,
        }
    }
    let mut native_waits = BTreeMap::<String, Vec<super::waits::NativeWait<'_>>>::new();
    let mut wait_joins_complete = true;
    for &(record, wait) in &rows.waits {
        if inclusion(definitions.join(&wait.key.references)?.query) == QueryInclusion::Excluded {
            continue;
        }
        let key = group_key(capture, &wait.key, population)?;
        if !runtime_plans.contains(&(key.clone(), plan_key(capture, &wait.key.references)?)) {
            limitations.push(limit(record.record_id.clone(), CollectedAnalysisLimit::WaitWithoutRuntime));
            wait_joins_complete = false;
            partial = true;
            continue;
        }
        native_waits.entry(key).or_default().push((record, wait));
    }
    let waits = super::waits::Projection::new(capture, wait_joins_complete, limitations);
    for (id, query) in &mut queries {
        partial |= waits.apply(query, native_waits.get(id).map(Vec::as_slice).unwrap_or_default(), limitations);
    }
    Ok((queries.into_values().collect(), inputs, partial))
}

fn group_key(
    capture: &EvidenceCapture,
    key: &NativeMeasurementKey,
    population: &CollectedQueryPopulation,
) -> Result<String> {
    let interval = &capture.source().query_history().ok_or_else(invalid)?.interval;
    match &key.cohort {
        Some(cohort) => capture
            .source()
            .scoped_id("collected-query-cohort-v2", &(interval, population, &key.references.query, cohort)),
        None => capture
            .source()
            .scoped_id("collected-query-group-v3", &(interval, population, &key.references, &key.native.qualifiers)),
    }
}

fn plan_key(capture: &EvidenceCapture, references: &StatementReferences) -> Result<String> {
    capture.source().scoped_id("collected-runtime-plan-v1", references)
}

fn common_references(groups: &[&NativeGroup<'_>]) -> Result<StatementReferences> {
    let Some(first) = groups.first() else { return Err(invalid()) };
    if groups.iter().any(|group| group.references.query != first.references.query) {
        return Err(invalid());
    }
    Ok(StatementReferences {
        query: first.references.query.clone(),
        plan: (groups.len() == 1).then(|| first.references.plan.clone()).flatten(),
    })
}

struct RuntimeSummary {
    stats: culprit_core::PeriodStats,
    cpu: Option<f64>,
    rows: Option<f64>,
    first: Option<String>,
    last: Option<String>,
    stddev: Option<f64>,
}

type RuntimeRow<'a> = (&'a ExportedRecord, &'a RuntimeMeasurement);

fn query(
    capture: &EvidenceCapture,
    id: &str,
    groups: &[&NativeGroup<'_>],
    limitations: &mut Vec<CollectedAnalysisLimitation>,
) -> Result<Option<QueryStat>> {
    let Some(first) = groups.first() else { return Err(invalid()) };
    let fragments = groups.iter().flat_map(|group| group.rows.iter().copied()).collect::<Vec<_>>();
    let Some(total) = aggregate(capture, id, &fragments, limitations)? else { return Ok(None) };
    let mut variants = Vec::new();
    let mut captured: Option<(f64, String, Option<Plan>)> = None;
    for group in groups {
        let Some(definition) = group.plan else { continue };
        let Some(part) = aggregate(capture, &definition.id, &group.rows, limitations)? else { continue };
        let parsed = plan(Some(definition), capture.source().target.engine(), limitations)?;
        variants.push(PlanVariant {
            id: definition.id.clone(),
            executions: part.stats.executions,
            total_time_ms: part.stats.total_ms,
            mean_time_ms: part.stats.mean_ms,
            first_seen: part.first,
            last_seen: part.last,
            // Definitions do not capture a typed forcing observation. Missing source evidence is not false.
            is_forced: None,
            min_time_ms: part.stats.min_ms,
            max_time_ms: part.stats.max_ms,
            stddev_time_ms: part.stddev,
            compiled: parsed.as_ref().and_then(|plan| plan.facts().compiled_parameters.clone()),
            cpu_time_ms: part.cpu,
            logical_reads: None,
        });
        if captured.as_ref().is_none_or(|(time, id, _)| {
            part.stats.total_ms > *time || part.stats.total_ms == *time && definition.id < *id
        }) {
            captured = Some((part.stats.total_ms, definition.id.clone(), parsed));
        }
    }
    variants
        .sort_by(|left, right| right.total_time_ms.total_cmp(&left.total_time_ms).then_with(|| left.id.cmp(&right.id)));
    let (plan_id, plan) = captured.map(|(_, id, plan)| (Some(id), plan)).unwrap_or_default();
    let history = (!variants.is_empty()).then(|| PlanHistory {
        source: capture.source().source.clone(),
        kind: PlanHistoryKind::Workload,
        note: culprit_core::t!("engine.collected_analysis.plan_population"),
        captured_plan_id: plan_id.clone(),
        captured_plan: None,
        source_message: None,
        note_message: Some(culprit_core::message!("engine.collected_analysis.plan_population")),
    });
    let DefinitionPayload::Available { text, .. } = &first.query.sql else { return Err(invalid()) };
    let temporary_work = runtime_temporary_space(fragments.iter().map(|(_, row)| *row), &capture.source().source);
    if temporary_work.is_none()
        && fragments.iter().any(|(_, row)| row.metric(&RuntimeMetricKey::TemporaryStorage).is_some())
    {
        limitations.push(limit(Some(id.into()), CollectedAnalysisLimit::MetricNotProjected));
    }
    Ok(Some(QueryStat {
        id: id.into(),
        text: text.clone(),
        query_hash: None,
        parameters: None,
        object_name: None,
        executions: total.stats.executions,
        total_time_ms: total.stats.total_ms,
        mean_time_ms: total.stats.mean_ms,
        max_time_ms: total.stats.max_ms,
        stddev_time_ms: total.stddev,
        cpu_time_ms: total.cpu,
        rows: total.rows,
        logical_reads: None,
        physical_reads: None,
        temporary_work,
        first_seen: total.first,
        last_seen: total.last,
        plans: variants,
        plan,
        waits: vec![],
        waits_coverage: None,
        trend: None,
        spread: Some(Spread { min_ms: total.stats.min_ms, ..Default::default() }),
        costs: None,
        rows_affected: None,
        plan_id,
        plan_history: history,
        variant_family: first.query.variant_family.clone(),
        requests: None,
        resolved_write_target: None,
    }))
}

fn aggregate(
    capture: &EvidenceCapture,
    id: &str,
    fragments: &[RuntimeRow<'_>],
    limitations: &mut Vec<CollectedAnalysisLimitation>,
) -> Result<Option<RuntimeSummary>> {
    let interval = &capture.source().query_history().ok_or_else(invalid)?.interval;
    let mut stats = culprit_core::PeriodStats::default();
    let mut executions = 0u64;
    let (mut cpu, mut rows) = (Some(0.0), Some(0.0));
    let (mut first, mut last) = (None, None);
    let (mut has_first, mut has_last, mut has_min, mut has_max) = (true, true, true, true);
    let mut stddev = None;
    for (export, row) in fragments {
        let Some(count) = exact_count(&row.executions)
            .and_then(|count| executions.checked_add(count))
            .filter(|total| *total <= MAX_EXACT_COUNT)
        else {
            return omitted(export, CollectedAnalysisLimit::CounterPrecision, limitations);
        };
        let n = count - executions;
        executions = count;
        if n == 0 {
            continue;
        }
        let Some(duration) = row.metric(&RuntimeMetricKey::Duration) else {
            return omitted(export, CollectedAnalysisLimit::DurationUnavailable, limitations);
        };
        let Some(mean) = milliseconds(duration.average, duration.unit) else {
            return omitted(export, CollectedAnalysisLimit::DurationUnavailable, limitations);
        };
        if !valid_bounds(row, interval)? {
            return omitted(export, CollectedAnalysisLimit::RuntimeBounds, limitations);
        }
        let total = mean * n as f64;
        if !total.is_finite() {
            return omitted(export, CollectedAnalysisLimit::CounterPrecision, limitations);
        }
        let mut part = culprit_core::PeriodStats::new(n as f64, total);
        part.min_ms = duration.minimum.and_then(|value| milliseconds(value, duration.unit));
        part.max_ms = duration.maximum.and_then(|value| milliseconds(value, duration.unit));
        has_min &= part.min_ms.is_some();
        has_max &= part.max_ms.is_some();
        stats.add(&part);
        cpu = optional_total(cpu, row.metric(&RuntimeMetricKey::CpuTime), n, milliseconds);
        rows = optional_total(rows, row.metric(&RuntimeMetricKey::Rows), n, |value, unit| {
            (unit == RuntimeMetricUnit::Count).then_some(value)
        });
        if (row.metric(&RuntimeMetricKey::CpuTime).is_some() && cpu.is_none())
            || (row.metric(&RuntimeMetricKey::Rows).is_some() && rows.is_none())
        {
            limitations.push(limit(export.record_id.clone(), CollectedAnalysisLimit::MetricNotProjected));
        }
        stddev = duration.standard_deviation.and_then(|value| milliseconds(value, duration.unit));
        has_first &= row.first_execution_at.is_some();
        has_last &= row.last_execution_at.is_some();
        first = extreme(first, row.first_execution_at.as_deref(), true)?;
        last = extreme(last, row.last_execution_at.as_deref(), false)?;
        if row.metrics.iter().any(|metric| {
            !matches!(
                metric.key,
                RuntimeMetricKey::Duration
                    | RuntimeMetricKey::CpuTime
                    | RuntimeMetricKey::Rows
                    | RuntimeMetricKey::TemporaryStorage
            )
        }) {
            limitations.push(limit(export.record_id.clone(), CollectedAnalysisLimit::MetricNotProjected));
        }
    }
    if executions == 0 || !stats.total_ms.is_finite() {
        return Ok(None);
    }
    // The native standard deviation does not declare population/sample conventions. Multiple fragments therefore
    // preserve their individual spread in the native inputs instead of inventing a pooled variance.
    if fragments.len() > 1 {
        if stddev.is_some() {
            limitations.push(limit(Some(id.into()), CollectedAnalysisLimit::MetricNotProjected));
        }
        stddev = None;
    }
    if !has_min {
        stats.min_ms = None;
    }
    if !has_max {
        stats.max_ms = None;
    }
    Ok(Some(RuntimeSummary {
        stats,
        cpu,
        rows,
        first: has_first.then_some(first).flatten(),
        last: has_last.then_some(last).flatten(),
        stddev,
    }))
}

fn optional_total(
    total: Option<f64>,
    metric: Option<&RuntimeMetric>,
    n: u64,
    convert: impl Fn(f64, RuntimeMetricUnit) -> Option<f64>,
) -> Option<f64> {
    let value = total? + convert(metric?.average, metric?.unit)? * n as f64;
    value.is_finite().then_some(value)
}

fn valid_bounds(row: &RuntimeMeasurement, interval: &TelemetryInterval) -> Result<bool> {
    let (from, until) = (validation::time(&interval.from)?, validation::time(&interval.until)?);
    let first = row.first_execution_at.as_deref().map(validation::time).transpose()?;
    let last = row.last_execution_at.as_deref().map(validation::time).transpose()?;
    Ok(first.is_none_or(|at| from <= at && at <= until)
        && last.is_none_or(|at| from <= at && at <= until)
        && match (first, last) {
            (Some(first), Some(last)) => first <= last,
            _ => true,
        })
}

fn extreme(current: Option<String>, next: Option<&str>, minimum: bool) -> Result<Option<String>> {
    Ok(match (current, next) {
        (Some(current), Some(next)) => {
            if (validation::time(next)? < validation::time(&current)?) == minimum {
                Some(next.into())
            } else {
                Some(current)
            }
        }
        (current, None) => current,
        (None, Some(next)) => Some(next.into()),
    })
}

fn plan(
    definition: Option<&PlanDefinition>,
    engine: culprit_core::Engine,
    limitations: &mut Vec<CollectedAnalysisLimitation>,
) -> Result<Option<Plan>> {
    let Some(definition) = definition else { return Ok(None) };
    if let DefinitionPayload::Available { text, .. } = &definition.plan
        && let Ok(mut plan) = crate::plans::parse_native_plan(text, engine)
    {
        plan.facts_mut().constants_redacted = true;
        return Ok(Some(plan));
    }
    limitations.push(limit(Some(definition.id.clone()), CollectedAnalysisLimit::PlanUnavailable));
    Ok(None)
}

pub(super) fn exact_count(value: &str) -> Option<u64> {
    value.parse::<u64>().ok().filter(|value| *value <= MAX_EXACT_COUNT)
}

pub(super) fn milliseconds(value: f64, unit: RuntimeMetricUnit) -> Option<f64> {
    let scale = match unit {
        RuntimeMetricUnit::Picoseconds => 1e-9,
        RuntimeMetricUnit::Nanoseconds => 1e-6,
        RuntimeMetricUnit::Microseconds => 1e-3,
        RuntimeMetricUnit::Milliseconds => 1.0,
        RuntimeMetricUnit::Seconds => 1e3,
        _ => return None,
    };
    let converted = value * scale;
    (converted.is_finite() && (value == 0.0 || converted > 0.0)).then_some(converted)
}

fn omitted<T>(
    row: &ExportedRecord,
    reason: CollectedAnalysisLimit,
    limitations: &mut Vec<CollectedAnalysisLimitation>,
) -> Result<Option<T>> {
    limitations.push(limit(row.record_id.clone(), reason));
    Ok(None)
}

fn omit(
    row: &ExportedRecord,
    reason: CollectedAnalysisLimit,
    limitations: &mut Vec<CollectedAnalysisLimitation>,
    partial: &mut bool,
) {
    limitations.push(limit(row.record_id.clone(), reason));
    *partial = true;
}
