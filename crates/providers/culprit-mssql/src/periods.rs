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
    let population = reader.population().await?;
    if population == crate::query_store::replicas::Population::AllGroups {
        return Err(reader.source_changed());
    }
    let mut data = PeriodData {
        source: TimelineSource::QueryStore,
        population: population.identity()?,
        baseline: baseline.clone(),
        recent: recent.clone(),
        ..Default::default()
    };
    for r in reader.read_rows(&totals_sql(&b0, &r0, &r1, population)).await? {
        if crate::query_store::replicas::Population::from_row(&r)? != population {
            return Err(reader.source_changed());
        }
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
    let coverage = reader.read_rows(&coverage_sql(&b0, &r0, population)?).await?.into_iter().next();
    let covered = coverage.as_ref().and_then(|r| r.f64("covered")).unwrap_or(0.0);
    let wanted = coverage.as_ref().and_then(|r| r.f64("wanted")).unwrap_or(baseline.hours() * 3600.0);
    data.baseline_coverage = if wanted > 0.0 { (covered / wanted).clamp(0.0, 1.0) } else { 0.0 };

    let mut queries: BTreeMap<String, QueryPeriods> = BTreeMap::new();
    for r in reader.read_rows(&per_plan_sql(&b0, &r0, &r1, population)).await? {
        if crate::query_store::replicas::Population::from_row(&r)? != population {
            return Err(reader.source_changed());
        }
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
    reader.verify_population().await?;
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
    use crate::{
        query_store::{Reader, replicas::tests::row as replica, window_sql::tests::layout_row},
        read::NativeRead,
        resources::test_support::{FakeRead, replace},
        value::{Rec, Val},
    };
    use culprit_core::telemetry::{CollectionStop, NativeIdentity, SourceShutdown};

    fn periods() -> (Period, Period) {
        (
            Period { start: "2026-11-01T00:00:00Z".into(), end: "2026-11-01T01:00:00Z".into() },
            Period { start: "2026-11-01T01:00:00Z".into(), end: "2026-11-01T02:00:00Z".into() },
        )
    }

    fn layout() -> Rec {
        replace(&layout_row(), &[("replicas", Val::Int(1))])
    }

    fn groups() -> Vec<Rec> {
        vec![replica(1, 1, None), replica(7, 2, Some("secondary"))]
    }

    fn native_row(group: i64) -> Rec {
        Rec::of(&[("replica_group_id", Val::Int(group))])
    }

    #[tokio::test]
    async fn exact_native_population_is_retained_even_when_both_periods_are_empty() {
        let mut reader = Reader::new(FakeRead::new(vec![
            Ok(vec![layout()]),
            Ok(groups()),
            Ok(vec![]),
            Ok(vec![Rec::of(&[("covered", Val::Int(3600)), ("wanted", Val::Int(3600))])]),
            Ok(vec![]),
            Ok(groups()),
        ]));
        reader.select_population(Some(&NativeIdentity::new("7"))).unwrap();
        let (baseline, recent) = periods();
        let result = collect(&mut reader, &baseline, &recent).await.unwrap().unwrap();
        assert_eq!(result.population, Some(NativeIdentity::new("7")));
        assert!(result.queries.is_empty());
        assert_eq!(result.baseline_coverage, 1.0);
        assert_eq!(reader.queries(), 6);
        assert_eq!(reader.close().await, SourceShutdown::Quiescent);
    }

    #[tokio::test]
    async fn substituted_period_rows_fence_the_reader_before_definitions_or_later_reads() {
        for wrong_totals in [false, true] {
            let mut replies = vec![Ok(vec![layout()]), Ok(groups())];
            if wrong_totals {
                replies.push(Ok(vec![native_row(1)]));
            } else {
                replies.extend([Ok(vec![]), Ok(vec![]), Ok(vec![native_row(1)])]);
            }
            let mut reader = Reader::new(FakeRead::new(replies));
            reader.select_population(Some(&NativeIdentity::new("7"))).unwrap();
            let (baseline, recent) = periods();
            let error = collect(&mut reader, &baseline, &recent).await.unwrap_err();
            assert_eq!(CollectionStop::from_error(&error), CollectionStop::SourceChanged);
            let queries = reader.queries();
            assert_eq!(queries, if wrong_totals { 3 } else { 5 });
            assert!(reader.read_rows("SELECT /* culprit */ 1").await.is_err());
            assert_eq!(reader.queries(), queries);
            assert_eq!(reader.close().await, SourceShutdown::Quiescent);
        }
    }

    #[tokio::test]
    async fn population_removal_at_final_verification_rejects_the_comparison() {
        let mut reader = Reader::new(FakeRead::new(vec![
            Ok(vec![layout()]),
            Ok(groups()),
            Ok(vec![]),
            Ok(vec![]),
            Ok(vec![]),
            Ok(vec![replica(1, 1, None)]),
        ]));
        reader.select_population(Some(&NativeIdentity::new("7"))).unwrap();
        let (baseline, recent) = periods();
        let error = collect(&mut reader, &baseline, &recent).await.unwrap_err();
        assert_eq!(CollectionStop::from_error(&error), CollectionStop::SourceChanged);
        assert_eq!(reader.queries(), 6);
        assert_eq!(reader.close().await, SourceShutdown::Quiescent);
    }

    #[test]
    fn timestamps_become_literals_or_nothing() {
        assert_eq!(literal("2026-10-11T14:05:00Z").as_deref(), Some("'2026-10-11T14:05:00+00:00'"));
        assert_eq!(literal("2026-10-11T16:05:00+02:00").as_deref(), Some("'2026-10-11T14:05:00+00:00'"));
        assert!(literal("2026-10-11'; DROP TABLE x; --").is_none());
    }
}
