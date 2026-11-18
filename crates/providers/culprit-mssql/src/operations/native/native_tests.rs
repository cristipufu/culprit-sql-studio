//! Execute the unchanged storage templates only on the existing explicit disposable endpoint.
use super::*;
use crate::{
    read::measurement::{MeasuredRead, shutdown_state},
    test_support::{
        OwnedSourceFixture,
        owned_case::{finish_case, until_cancelled},
        receipt_directory, save_cost_receipt,
    },
};
use culprit_collector::capture::CollectionCancellation;
use culprit_core::telemetry::*;
use futures_util::FutureExt;
use serde_json::json;
use std::{panic::AssertUnwindSafe, path::Path, time::Duration};
use tokio::time::Instant;

const CASE_TIMEOUT: Duration = Duration::from_secs(60);
const SOURCE_KINDS: [Kind; 3] = [Kind::Log, Kind::Filegroups, Kind::Temp];

#[tokio::test]
#[ignore = "requires the explicit nonce-owned loopback MSSQL fixture and fresh CULPRIT_NATIVE_COST_RECEIPT_DIR; select operations::native::native_tests explicitly; no DDL, DML or workload is created"]
async fn owned_exact_storage_queries_retain_integer_units_visible_files_and_confirmed_close() {
    let directory = receipt_directory();
    let cancellation = CollectionCancellation::default();
    let started = Instant::now();
    let (expired, outcome) = finish_case(
        started + CASE_TIMEOUT,
        &cancellation,
        AssertUnwindSafe(run(&directory, &cancellation)).catch_unwind(),
    )
    .await;
    save_cost_receipt(&directory, "storage-native-outcome", &json!({
        "case_deadline_exceeded":expired,"cancellation_requested":cancellation.is_cancelled(),
        "completed":!expired && matches!(&outcome,Ok(Ok(()))),
        "full_case_client_elapsed_ms":started.elapsed().as_secs_f64()*1000.0,
        "production_admission":"unqualified","native_cpu_io_cost_qualified":false,
        "cleanup":"same source owner closes or fences before outcome; supervisor owns disposable endpoint teardown; no schema objects or workload workers created",
    })).expect("private owned storage outcome receipt");
    match outcome {
        Ok(result) => result.expect("actual exact storage templates"),
        Err(panic) => std::panic::resume_unwind(panic),
    }
    assert!(!expired, "the original owner settled; a late result is not successful qualification");
}

async fn run(directory: &Path, cancellation: &CollectionCancellation) -> Result<()> {
    let fixture = OwnedSourceFixture::open("culprit-storage-source").await?;
    if cancellation.is_cancelled() {
        return Err(CollectionStop::Cancelled.error());
    }
    let budget = CollectionBudget {
        max_queries: SOURCE_KINDS.len() as u32,
        page_bytes: 8 * 1024 * 1024,
        statement_timeout_ms: 1500,
        total_timeout_ms: 10_000,
        ..Default::default()
    };
    let mut io =
        MeasuredRead::open(fixture.pool.clone(), fixture.port, fixture.password(), &fixture.database, budget).await?;
    let file_template = digest(format!("{} OPTION(MAXDOP 1)", Kind::Filegroups.sql()).as_bytes());
    let target = io.scope().target.clone();
    let started = Instant::now();
    let mut watch = None;
    let outcome = AssertUnwindSafe(async {
        watch = Some(io.watch_template(&file_template)?);
        until_cancelled(cancellation, project(&mut io, target)).await
    })
    .catch_unwind()
    .await;
    let shutdown = io.close().await;
    let quiet = io.is_quiet().await;
    let saved = save_cost_receipt(
        directory,
        "storage-native-queries",
        &json!({
            "scope":io.scope(),"accounting":io.report(),"source_queries":io.queries(),
            "observations":outcome.as_ref().ok().and_then(|result|result.as_ref().ok()),
            "error":outcome.as_ref().ok().and_then(|result|result.as_ref().err()).map(|error|error.code),
            "error_detail":outcome.as_ref().ok().and_then(|result|result.as_ref().err()).and_then(|error|error.detail),
            "panicked":outcome.is_err(),"shutdown":shutdown_state(shutdown),
            "independently_observed_quiet":quiet.as_ref().ok(),
            "file_template_active_observed":watch.as_ref().map(|watch|watch.was_active()),
            "exact_template_cancellation":"unexercised; fast completion is not an active cancellation proof",
            "full_source_including_observer_client_elapsed_ms":started.elapsed().as_secs_f64()*1000.0,
            "production_admission":"unqualified","native_cpu_io_cost_qualified":false,
            "limitations":"owned endpoint compatibility only; sampled grants are not peaks or no-grant proof; row caps bound retained output, not server enumeration; no Azure or Managed Instance qualification",
        }),
    );
    if let Err(error) = &saved {
        eprintln!("storage native receipt write failed: {:?}", error.code);
    }
    let result = match outcome {
        Ok(result) => result,
        Err(panic) => {
            std::panic::resume_unwind(panic);
        }
    };
    if shutdown != SourceShutdown::Quiescent {
        return Err(CollectionStop::CancellationUnconfirmed.error());
    }
    let read = result?;
    saved?;
    if !quiet? {
        return Err(CollectionStop::CancellationUnconfirmed.error());
    }
    if !read.is_valid() || io.queries() != SOURCE_KINDS.len() as u32 || io.report()["measurement_complete"] != true {
        return Err(invalid_data());
    }
    let metrics = io.read_metrics().ok_or_else(invalid_data)?;
    for kind in SOURCE_KINDS {
        let template = digest(format!("{} OPTION(MAXDOP 1)", kind.sql()).as_bytes());
        if !metrics
            .templates
            .iter()
            .any(|metric| metric.template_id == template && metric.completed_tasks == 1 && metric.failures == 0)
        {
            return Err(invalid_data());
        }
    }
    Ok(())
}

async fn project(io: &mut MeasuredRead, target: CollectionTarget) -> Result<StorageRead> {
    let started = std::time::SystemTime::now();
    let mut sources = Vec::new();
    let mut observations = Vec::new();
    for kind in SOURCE_KINDS {
        let rows = read(io, kind, MAX_OPERATION_ROWS).await?.observations(kind)?;
        if rows.iter().any(|row| !row.is_valid()) {
            return Err(invalid_data());
        }
        if kind == Kind::Filegroups
            && !rows.iter().any(|row| matches!(&row.fact, OperationalFact::MssqlDatabaseFile(_)))
        {
            return Err(invalid_data());
        }
        sources.push(StateSourceCoverage::collected(kind.source().name.into(), rows.len() as u32));
        observations.extend(rows);
    }
    let read = StorageRead {
        target,
        context: ObservationReadContext::collector(started, std::time::SystemTime::now())?,
        sources,
        observations,
        read_complete: true,
    };
    read.is_valid().then_some(read).ok_or_else(invalid_data)
}
