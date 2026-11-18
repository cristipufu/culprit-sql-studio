//! Exact changed templates on one explicitly owned endpoint. This never enables an Azure-only collector route
//! on a boxed fixture, or promotes a source from Unqualified.
use super::*;
use crate::{
    query_store::costs,
    read::measurement::{MeasuredRead, assert_no_followup, shutdown_state},
    test_support::{OwnedSourceFixture, receipt_directory, save_cost_receipt},
};
use culprit_collector::capture::CollectionCancellation;
use culprit_core::telemetry::*;
use futures_util::FutureExt;
use serde_json::{Value, json};
use std::{collections::BTreeMap, panic::AssertUnwindSafe, path::Path, time::Duration};
use tokio::time::Instant;
mod lifecycle;
mod profile_failure;
mod setup;
use lifecycle::{finish_case, until_cancelled};

const CARDINALITIES: [usize; 3] = [1, 16, 64];
const SCENARIO_TIMEOUT: Duration = Duration::from_secs(120);
const ADMISSION_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_PROFILE_METADATA_QUERIES: u32 = 1;

#[tokio::test]
#[ignore = "requires explicit nonce-owned loopback SQL Server with existing READ_WRITE/ALL Query Store and fresh CULPRIT_NATIVE_COST_RECEIPT_DIR; select query_store::reader::native_compilation explicitly"]
async fn owned_compilation_stage_projection_preserves_query_identity_cardinality_and_settlement() {
    let directory = receipt_directory();
    let cancellation = CollectionCancellation::default();
    let (admission_expired, fixture) = finish_case(
        Instant::now() + ADMISSION_TIMEOUT,
        &cancellation,
        OwnedSourceFixture::open("culprit-compilation-source"),
    )
    .await;
    let fixture = fixture.expect("exact owned endpoint");
    assert!(!admission_expired, "fixture admission deadline; original admission has settled");
    let started = Instant::now();
    let (expired, outcome) = finish_case(
        Instant::now() + SCENARIO_TIMEOUT,
        &cancellation,
        AssertUnwindSafe(run(&fixture, &directory, &cancellation)).catch_unwind(),
    )
    .await;
    save_cost_receipt(&directory,"compilation-native-outcome",&json!({
        "scenario_completed":!expired && matches!(&outcome,Ok(Ok(()))),"source_shutdown":shutdown_state(fixture.pool.source_shutdown()),
        "case_deadline_exceeded":expired,"cancellation_requested":cancellation.is_cancelled(),
        "fixture_admission_client_elapsed_ms":fixture.admission_elapsed_ms,"full_experiment_client_elapsed_ms":started.elapsed().as_secs_f64()*1000.0,
        "fixture_cleanup":"native owners settle or fence before this outcome; supervisor owns final disposable database teardown including any owned probe table; no workload workers created",
        "production_admission":"unqualified","native_cpu_io_cost_qualified":false,
    })).expect("private native outcome receipt");
    match outcome {
        Ok(result) => result.expect("owned Query Store projection"),
        Err(panic) => std::panic::resume_unwind(panic),
    }
    assert!(!expired, "absolute owned Query Store experiment deadline; original owners have settled or fenced");
}

async fn run(fixture: &OwnedSourceFixture, directory: &Path, cancellation: &CollectionCancellation) -> Result<()> {
    let originals = setup::seed(fixture, directory, cancellation).await?;
    let ids = originals.keys().cloned().collect::<Vec<_>>();
    for count in CARDINALITIES {
        measured_selection(fixture, &ids[..count], &originals, directory, count, cancellation).await?;
        measured_snapshot(fixture, directory, count, cancellation).await?;
    }
    expired_deadline(fixture, &ids, directory, cancellation).await?;
    cancel_template(fixture, &ids, directory, cancellation).await?;
    Ok(())
}

fn budget() -> CollectionBudget {
    CollectionBudget {
        max_queries: MAX_PROFILE_METADATA_QUERIES + 1,
        statement_timeout_ms: 1500,
        total_timeout_ms: 5000,
        ..Default::default()
    }
}
async fn open(
    fixture: &OwnedSourceFixture,
    budget: CollectionBudget,
    cancellation: &CollectionCancellation,
    directory: &Path,
    receipt_name: &str,
) -> Result<Reader<MeasuredRead>> {
    let started = Instant::now();
    let io = match measured(fixture, budget, cancellation).await {
        Ok(io) => io,
        Err(error) => {
            profile_failure::save_open_failure(fixture, directory, receipt_name, started, &error);
            return Err(error);
        }
    };
    profile(io, cancellation, directory, receipt_name, started).await
}
async fn measured(
    fixture: &OwnedSourceFixture,
    budget: CollectionBudget,
    cancellation: &CollectionCancellation,
) -> Result<MeasuredRead> {
    if cancellation.is_cancelled() {
        return Err(CollectionStop::Cancelled.error());
    }
    // Opening owns preparation/login work. Let its existing bounded owner finish; profile then closes it
    // without admitting layout/source SQL if the case deadline was reached in the meantime.
    crate::test_support::measured_read(
        fixture.pool.clone(),
        fixture.port,
        fixture.password(),
        &fixture.database,
        budget,
    )
    .await
}
async fn profile(
    io: MeasuredRead,
    cancellation: &CollectionCancellation,
    directory: &Path,
    receipt_name: &str,
    started: Instant,
) -> Result<Reader<MeasuredRead>> {
    let mut reader = Reader::new(io);
    let result = until_cancelled(cancellation, reader.native_layout()).await.and_then(|layout| {
        if reader.io.report()["preparation"]["native_edition"].as_i64() != Some(layout.edition) {
            return Err(invalid_data());
        }
        reader.require_compilation_profile()
    });
    if let Err(error) = result {
        return Err(profile_failure::close_and_save(&mut reader, directory, receipt_name, started, error).await);
    }
    Ok(reader)
}

async fn measured_selection(
    fixture: &OwnedSourceFixture,
    ids: &[String],
    originals: &BTreeMap<String, NativeQueryIdentity>,
    directory: &Path,
    count: usize,
    cancellation: &CollectionCancellation,
) -> Result<()> {
    let started = Instant::now();
    let mut reader = open(fixture, budget(), cancellation, directory, &format!("compilation-selected-{count}")).await?;
    let result = until_cancelled(cancellation, reader.compilations(ids)).await;
    let mut receipt = settle(&mut reader, started, &result).await;
    receipt["native_observations"] = json!(
        result.as_ref().ok().map(|points| points.iter().map(costs::Compilation::observation).collect::<Vec<_>>())
    );
    save_cost_receipt(directory, &format!("compilation-selected-{count}"), &receipt)?;
    let points = result?;
    assert_eq!(points.len(), count, "all explicitly selected native entries must be present");
    for point in points {
        assert_eq!(originals.get(&point.identity.query_id), Some(&point.identity));
        let raw = point.observation();
        assert!(raw.is_valid(), "native NULL remains unknown; no phase is fabricated from another field");
        assert_eq!(point.record()?.key, raw.key()?);
    }
    verify_measurement(&reader, &receipt, &costs::compilation_sql());
    Ok(())
}

async fn measured_snapshot(
    fixture: &OwnedSourceFixture,
    directory: &Path,
    limit: usize,
    cancellation: &CollectionCancellation,
) -> Result<()> {
    let started = Instant::now();
    let mut reader = open(fixture, budget(), cancellation, directory, &format!("compilation-snapshot-{limit}")).await?;
    // The real streamed recipe asks for one overflow witness beyond the caller's retained population cap.
    let witness_limit = limit + 1;
    let rows = until_cancelled(
        cancellation,
        reader.read(&costs::compilation_snapshot_sql(), vec![Parameter::Integer(witness_limit as i64)], witness_limit),
    )
    .await;
    let mut receipt = settle(&mut reader, started, &rows).await;
    receipt["caller_row_cap"] = json!(limit);
    receipt["native_witness_limit"] = json!(witness_limit);
    receipt["observed_rows"] = json!(rows.as_ref().ok().map(|rows| rows.len()));
    receipt["limited"] = json!(rows.as_ref().ok().map(|rows| rows.len() > limit));
    save_cost_receipt(directory, &format!("compilation-snapshot-{limit}"), &receipt)?;
    let rows = rows?;
    assert!(rows.len() <= witness_limit, "the actual fixed snapshot projection honors its witness bound");
    if limit < CARDINALITIES[2] {
        assert_eq!(rows.len(), witness_limit, "the already-retained64 probes supply a real overflow witness");
    }
    let mut identities = std::collections::BTreeSet::new();
    for row in rows {
        let point = costs::Compilation::decode(&row)?.observation();
        assert!(point.is_valid());
        assert!(identities.insert(point.key()?));
    }
    verify_measurement(&reader, &receipt, &costs::compilation_snapshot_sql());
    Ok(())
}

async fn settle<T>(reader: &mut Reader<MeasuredRead>, started: Instant, result: &Result<T>) -> Value {
    let shutdown = reader.close().await;
    let quiet = reader.io.is_quiet().await;
    json!({"scope":reader.io.scope(),"source_queries":reader.queries(),"error":result.as_ref().err().map(|error|error.code),
        "shutdown":shutdown_state(shutdown),"independently_observed_quiet":quiet.as_ref().ok(),
        "full_profile_including_preparation_and_observer_client_elapsed_ms":started.elapsed().as_secs_f64()*1000.0,
        "accounting":reader.io.report(),"production_admission":"unqualified","native_cpu_io_cost_qualified":false,
        "native_coverage":"owned boxed endpoint only; Azure SQL collector route remains separately guarded and unqualified",
        "bounds":"TOP limits retained output, not server enumeration or CPU; cache state is uncontrolled",
        "phase_roles":"native layout preparation plus one changed compilation template; all phases remain charged, no subtraction or reset",
    })
}
fn verify_measurement(reader: &Reader<MeasuredRead>, receipt: &Value, sql: &str) {
    assert_eq!(reader.queries(), MAX_PROFILE_METADATA_QUERIES + 1);
    let metrics = reader.read_metrics().unwrap();
    let template = digest(sql.as_bytes());
    let source = metrics.templates.iter().find(|metric| metric.template_id == template).unwrap();
    assert_eq!(source.completed_tasks, 1, "exactly one unchanged actual source template");
    assert_eq!(source.failures, 0);
    for phase in &metrics.templates {
        assert!(
            [template.clone(), digest(crate::query_store::runtime::LAYOUT.as_bytes())].contains(&phase.template_id),
            "only actual native preparation and the changed template enter this bounded profile"
        );
    }
    assert_eq!(receipt["shutdown"], "quiescent");
    assert_eq!(receipt["independently_observed_quiet"], true);
    assert_eq!(
        receipt["accounting"]["measurement_complete"], true,
        "missing native counters cannot pass a numeric sample"
    );
    assert!(!metrics.has_unconfirmed_work());
}

async fn expired_deadline(
    fixture: &OwnedSourceFixture,
    ids: &[String],
    directory: &Path,
    cancellation: &CollectionCancellation,
) -> Result<()> {
    let mut reader = open(
        fixture,
        CollectionBudget { statement_timeout_ms: 1000, total_timeout_ms: 1000, ..budget() },
        cancellation,
        directory,
        "compilation-expired-deadline",
    )
    .await?;
    let metadata_queries = reader.queries();
    tokio::select! {
        _ = tokio::time::sleep(Duration::from_millis(1003)) => (),
        _ = cancellation.cancelled() => (),
    }
    let result = until_cancelled(cancellation, reader.compilations(ids)).await;
    let before = reader.queries();
    let refused = until_cancelled(cancellation, reader.compilations(ids)).await;
    let shutdown = reader.close().await;
    let quiet = reader.io.is_quiet().await;
    save_cost_receipt(
        directory,
        "compilation-expired-deadline",
        &json!({
            "error":result.as_ref().err().map(|error|error.code),"source_queries":reader.queries(),
            "followup_refused":refused.is_err(),"shutdown":shutdown_state(shutdown),"independently_observed_quiet":quiet.as_ref().ok(),
            "exact_template_cancellation_exercised":false,"accounting":reader.io.report(),"production_admission":"unqualified",
        }),
    )?;
    if cancellation.is_cancelled() {
        return Err(if shutdown == SourceShutdown::Unconfirmed {
            CollectionStop::CancellationUnconfirmed.error()
        } else {
            CollectionStop::Cancelled.error()
        });
    }
    assert!(matches!(result,Err(ref error) if CollectionStop::from_error(error)==CollectionStop::TimedOut));
    assert_eq!(before, metadata_queries, "no compilation SELECT may be admitted after deadline");
    assert!(refused.is_err() && reader.queries() == before, "the retired wrapper must refuse before native admission");
    assert_eq!(shutdown, SourceShutdown::Quiescent);
    assert!(quiet?);
    Ok(())
}

async fn cancel_template(
    fixture: &OwnedSourceFixture,
    ids: &[String],
    directory: &Path,
    cancellation: &CollectionCancellation,
) -> Result<()> {
    let template = digest(costs::compilation_sql().as_bytes());
    let started = Instant::now();
    let mut io = measured(fixture, budget(), cancellation).await?;
    let watch = io.watch_template(&template)?;
    let mut reader = profile(io, cancellation, directory, "compilation-template-cancellation", started).await?;
    let completed = {
        let work = reader.compilations(ids);
        tokio::pin!(work);
        let deadline = tokio::time::sleep(Duration::from_secs(2));
        tokio::pin!(deadline);
        loop {
            tokio::select! {
                result=&mut work=>break Some(result),
                _=cancellation.cancelled()=>break None,
                _=&mut deadline=>break None,
                _=tokio::time::sleep(Duration::from_millis(1))=>if watch.was_active(){break None;},
            }
        }
    };
    let dropped = completed.is_none();
    let shutdown = reader.close().await;
    let quiet = reader.io.is_quiet().await;
    save_cost_receipt(
        directory,
        "compilation-template-cancellation",
        &json!({
            "template_id":template,"exact_native_template_active_observed":watch.was_active(),"caller_dropped":dropped,
            "exact_template_cancellation_exercised":dropped && watch.was_active(),
            "completed_too_fast_to_cancel":completed.as_ref().is_some_and(|result|result.is_ok()),
            "error":completed.as_ref().and_then(|result|result.as_ref().err()).map(|error|error.code),
            "shutdown":shutdown_state(shutdown),"independently_observed_quiet":quiet.as_ref().ok(),
            "accounting":reader.io.report(),"production_admission":"unqualified",
            "limitation":"no cancellation qualification from a completed fast template, unrelated request or unobserved start; no SQL delays or operators are added",
        }),
    )?;
    if cancellation.is_cancelled() {
        return Err(if shutdown == SourceShutdown::Unconfirmed {
            CollectionStop::CancellationUnconfirmed.error()
        } else {
            CollectionStop::Cancelled.error()
        });
    }
    if let Some(result) = completed {
        result?;
    }
    assert!(quiet?);
    if shutdown == SourceShutdown::Unconfirmed {
        assert_no_followup(&mut reader.io).await;
    }
    Ok(())
}
