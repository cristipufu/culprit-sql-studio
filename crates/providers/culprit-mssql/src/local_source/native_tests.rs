//! Explicit nonce-owned, endpoint-only experiments. They never issue production cost approval.
use super::*;
use crate::{
    conn::SessionKind,
    read::{NativeRead, Parameter, ReadError, ReadSession, measurement::MeasuredRead},
    test_support::{OwnedSourceFixture, config, connect, receipt_directory, save_cost_receipt},
};
use culprit_core::Result;
use futures_util::FutureExt;
use serde_json::{Value, json};
use std::{panic::AssertUnwindSafe, time::Duration};
use tokio::time::Instant;

mod lifecycle;
mod matrix;
mod population;

const SOURCE_QUERIES: u32 = 6;
const SCENARIO_TIMEOUT: Duration = Duration::from_secs(75);
const LIFECYCLE_TIMEOUT: Duration = Duration::from_secs(30);
const CLEANUP_TIMEOUT: Duration = Duration::from_secs(5);

fn source_budget() -> CollectionBudget {
    CollectionBudget {
        max_queries: SOURCE_QUERIES,
        statement_timeout_ms: 1500,
        total_timeout_ms: 10_000,
        ..Default::default()
    }
}

async fn fixture() -> Result<OwnedSourceFixture> {
    tokio::time::timeout(Duration::from_secs(10), OwnedSourceFixture::open("culprit-context-source"))
        .await
        .map_err(|_| CollectionStop::TimedOut.error())?
}

async fn measured(fixture: &OwnedSourceFixture, budget: CollectionBudget) -> Result<MeasuredRead> {
    MeasuredRead::open(fixture.pool.clone(), fixture.port, fixture.password(), &fixture.database, budget).await
}

fn finish_case(
    outcome: std::result::Result<std::thread::Result<Result<()>>, tokio::time::error::Elapsed>,
    cleanup: Result<()>,
) {
    match outcome {
        Ok(Ok(result)) => result.expect("owned context experiment"),
        Ok(Err(panic)) => std::panic::resume_unwind(panic),
        Err(_) => panic!("absolute owned context scenario deadline"),
    }
    cleanup.expect("owned actors released and native owner closed");
}

fn experiment(scope: &NativeReadContext) -> Value {
    json!({
        "scope":scope,
        "profile":"existing-endpoint-local-source-context",
        "component":{"name":"culprit-mssql","version":env!("CARGO_PKG_VERSION")},
        "native_cpu_io_cost_qualified":false,
        "production_admission":"unqualified",
        "platform_coverage":"only the owned endpoint's observed native edition/version; no Azure SQL or Hyperscale qualification",
        "cache_state":"uncontrolled; prepared warm connection",
        "cpu_resolution":"native zero ticks do not establish zero CPU",
        "grants":"current-grant samples may miss short grants; unchanged-template actual-plan validation is separate",
    })
}

#[tokio::test]
#[ignore = "requires nonce-owned loopback SQL Server, CULPRIT_SMOKE_OWNER / DATABASE, MSSQL source port/password and CULPRIT_NATIVE_COST_RECEIPT_DIR; select local_source::native_tests explicitly"]
async fn owned_context_projection_keeps_one_row_and_no_grant_with_unrelated_idle_sessions() {
    let directory = receipt_directory();
    let fixture = fixture().await.expect("nonce-owned endpoint admission");
    let started = Instant::now();
    let mut population = population::IdleSessions::new();
    let mut observer = population::observer(&fixture);
    let outcome = tokio::time::timeout(
        SCENARIO_TIMEOUT,
        AssertUnwindSafe(matrix::run(&fixture, &mut population, &mut observer, &directory)).catch_unwind(),
    )
    .await;
    // Drop every held client synchronously before an awaited metadata check or cleanup deadline.
    population.release();
    let cleanup_started = Instant::now();
    let cleanup = tokio::time::timeout(CLEANUP_TIMEOUT, population.confirm_released(&mut observer)).await;
    let observer_shutdown = observer.close().await;
    let source_shutdown = fixture.pool.source_shutdown();
    save_cost_receipt(
        &directory,
        "context-matrix-cleanup",
        &json!({
            "idle_connections":population.len(),
            "scenario_completed":matches!(&outcome,Ok(Ok(Ok(())))),
            "native_release_confirmed":cleanup.as_ref().is_ok_and(|result|result.is_ok()),
            "cleanup_client_elapsed_ms":cleanup_started.elapsed().as_secs_f64()*1000.0,
            "observer_shutdown":crate::read::measurement::shutdown_state(observer_shutdown),
            "source_shutdown":crate::read::measurement::shutdown_state(source_shutdown),
            "fixture_admission_client_elapsed_ms":fixture.admission_elapsed_ms,
            "full_experiment_client_elapsed_ms":started.elapsed().as_secs_f64()*1000.0,
            "population_setup_and_release":population.accounting(),
            "population_observer_read_metrics":observer.read_metrics(),
            "outside_source_measurement":true,
        }),
    )
    .expect("private context cleanup receipt");
    let cleanup = cleanup.map_err(|_| CollectionStop::TimedOut.error()).and_then(|result| result).and_then(|()| {
        if observer_shutdown == SourceShutdown::Quiescent && source_shutdown == SourceShutdown::Quiescent {
            Ok(())
        } else {
            Err(CollectionStop::CancellationUnconfirmed.error())
        }
    });
    finish_case(outcome, cleanup);
}

#[tokio::test]
#[ignore = "requires nonce-owned loopback SQL Server and CULPRIT_NATIVE_COST_RECEIPT_DIR; exact fast-template cancellation may remain explicitly unexercised"]
async fn owned_context_deadline_and_caller_cancellation_preserve_native_settlement() {
    let directory = receipt_directory();
    let fixture = fixture().await.expect("nonce-owned endpoint admission");
    let outcome =
        tokio::time::timeout(LIFECYCLE_TIMEOUT, AssertUnwindSafe(lifecycle::run(&fixture, &directory)).catch_unwind())
            .await;
    save_cost_receipt(
        &directory,
        "context-lifecycle-outcome",
        &json!({
            "scenario_completed":matches!(&outcome,Ok(Ok(Ok(())))),
            "source_shutdown":crate::read::measurement::shutdown_state(fixture.pool.source_shutdown()),
            "fixture_admission_client_elapsed_ms":fixture.admission_elapsed_ms,
            "production_admission":"unqualified",
        }),
    )
    .expect("private lifecycle outcome");
    finish_case(outcome, Ok(()));
}
