//! Concurrent native writes are bounded fixture input, not a diagnostic query or a proposed production fix.
use super::{
    fixture::{self, Fixture, invalid},
    workload::WRITERS_PER_PATTERN,
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
            "header_resolutions":resolutions,"later_request_wait_checks":relations,"diagnostic_native_tasks":native_queries}));
        if counters.inserts!=inserts || counters.latch_waits==0 {return Err(invalid("bounded workload did not establish its native index insert/latch signal"));}
        if participants!=BTreeSet::from(["a","b"]) {return Err(invalid("bounded native run did not qualify both actual write patterns; no attribution proof"));}
        Ok(())
    }) }).await;
}

#[derive(Debug, Default)]
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
    if before.identity != after.identity {
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
    fixture::run(|fixture| { Box::pin(async move{
        let mut baseline=Vec::new();let mut separated=Vec::new();
        for (round,split) in [false,true,true,false].into_iter().enumerate() {
            let measured=measure(fixture,round,split).await?;
            if split{separated.push(measured);}else{baseline.push(measured);}
        }
        let mean=|values:&[f64]|values.iter().sum::<f64>()/values.len()as f64;
        let base=mean(&baseline);let changed=mean(&separated);
        println!("{}",serde_json::json!({"fixture":"ABBA-write-routing","native_hot_index_mean_accumulated_latch_ms_per_insert_baseline":base,
            "native_hot_index_mean_accumulated_latch_ms_per_insert_separated":changed,
            "interpretation":"owned intervention comparison, not a production promise or elapsed-time share"}));
        if base<=0.0 || changed>=base {return Err(invalid("owned ABBA run did not establish the predicted target-contention reduction"));}
        Ok(())
    }) }).await;
}
async fn measure(fixture: &mut Fixture, round: usize, separated: bool) -> Result<f64> {
    fixture.reset().await?;
    let before = fixture.indexes().await?;
    let started = fixture.start_workload(separated, false).await?;
    let completed = fixture.finish_workload().await?;
    let elapsed = started.elapsed().as_secs_f64();
    let after = fixture.indexes().await?;
    let hot = delta(&before, &after, "page_fixture_hot")?;
    let split = delta(&before, &after, "page_fixture_split")?;
    let writes = completed.iter().map(|row| row.inserts).sum::<u64>();
    let a = completed.iter().filter(|row| row.pattern == 1).map(|row| row.inserts).sum::<u64>();
    let b = completed.iter().filter(|row| row.pattern == 2).map(|row| row.inserts).sum::<u64>();
    if hot.inserts != if separated { a } else { a + b } || split.inserts != if separated { b } else { 0 } {
        return Err(invalid("owned completed writes do not match native index-counter populations"));
    }
    println!(
        "{}",
        serde_json::json!({"round":round,"pattern_b_separate_index":separated,"writers_per_pattern":WRITERS_PER_PATTERN,
        "completed_a":a,"completed_b":b,"elapsed_seconds":elapsed,"completed_inserts_per_second":writes as f64/elapsed,
        "hot_native_latch_waits":hot.latch_waits,"hot_native_accumulated_latch_ms":hot.latch_ms,"hot_native_inserts":hot.inserts,
        "split_native_latch_waits":split.latch_waits,"split_native_accumulated_latch_ms":split.latch_ms,"split_native_inserts":split.inserts})
    );
    if hot.inserts == 0 {
        return Err(invalid("owned native run had no completed target inserts"));
    }
    Ok(hot.latch_ms as f64 / hot.inserts as f64)
}
