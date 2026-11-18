//! Qualification experiments execute the shipped performance statement on one explicitly owned endpoint.
//! They establish neither Hyperscale compatibility nor production cost approval.
use super::*;
use crate::{
    read::{
        NativeRead,
        measurement::{MeasuredRead, assert_no_followup, shutdown_state},
    },
    test_support::{
        OwnedSourceFixture,
        idle_sessions::{self, IdleSessions},
        owned_case::{finish_case, until_cancelled},
        receipt_directory, save_cost_receipt,
    },
};
use culprit_collector::capture::CollectionCancellation;
use culprit_core::telemetry::*;
use futures_util::FutureExt;
use serde_json::{Value, json};
use std::{panic::AssertUnwindSafe, path::Path, time::Duration};
use tokio::time::Instant;

mod limits;
mod matrix;

const ADMISSION_TIMEOUT: Duration = Duration::from_secs(10);
const CASE_TIMEOUT: Duration = Duration::from_secs(90);
const SOURCE_VALUE_BYTES: u32 = 64 * 1024;

#[derive(Clone, Copy)]
enum Experiment {
    Population,
    Limits,
}
impl Experiment {
    fn name(self) -> &'static str {
        match self {
            Self::Population => "performance-population",
            Self::Limits => "performance-limits",
        }
    }
}

#[tokio::test]
#[ignore = "requires nonce-owned loopback SQL Server, CULPRIT_SMOKE_OWNER/DATABASE, MSSQL source port/password and fresh CULPRIT_NATIVE_COST_RECEIPT_DIR; select performance::native_tests explicitly"]
async fn owned_performance_projection_keeps_six_existing_counters_and_one_exact_aggregate_under_unrelated_sessions() {
    experiment(Experiment::Population).await;
}

#[tokio::test]
#[ignore = "requires nonce-owned loopback SQL Server and fresh CULPRIT_NATIVE_COST_RECEIPT_DIR; unobserved active-template cancellation remains unqualified"]
async fn owned_performance_limits_deadline_and_caller_cancellation_preserve_the_original_native_owner() {
    experiment(Experiment::Limits).await;
}

async fn experiment(kind: Experiment) {
    let directory = receipt_directory();
    let cancellation = CollectionCancellation::default();
    let (expired, fixture) = finish_case(
        Instant::now() + ADMISSION_TIMEOUT,
        &cancellation,
        OwnedSourceFixture::open("culprit-performance-qualification"),
    )
    .await;
    let fixture = fixture.expect("full native nonce/database ownership");
    assert!(!expired, "owned admission settled after its deadline; no source statement is admitted");
    let mut population = IdleSessions::new();
    let mut observer = idle_sessions::observer(&fixture);
    let started = Instant::now();
    let (expired, outcome) = finish_case(
        Instant::now() + CASE_TIMEOUT,
        &cancellation,
        AssertUnwindSafe(async {
            match kind {
                Experiment::Population => {
                    Box::pin(matrix::run(&fixture, &mut population, &mut observer, &directory, &cancellation)).await
                }
                Experiment::Limits => Box::pin(limits::run(&fixture, &directory, &cancellation)).await,
            }
        })
        .catch_unwind(),
    )
    .await;
    // Native reader owners settle inside the body. Actor release is synchronous and precedes cleanup awaits.
    population.release();
    let cleanup = population.confirm_released(&mut observer).await;
    let observer_shutdown = observer.close().await;
    let source_shutdown = fixture.pool.source_shutdown();
    save_cost_receipt(
        &directory,
        &format!("{}-outcome", kind.name()),
        &json!({
            "scenario_completed": !expired && matches!(&outcome, Ok(Ok(()))),
            "case_deadline_reached": expired, "cancellation_requested": cancellation.is_cancelled(),
            "full_experiment_client_elapsed_ms": started.elapsed().as_secs_f64()*1000.0,
            "fixture_admission_client_elapsed_ms": fixture.admission_elapsed_ms,
            "population_released": cleanup.is_ok(), "population_setup_and_release": population.accounting(),
            "population_observer_metrics": observer.read_metrics(),
            "population_observer_shutdown": shutdown_state(observer_shutdown),
            "source_shutdown": shutdown_state(source_shutdown),
            "remaining_source_cases_withheld": source_shutdown == SourceShutdown::Unconfirmed,
            "production_admission": "unqualified", "native_cpu_io_cost_qualified": false,
            "cleanup": "actors released and native absence checked; the existing supervisor owns final exact nonce-labelled container removal",
        }),
    )
    .expect("private owned outcome receipt");
    match outcome {
        Err(panic) => std::panic::resume_unwind(panic),
        Ok(result) => result.expect("owned performance experiment; original native failure is retained"),
    }
    cleanup.expect("all unrelated actors were released");
    assert_eq!(observer_shutdown, SourceShutdown::Quiescent);
    assert!(!expired, "the case deadline signals cancellation and awaits original owners before failing");
}

fn source_budget() -> CollectionBudget {
    CollectionBudget {
        max_queries: 1,
        page_rows: MAX_COUNTERS as u32 + 1,
        max_snapshot_rows: MAX_COUNTERS as u32,
        page_bytes: SOURCE_VALUE_BYTES,
        total_bytes: SOURCE_VALUE_BYTES,
        statement_timeout_ms: 1500,
        total_timeout_ms: 5000,
        ..Default::default()
    }
}

async fn measured(
    fixture: &OwnedSourceFixture,
    budget: CollectionBudget,
    cancellation: &CollectionCancellation,
) -> Result<MeasuredRead> {
    if cancellation.is_cancelled() {
        return Err(CollectionStop::Cancelled.error());
    }
    // Preparation keeps its existing held connection and separate accounting. Never drop its owner on a case timer.
    let mut reader =
        MeasuredRead::open(fixture.pool.clone(), fixture.port, fixture.password(), &fixture.database, budget).await?;
    if cancellation.is_cancelled() {
        let shutdown = reader.close().await;
        return Err(if shutdown == SourceShutdown::Unconfirmed {
            CollectionStop::CancellationUnconfirmed.error()
        } else {
            CollectionStop::Cancelled.error()
        });
    }
    Ok(reader)
}

struct Settlement {
    shutdown: SourceShutdown,
    quiet: Result<bool>,
    queries: u32,
    metrics: Option<ReadWorkMetrics>,
    accounting: Value,
}
impl Settlement {
    fn receipt(&self) -> Value {
        json!({
            "source_queries": self.queries, "shutdown": shutdown_state(self.shutdown),
            "independently_observed_quiet": self.quiet.as_ref().ok(),
            "quiet_observation_error": self.quiet.as_ref().err().map(|error| error.code),
            "accounting": self.accounting,
            "production_admission": "unqualified", "native_cpu_io_cost_qualified": false,
            "measurement_scope": "one prepared source session and one unchanged performance statement; no database share, reset or accumulation-duration attribution",
            "measurement_limits": "preparation, observer and cleanup costs are separate; instrumentation phase elapsed includes observer scheduling while source read_metrics measures the reader; native integer CPU can round to zero; native session reads/writes are not storage bytes; sampled missing grants are unknown",
            "platform_limit": "only the owned endpoint edition/version, not Azure SQL or positive Hyperscale RBPEX qualification",
        })
    }
    fn confirm_quiet(&self) -> Result<()> {
        match &self.quiet {
            Ok(true) => Ok(()),
            Ok(false) => Err(CollectionStop::CancellationUnconfirmed.error()),
            Err(error) => Err(error.clone()),
        }
    }
}

async fn settle(reader: &mut MeasuredRead) -> Settlement {
    let shutdown = reader.close().await;
    let quiet = reader.is_quiet().await;
    let settlement = Settlement {
        shutdown,
        quiet,
        queries: reader.queries(),
        metrics: reader.read_metrics(),
        accounting: reader.report(),
    };
    assert_no_followup(reader).await;
    settlement
}

fn error_receipt(result: &Result<PerformanceRead>) -> Value {
    result
        .as_ref()
        .err()
        .map_or(Value::Null, |error| json!({"code": error.code, "stop": CollectionStop::from_error(error)}))
}
