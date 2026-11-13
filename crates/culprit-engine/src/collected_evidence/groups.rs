use super::*;
use culprit_core::{Plan, QueryStat, Spread, collected_analysis::CollectedQueryInput, telemetry::*};
use std::collections::BTreeMap;

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
) -> Result<Option<CapturedTotals>> {
    if capture.gaps().iter().any(|gap| gap.stream.is_none_or(|stream| stream == TelemetryStream::QueryRuntime)) {
        return Ok(None);
    }
    let interval = &capture.source().query_history().ok_or_else(invalid)?.interval;
    let mut totals = CapturedTotals { executions: 0, time_ms: 0.0, cpu_ms: Some(0.0), queries: 0 };
    let mut queries = std::collections::BTreeSet::new();
    for record in capture.records() {
        let TelemetryRecord::QueryRuntime(row) = &record.row().record else { continue };
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
    limitations: &mut Vec<CollectedAnalysisLimitation>,
) -> Result<(Vec<QueryStat>, Vec<CollectedQueryInput>, bool)> {
    let mut groups = BTreeMap::<String, NativeGroup<'_>>::new();
    let mut partial = !capture.is_read_complete();
    for record in capture.records() {
        let TelemetryRecord::QueryRuntime(runtime) = &record.row().record else { continue };
        let joined = definitions.join(&runtime.key.references)?;
        let Some(query) = joined.query else {
            omit(record.row(), CollectedAnalysisLimit::MissingQueryDefinition, limitations, &mut partial);
            continue;
        };
        match inclusion(Some(query)) {
            QueryInclusion::Application => (),
            QueryInclusion::Unresolved => {
                omit(record.row(), CollectedAnalysisLimit::UnknownStatementOrigin, limitations, &mut partial);
                continue;
            }
            QueryInclusion::Excluded => continue,
        }
        if !query.sql.is_available() {
            omit(record.row(), CollectedAnalysisLimit::QueryTextUnavailable, limitations, &mut partial);
            continue;
        }
        if runtime.key.references.plan.is_some() && joined.plan.is_none() {
            limitations.push(limit(record.row().record_id.clone(), CollectedAnalysisLimit::MissingPlanDefinition));
        }
        let key = group_key(capture, &runtime.key)?;
        groups
            .entry(key)
            .or_insert_with(|| NativeGroup {
                rows: vec![],
                query,
                plan: joined.plan,
                references: runtime.key.references.clone(),
            })
            .rows
            .push((record.row(), runtime));
    }
    let mut queries = BTreeMap::new();
    let mut inputs = vec![];
    for (id, group) in groups {
        match query(capture, &id, &group, limitations)? {
            Some(query) => {
                inputs.push(CollectedQueryInput {
                    query_id: id.clone(),
                    references: group.references,
                    native_rows: group.rows.iter().map(|(_, row)| row.key.native.clone()).collect(),
                });
                queries.insert(id, query);
            }
            None => partial = true,
        }
    }
    for record in capture.records() {
        let TelemetryRecord::QueryWaits(wait) = &record.row().record else { continue };
        if inclusion(definitions.join(&wait.key.references)?.query) == QueryInclusion::Excluded {
            continue;
        }
        let key = group_key(capture, &wait.key)?;
        if !queries.contains_key(&key) {
            limitations.push(limit(record.row().record_id.clone(), CollectedAnalysisLimit::WaitWithoutRuntime));
            continue;
        }
        // WaitMeasurement preserves an opaque native category, not the vocabulary required by WaitStat. Its exact
        // value and unit remain in the authenticated native inputs until that semantic mapping is established.
        partial = true;
        limitations.push(limit(record.row().record_id.clone(), CollectedAnalysisLimit::MetricNotProjected));
    }
    Ok((queries.into_values().collect(), inputs, partial))
}

fn group_key(capture: &EvidenceCapture, key: &NativeMeasurementKey) -> Result<String> {
    capture.source().scoped_id("collected-query-group-v1", &(&key.references, &key.native.qualifiers))
}

fn query(
    capture: &EvidenceCapture,
    id: &str,
    group: &NativeGroup<'_>,
    limitations: &mut Vec<CollectedAnalysisLimitation>,
) -> Result<Option<QueryStat>> {
    let interval = &capture.source().query_history().ok_or_else(invalid)?.interval;
    let mut stats = culprit_core::PeriodStats::default();
    let mut executions = 0u64;
    let (mut cpu, mut rows) = (Some(0.0), Some(0.0));
    let (mut first, mut last) = (None, None);
    let (mut has_first, mut has_last, mut has_min, mut has_max) = (true, true, true, true);
    let mut stddev = None;
    for (export, row) in &group.rows {
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
            !matches!(metric.key, RuntimeMetricKey::Duration | RuntimeMetricKey::CpuTime | RuntimeMetricKey::Rows)
        }) {
            limitations.push(limit(export.record_id.clone(), CollectedAnalysisLimit::MetricNotProjected));
        }
    }
    if executions == 0 || !stats.total_ms.is_finite() {
        return Ok(None);
    }
    // The native standard deviation does not declare population/sample conventions. Multiple fragments therefore
    // preserve their individual spread in the native inputs instead of inventing a pooled variance.
    if group.rows.len() > 1 {
        if stddev.is_some() {
            limitations.push(limit(Some(id.into()), CollectedAnalysisLimit::MetricNotProjected));
        }
        stddev = None;
    }
    let plan = plan(group.plan, capture.source().target.engine(), limitations)?;
    let DefinitionPayload::Available { text, .. } = &group.query.sql else { return Err(invalid()) };
    Ok(Some(QueryStat {
        id: id.into(),
        text: text.clone(),
        query_hash: None,
        parameters: None,
        object_name: None,
        executions: stats.executions,
        total_time_ms: stats.total_ms,
        mean_time_ms: stats.mean_ms,
        max_time_ms: has_max.then_some(stats.max_ms).flatten(),
        stddev_time_ms: stddev,
        cpu_time_ms: cpu,
        rows,
        logical_reads: None,
        physical_reads: None,
        temp_writes: None,
        first_seen: has_first.then_some(first).flatten(),
        last_seen: has_last.then_some(last).flatten(),
        plans: vec![],
        plan,
        waits: vec![],
        waits_coverage: None,
        trend: None,
        spread: has_min.then(|| Spread { min_ms: stats.min_ms, ..Default::default() }),
        costs: None,
        rows_affected: None,
        plan_id: group.plan.map(|plan| plan.identity.plan_id.clone()),
        plan_history: None,
        requests: None,
        resolved_write_target: None,
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
        && let Ok(mut plan) = crate::plans::parse_plan(text)
        && plan.engine == engine
    {
        plan.facts_mut().constants_redacted = true;
        return Ok(Some(plan));
    }
    limitations.push(limit(Some(definition.id.clone()), CollectedAnalysisLimit::PlanUnavailable));
    Ok(None)
}

fn exact_count(value: &str) -> Option<u64> {
    value.parse::<u64>().ok().filter(|value| *value <= MAX_EXACT_COUNT)
}

fn milliseconds(value: f64, unit: RuntimeMetricUnit) -> Option<f64> {
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
