//! Concurrent native writes are bounded fixture input, not a diagnostic query or a proposed production fix.
use super::{
    fixture::{self, Fixture, invalid, same_index_target},
    workload::{CompletedWorker, WRITERS_PER_PATTERN, require_abba_completions},
};
use culprit_core::{Result, telemetry::*};
use std::{
    collections::{BTreeMap, BTreeSet},
    time::Duration,
};

#[tokio::test]
#[ignore = "requires disposable loopback SQL Server and CULPRIT_MSSQL_SOURCE_TEST_PORT / CULPRIT_MSSQL_SOURCE_TEST_PASSWORD"]
async fn local_concurrent_insert_patterns_map_real_targets_without_blaming_an_unrelated_select() {
    fixture::run(|fixture| { Box::pin(async move {
        let before=fixture.indexes().await?;
        fixture.start_workload(false,true).await?;
        let mut participants=BTreeSet::new();
        let mut checked=0;let mut mapped=0;let mut plans=0;let mut observations=0;
        let mut resolutions=BTreeMap::<String,usize>::new();let mut relations=BTreeMap::<String,usize>::new();
        let mut native_queries=0;
        let mut linked_index_points=0;let mut unlinked_index_points=0;let mut point_for_cost=None;
        for _ in 0..32 {
            if fixture.workers_finished(){break;}
            let (capture,metrics)=fixture.inspect().await?;
            native_queries+=metrics.templates.iter().map(|task|task.completed_tasks as usize).sum::<usize>();
            if !capture.snapshot.is_valid() || metrics.has_unconfirmed_work(){return Err(invalid("native page capture was not qualified"));}
            for row in capture.snapshot.observations {
                observations+=1;
                *resolutions.entry(format!("{:?}",row.resolution)).or_default()+=1;
                if let Some(check)=&row.after_check {*relations.entry(format!("{:?}/{:?}",check.request,check.wait)).or_default()+=1;}
                let Some(target)=&row.target else {continue};
                if row.observed_address != Some(PageWaitAddress {
                    database_id: target.header.database_id, file_id: target.header.file_id, page_id: target.header.page_id,
                }) {return Err(invalid("native mapped header coordinates disagreed with the original decoded wait address"));}
                mapped+=1;
                let Some(object)=&target.object else {continue};
                if object.name!="page_fixture_hot" {continue;}
                if let Some(point @ PageIndexOperations::Observed { relation,counters,.. })=&row.index_operations {
                    if *relation==PageIndexOperationRelation::CheckedTarget {
                        if !row.index_operations.as_ref().unwrap().matches_target(target) {return Err(invalid("native index point escaped its checked object/partition mapping"));}
                        linked_index_points+=1;
                        if counters.counter(MssqlIndexCounter::PageLatchWaits).is_some_and(|count|count.parse::<u64>().is_ok_and(|count|count>0)) {
                            point_for_cost=Some(point.clone());
                        }
                    } else { unlinked_index_points+=1; }
                }
                let application=row.request.client.as_ref().and_then(|client|client.application.as_deref());
                if application==Some("page-fixture-unrelated-select") {return Err(invalid("unrelated SELECT was mapped to the writers' hot index"));}
                if row.statement_identity.is_none(){continue;}
                if !matches!(target.kind,PageWaitTargetKind::IndexLeaf|PageWaitTargetKind::IndexBranch) || object.index_name.as_deref()!=Some("PK_page_fixture_hot") {
                    eprintln!("native mapped header: {:?}, kind={:?}",target.header,target.kind);
                    return Err(invalid("native sequential-index target did not qualify as its actual rowstore structure"));
                }
                let DefinitionPayload::Available { text, .. } = &row.request.statement else { return Err(invalid("checked native statement was absent")); };
                let statement=text.to_ascii_lowercase();
                if !statement.contains("insert") || !statement.contains("page_fixture_hot") || !statement.contains("marker") {return Err(invalid("native checked SQL did not identify the affected write"));}
                // Native auto-parameterization can quote identifiers and replace values. The explicit
                // payload column still distinguishes these two known fixture writes.
                match application {
                    Some("page-fixture-write-a") if !statement.contains("payload")=>{participants.insert("a");},
                    Some("page-fixture-write-b") if statement.contains("payload")=>{participants.insert("b");},
                    _=>{
                        eprintln!("owned fixture unexpected checked association: application={application:?}, sql={:?}",statement.chars().take(2048).collect::<String>());
                        return Err(invalid("checked write pattern and original native application context disagreed"));
                    },
                }
                if row.before_check.as_ref().is_none_or(|check|check.request!=PageWaitRequestState::SameStatement)
                    || row.after_check.as_ref().is_none_or(|check|check.request!=PageWaitRequestState::SameStatement) {
                    return Err(invalid("native SQL link escaped its generation/statement checks"));
                }
                checked+=1;plans+=usize::from(row.plan_identity.is_some());
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let completed=fixture.finish_workload().await?;
        let after=fixture.indexes().await?;
        let inserts=completed.iter().filter(|row|row.pattern!=0).map(|row|row.inserts).sum::<u64>();
        let counters=delta(&before,&after,"page_fixture_hot")?;
        println!("{}",serde_json::json!({"fixture":"two-writes-and-unrelated-select","writers_per_pattern":WRITERS_PER_PATTERN,
            "completed_inserts":inserts,"native_index_inserts":counters.inserts,"native_latch_waits":counters.latch_waits,
            "native_accumulated_latch_ms":counters.latch_ms,"observed_wait_rows":observations,"mapped_current_headers":mapped,
            "checked_write_associations":checked,"native_plan_references":plans,"observed_patterns":participants,
            "header_resolutions":resolutions,"later_request_wait_checks":relations,"diagnostic_native_tasks":native_queries,
            "linked_index_counter_points":linked_index_points,"unlinked_index_counter_points":unlinked_index_points}));
        if counters.inserts!=inserts || counters.latch_waits==0 {return Err(invalid("bounded workload did not establish its native index insert/latch signal"));}
        if participants!=BTreeSet::from(["a","b"]) {return Err(invalid("bounded native run did not qualify both actual write patterns; no attribution proof"));}
        let point=point_for_cost.ok_or_else(||invalid("bounded native run did not retain a genuine positive checked index-counter point"))?;
        fixture.qualify_index_point(&point).await?;
        Ok(())
    }) }).await;
}

#[derive(Clone, Copy, Debug, Default)]
struct Counters {
    inserts: u64,
    latch_waits: u64,
    latch_ms: u64,
}
fn counter(row: &MssqlIndexOperations, kind: MssqlIndexCounter) -> Result<u64> {
    row.counter(kind)
        .and_then(|value| value.parse().ok())
        .ok_or_else(|| invalid("required native index counter is unknown"))
}
fn delta(
    before: &BTreeMap<String, MssqlIndexOperations>,
    after: &BTreeMap<String, MssqlIndexOperations>,
    name: &str,
) -> Result<Counters> {
    let before = before.get(name).ok_or_else(|| invalid("native index was absent before work"))?;
    let after = after.get(name).ok_or_else(|| invalid("native index was absent after work"))?;
    if !same_index_target(before, after) {
        return Err(invalid("native index generation changed during fixture measurement"));
    }
    let difference = |kind| {
        counter(after, kind)?
            .checked_sub(counter(before, kind)?)
            .ok_or_else(|| invalid("native index counter reset during fixture work"))
    };
    Ok(Counters {
        inserts: difference(MssqlIndexCounter::LeafInserts)?,
        latch_waits: difference(MssqlIndexCounter::PageLatchWaits)?,
        latch_ms: difference(MssqlIndexCounter::PageLatchWaitMs)?,
    })
}

#[tokio::test]
#[ignore = "requires disposable loopback SQL Server and CULPRIT_MSSQL_SOURCE_TEST_PORT / CULPRIT_MSSQL_SOURCE_TEST_PASSWORD"]
async fn local_controlled_write_routing_changes_target_contention_under_comparable_abba_load() {
    fixture::run(|fixture| {
        Box::pin(async move {
            let mut baseline = Vec::new();
            let mut separated = Vec::new();
            for (round, split) in [false, true, true, false].into_iter().enumerate() {
                let measured = measure(fixture, round, split).await?;
                if split {
                    separated.push(measured);
                } else {
                    baseline.push(measured);
                }
            }
            let comparison = compare(&baseline, &separated)?;
            println!(
                "{}",
                serde_json::json!({
                    "fixture":"ABBA-write-routing",
                    "native_combined_latch_ms_per_completed_write_baseline":comparison.baseline_latch_ms_per_write,
                    "native_combined_latch_ms_per_completed_write_separated":comparison.separated_latch_ms_per_write,
                    "whole_work_completed_inserts_per_second_baseline":comparison.baseline_writes_per_second,
                    "whole_work_completed_inserts_per_second_separated":comparison.separated_writes_per_second,
                    "interpretation":"owned intervention comparison, not a production promise or elapsed-time share",
                    "direct_holder_proven":false,"native_cost_qualified":false,
                })
            );
            comparison.require_improvement()
        })
    })
    .await;
}

async fn measure(fixture: &mut Fixture, round: usize, separated: bool) -> Result<RoundMeasurement> {
    fixture.reset().await?;
    fixture.begin_index_measurement().await?;
    let started = fixture.start_abba_workload(separated).await?;
    let completed = fixture.finish_workload().await?;
    let elapsed = started.elapsed();
    // Every endpoint, accounting and outcome assertion follows the same retained reader's awaited close.
    let window = fixture.finish_index_measurement().await?;
    let measured = completed_round(window.before(), window.after(), &completed, separated, elapsed)?;
    println!(
        "{}",
        serde_json::json!({"round":round,"pattern_b_separate_index":separated,"writers_per_pattern":WRITERS_PER_PATTERN,
        "completed_a":measured.writes/2,"completed_b":measured.writes/2,
        "expected_inserts_per_worker":super::workload::ABBA_INSERTS_PER_WORKER,
        "elapsed_seconds":elapsed.as_secs_f64(),"completed_inserts_per_second":measured.writes as f64/elapsed.as_secs_f64(),
        "hot_native_latch_waits":measured.hot.latch_waits,"hot_native_accumulated_latch_ms":measured.hot.latch_ms,
        "hot_native_inserts":measured.hot.inserts,
        "split_native_latch_waits":measured.split.latch_waits,"split_native_accumulated_latch_ms":measured.split.latch_ms,
        "split_native_inserts":measured.split.inserts,"combined_native_accumulated_latch_ms":measured.combined_latch_ms,
        "native_observed_from":window.context().observed_from,"native_observed_until":window.context().observed_until,
        "native_queries":window.queries(),"read_work":window.metrics().summary(None)?,
        "counter_reset":culprit_core::ResetMarker::Unknown,"native_cost_qualified":false})
    );
    Ok(measured)
}

#[derive(Clone, Copy, Debug)]
struct RoundMeasurement {
    hot: Counters,
    split: Counters,
    writes: u64,
    combined_latch_ms: u64,
    elapsed: Duration,
}

fn completed_round(
    before: &BTreeMap<String, MssqlIndexOperations>,
    after: &BTreeMap<String, MssqlIndexOperations>,
    completed: &[CompletedWorker],
    separated: bool,
    elapsed: Duration,
) -> Result<RoundMeasurement> {
    require_abba_completions(completed)?;
    let hot = delta(before, after, "page_fixture_hot")?;
    let split = delta(before, after, "page_fixture_split")?;
    let unrelated = delta(before, after, "page_fixture_select")?;
    let a = completed.iter().filter(|row| row.pattern == 1).map(|row| row.inserts).sum::<u64>();
    let b = completed.iter().filter(|row| row.pattern == 2).map(|row| row.inserts).sum::<u64>();
    if hot.inserts != if separated { a } else { a + b }
        || split.inserts != if separated { b } else { 0 }
        || unrelated.inserts != 0
        || elapsed.is_zero()
    {
        return Err(invalid("owned completed writes do not match the complete timed native counter populations"));
    }
    let combined_latch_ms = hot
        .latch_ms
        .checked_add(split.latch_ms)
        .ok_or_else(|| invalid("owned combined native latch work overflowed"))?;
    Ok(RoundMeasurement { hot, split, writes: a + b, combined_latch_ms, elapsed })
}

struct Comparison {
    baseline_latch_ms_per_write: f64,
    separated_latch_ms_per_write: f64,
    baseline_writes_per_second: f64,
    separated_writes_per_second: f64,
}

impl Comparison {
    fn require_improvement(&self) -> Result<()> {
        if self.separated_latch_ms_per_write >= self.baseline_latch_ms_per_write
            || self.separated_writes_per_second <= self.baseline_writes_per_second
        {
            return Err(invalid(
                "owned ABBA run did not reduce combined target contention and improve whole-work throughput",
            ));
        }
        Ok(())
    }
}

fn compare(baseline: &[RoundMeasurement], separated: &[RoundMeasurement]) -> Result<Comparison> {
    const ROUNDS_PER_ROUTE: usize = 2;
    if baseline.len() != ROUNDS_PER_ROUTE
        || separated.len() != ROUNDS_PER_ROUTE
        || baseline.iter().any(|round| round.combined_latch_ms == 0 || round.hot.latch_waits == 0)
    {
        return Err(invalid("owned ABBA comparison lacks its complete rounds or positive baseline contention"));
    }
    let totals = |rounds: &[RoundMeasurement]| {
        let writes = rounds.iter().map(|round| round.writes as f64).sum::<f64>();
        let latch_ms = rounds.iter().map(|round| round.combined_latch_ms as f64).sum::<f64>();
        let elapsed = rounds.iter().map(|round| round.elapsed.as_secs_f64()).sum::<f64>();
        (latch_ms / writes, writes / elapsed)
    };
    let (baseline_latch_ms_per_write, baseline_writes_per_second) = totals(baseline);
    let (separated_latch_ms_per_write, separated_writes_per_second) = totals(separated);
    Ok(Comparison {
        baseline_latch_ms_per_write,
        separated_latch_ms_per_write,
        baseline_writes_per_second,
        separated_writes_per_second,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{index_operations, resources::test_support::FakeRead};

    fn round(hot: u64, split: u64, seconds: u64) -> RoundMeasurement {
        RoundMeasurement {
            hot: Counters { inserts: 100, latch_waits: 1, latch_ms: hot },
            split: Counters { inserts: 100, latch_waits: u64::from(split > 0), latch_ms: split },
            writes: 200,
            combined_latch_ms: hot + split,
            elapsed: Duration::from_secs(seconds),
        }
    }

    #[test]
    fn comparable_completed_work_must_reduce_combined_latches_and_improve_whole_work_throughput() {
        let baseline = [round(1000, 0, 2); 2];
        let separated = [round(250, 250, 1); 2];
        let comparison = compare(&baseline, &separated).unwrap();
        assert_eq!(comparison.baseline_latch_ms_per_write, 5.0);
        assert_eq!(comparison.separated_latch_ms_per_write, 2.5);
        assert_eq!(comparison.baseline_writes_per_second, 100.0);
        assert_eq!(comparison.separated_writes_per_second, 200.0);
        comparison.require_improvement().unwrap();
    }

    #[test]
    fn lower_hot_index_waits_cannot_hide_pressure_transferred_to_the_second_index() {
        let baseline = [round(1000, 0, 2); 2];
        let separated = [round(100, 2000, 1); 2];
        assert!(separated[0].hot.latch_ms < baseline[0].hot.latch_ms);
        assert!(compare(&baseline, &separated).unwrap().require_improvement().is_err());
    }

    #[test]
    fn lower_combined_waits_cannot_hide_slower_whole_work_throughput() {
        let baseline = [round(1000, 0, 1); 2];
        let separated = [round(100, 100, 2); 2];
        assert!(compare(&baseline, &separated).unwrap().require_improvement().is_err());
    }

    #[test]
    fn healthy_or_missing_baseline_rounds_are_not_a_causal_success() {
        let separated = [round(100, 100, 1); 2];
        assert!(compare(&[round(0, 0, 2); 2], &separated).is_err());
        assert!(compare(&[round(1000, 0, 2)], &separated).is_err());
        assert!(compare(&[round(1000, 0, 2); 2], &separated[..1]).is_err());
    }

    async fn indexes() -> BTreeMap<String, MssqlIndexOperations> {
        let layout =
            index_operations::Layout::read(&mut FakeRead::new(vec![Ok(vec![index_operations::tests::layout_row(
                true,
                &[],
            )])]))
            .await
            .unwrap();
        ["page_fixture_hot", "page_fixture_split", "page_fixture_select"]
            .into_iter()
            .enumerate()
            .map(|(ordinal, name)| {
                let mut row = index_operations::decode(&index_operations::tests::row(), &layout).unwrap();
                row.identity.object_id += ordinal as u32;
                row.object = Some(name.into());
                row.counters.insert(MssqlIndexCounter::LeafInserts, Some("1".into()));
                row.counters.insert(MssqlIndexCounter::PageLatchWaits, Some("5".into()));
                row.counters.insert(MssqlIndexCounter::PageLatchWaitMs, Some("10".into()));
                (name.into(), row)
            })
            .collect()
    }

    fn completed() -> Vec<CompletedWorker> {
        (0..WRITERS_PER_PATTERN * 2)
            .map(|ordinal| CompletedWorker {
                pattern: (ordinal % 2 + 1) as u8,
                inserts: super::super::workload::ABBA_INSERTS_PER_WORKER as u64,
            })
            .collect()
    }

    #[tokio::test]
    async fn native_insert_accounting_rejects_partial_actors_unknown_counters_and_resets_after_close() {
        let before = indexes().await;
        let mut after = before.clone();
        let actors = completed();
        let writes = actors.iter().map(|worker| worker.inserts).sum::<u64>();
        after
            .get_mut("page_fixture_hot")
            .unwrap()
            .counters
            .insert(MssqlIndexCounter::LeafInserts, Some((writes + 1).to_string()));
        let measured = completed_round(&before, &after, &actors, false, Duration::from_secs(1)).unwrap();
        assert_eq!(measured.writes, writes);
        assert_eq!(measured.combined_latch_ms, 0, "measured zero stays zero, not a positive baseline");
        assert!(completed_round(&before, &after, &actors[..actors.len() - 1], false, Duration::from_secs(1)).is_err());
        assert!(completed_round(&before, &after, &actors, false, Duration::ZERO).is_err());
        for change in ["unknown", "reset", "lost-insert", "unrelated-insert"] {
            let mut changed = after.clone();
            match change {
                "unknown" => {
                    changed
                        .get_mut("page_fixture_hot")
                        .unwrap()
                        .counters
                        .insert(MssqlIndexCounter::PageLatchWaitMs, None);
                }
                "reset" => {
                    changed
                        .get_mut("page_fixture_hot")
                        .unwrap()
                        .counters
                        .insert(MssqlIndexCounter::PageLatchWaitMs, Some("9".into()));
                }
                "lost-insert" => {
                    changed
                        .get_mut("page_fixture_hot")
                        .unwrap()
                        .counters
                        .insert(MssqlIndexCounter::LeafInserts, Some(writes.to_string()));
                }
                _ => {
                    changed
                        .get_mut("page_fixture_select")
                        .unwrap()
                        .counters
                        .insert(MssqlIndexCounter::LeafInserts, Some("2".into()));
                }
            }
            assert!(completed_round(&before, &changed, &actors, false, Duration::from_secs(1)).is_err(), "{change}");
        }
    }
}
