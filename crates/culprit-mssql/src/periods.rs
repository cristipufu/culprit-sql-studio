//! "What changed" from Query Store: every query and plan in two periods, snapped to Query Store's intervals.
//!
//! Period boundaries move to the start of the interval they fall in, so an interval belongs wholly to one period.
//! Averages are weighted by executions; the fastest and slowest run come from the intervals' own min and max.

use std::collections::BTreeMap;

use culprit_core::TimelineSource;
use culprit_core::changes::{Period, PeriodData, PeriodStats, PlanPeriods, QueryPeriods};

use crate::workload::split_parameters;

use crate::query_store::period_sql::{coverage_sql, literal, per_plan_sql, totals_sql};

pub(crate) async fn collect<R: crate::read::NativeRead>(
    reader: &mut crate::query_store::Reader<R>,
    baseline: &Period,
    recent: &Period,
) -> culprit_core::Result<Option<PeriodData>> {
    let (Some(b0), Some(r0), Some(r1)) = (literal(&baseline.start), literal(&recent.start), literal(&recent.end))
    else {
        return Ok(None);
    };
    let mut data = PeriodData {
        source: TimelineSource::QueryStore,
        baseline: baseline.clone(),
        recent: recent.clone(),
        ..Default::default()
    };
    for r in reader.read_rows(&totals_sql(&b0, &r0, &r1)).await? {
        let stats = PeriodStats::new(r.f64_or_zero("executions"), r.f64_or_zero("total_ms"));
        let snapped =
            Period { start: r.str("period_start").unwrap_or_default(), end: r.str("period_end").unwrap_or_default() };
        if r.f64_or_zero("recent") >= 1.0 {
            data.recent_total = stats;
            data.recent = snapped;
        } else {
            data.baseline_total = stats;
            data.baseline = snapped;
        }
    }
    // Snapped periods can't run past what was asked for.
    data.recent.end = min_iso(&data.recent.end, &recent.end);
    data.baseline.end = min_iso(&data.baseline.end, &data.recent.start);
    if data.baseline.start.is_empty() {
        data.baseline = baseline.clone();
    }
    if data.recent.start.is_empty() {
        data.recent = recent.clone();
    }
    let coverage = reader.read_rows(&coverage_sql(&b0, &r0)).await?.into_iter().next();
    let covered = coverage.as_ref().and_then(|r| r.f64("covered")).unwrap_or(0.0);
    let wanted = coverage.as_ref().and_then(|r| r.f64("wanted")).unwrap_or(baseline.hours() * 3600.0);
    data.baseline_coverage = if wanted > 0.0 { (covered / wanted).clamp(0.0, 1.0) } else { 0.0 };

    let mut queries: BTreeMap<String, QueryPeriods> = BTreeMap::new();
    for r in reader.read_rows(&per_plan_sql(&b0, &r0, &r1)).await? {
        let query_id = r.string("query_id");
        let mut stats = PeriodStats::new(r.f64_or_zero("executions"), r.f64_or_zero("total_ms"));
        stats.min_ms = r.f64("min_ms");
        stats.max_ms = r.f64("max_ms");
        let is_recent = r.f64_or_zero("recent") >= 1.0;
        let q = queries.entry(query_id.clone()).or_insert_with(|| QueryPeriods { query_id, ..Default::default() });
        let side = if is_recent { &mut q.recent } else { &mut q.baseline };
        match side {
            Some(existing) => existing.add(&stats),
            None => *side = Some(stats.clone()),
        }
        let plan_id = r.string("plan_id");
        let plan = match q.plans.iter().position(|p| p.id == plan_id) {
            Some(i) => &mut q.plans[i],
            None => {
                q.plans.push(PlanPeriods { id: plan_id, ..Default::default() });
                q.plans.last_mut().expect("just pushed")
            }
        };
        if is_recent {
            plan.recent = Some(stats);
            plan.first_recent = r.str("first_start");
        } else {
            plan.baseline = Some(stats);
        }
    }

    let ids = queries.keys().cloned().collect::<Vec<_>>();
    for row in reader.definitions(&ids, crate::query_store::definitions::Kind::Query, 256 * 1024).await? {
        if let Some(query) = queries.get_mut(&row.id) {
            let (_, text) = split_parameters(row.payload.text().unwrap_or_default());
            query.text = text;
            query.object_name = row.object_name;
        }
    }
    data.queries = queries.into_values().collect();
    Ok(Some(data))
}

fn min_iso(a: &str, b: &str) -> String {
    match (culprit_core::changes::epoch(a), culprit_core::changes::epoch(b)) {
        (Some(x), Some(y)) if y < x => b.to_string(),
        (Some(_), _) => a.to_string(),
        _ => b.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timestamps_become_literals_or_nothing() {
        assert_eq!(literal("2026-10-11T14:05:00Z").as_deref(), Some("'2026-10-11T14:05:00+00:00'"));
        assert_eq!(literal("2026-10-11T16:05:00+02:00").as_deref(), Some("'2026-10-11T14:05:00+00:00'"));
        assert!(literal("2026-10-11'; DROP TABLE x; --").is_none());
    }
}
