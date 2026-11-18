//! An owned metadata experiment, not a runtime-population fallback or a source-cost qualification.
use super::*;
use crate::{
    read::measurement::shutdown_state,
    test_support::{
        OwnedSourceFixture,
        owned_case::{finish_case, until_cancelled},
        query_store_probes::{self, ProbePolicy},
        receipt_directory, save_cost_receipt,
    },
};
use culprit_collector::capture::CollectionCancellation;
use culprit_core::telemetry::*;
use futures_util::FutureExt;
use serde_json::json;
use std::{panic::AssertUnwindSafe, path::Path, time::Duration};
use tokio::time::Instant;

mod metadata;
#[cfg(test)]
mod tests;
use metadata::{NativeProfilePoint, RuntimeFragment};

const ADMISSION_TIMEOUT: Duration = Duration::from_secs(10);
const EXPERIMENT_TIMEOUT: Duration = Duration::from_secs(60);
const MAX_CATALOG_ROWS: usize = super::super::replicas::MAX_REPLICA_GROUP_ROWS + 1;
const RUNTIME_SAMPLE_ROWS: usize = 3;
const SOURCE_DISPATCHES: u32 = 7;

#[tokio::test]
#[ignore = "requires fresh explicit nonce-owned loopback SQL Server READ_WRITE/ALL Query Store and CULPRIT_NATIVE_COST_RECEIPT_DIR; select query_store::reader::native_population_profile explicitly; never enable trace flags"]
async fn owned_replica_capture_metadata_preserves_before_after_native_flags_and_raw_group_values() {
    let directory = receipt_directory();
    let cancellation = CollectionCancellation::default();
    let (admission_expired, fixture) = finish_case(
        Instant::now() + ADMISSION_TIMEOUT,
        &cancellation,
        OwnedSourceFixture::open("culprit-population-profile"),
    )
    .await;
    let fixture = fixture.expect("exact nonce-owned loopback database");
    assert!(!admission_expired, "fixture admission settled after its deadline");
    let started = Instant::now();
    let (expired, outcome) = finish_case(
        started + EXPERIMENT_TIMEOUT,
        &cancellation,
        AssertUnwindSafe(run(&fixture, &directory, &cancellation)).catch_unwind(),
    )
    .await;
    save_cost_receipt(&directory, "population-profile-outcome", &json!({
        "case_deadline_exceeded":expired,"cancellation_requested":cancellation.is_cancelled(),
        "completed":!expired && matches!(&outcome,Ok(Ok(()))),
        "source_shutdown":shutdown_state(fixture.pool.source_shutdown()),
        "fixture_admission_client_elapsed_ms":fixture.admission_elapsed_ms,
        "full_experiment_client_elapsed_ms":started.elapsed().as_secs_f64()*1000.0,
        "production_admission":"unqualified","native_cpu_io_cost_qualified":false,
        "cleanup":"original read owners settle or fence before this outcome; supervisor owns final nonce database and probe table teardown; no workers or instrumentation changes",
    })).expect("private population-profile outcome receipt");
    match outcome {
        Ok(result) => result.expect("bounded native metadata experiment"),
        Err(panic) => std::panic::resume_unwind(panic),
    }
    assert!(!expired, "original owners settled after the absolute experiment deadline");
}

async fn run(fixture: &OwnedSourceFixture, directory: &Path, cancellation: &CollectionCancellation) -> Result<()> {
    let originals = query_store_probes::seed(
        fixture,
        directory,
        cancellation,
        64,
        ProbePolicy::Collector,
        "population-profile-setup",
    )
    .await?;
    let original = originals.values().next().ok_or_else(invalid_data)?;
    let started = Instant::now();
    // Preparation and observer work remain visible in the original measurement report. This budget covers
    // exactly seven single-result-set dispatches, each containing one explicitly accounted statement.
    let budget = CollectionBudget {
        max_queries: SOURCE_DISPATCHES,
        statement_timeout_ms: 1500,
        total_timeout_ms: 10_000,
        ..Default::default()
    };
    let mut owner = match crate::test_support::measured_read(
        fixture.pool.clone(),
        fixture.port,
        fixture.password(),
        &fixture.database,
        budget,
    )
    .await
    {
        Ok(owner) => owner,
        Err(error) => {
            save_cost_receipt(
                directory,
                "population-profile-open-failure",
                &json!({
                    "error":error.code,"error_detail":error.detail,"scope":null,"accounting":null,
                    "pool_source_shutdown":shutdown_state(fixture.pool.source_shutdown()),
                    "limitation":"opening failed before a measured owner was returned; original scope and accounting are unavailable",
                }),
            )?;
            return Err(error);
        }
    };
    let mut observations = Observations::default();
    let outcome = AssertUnwindSafe(until_cancelled(
        cancellation,
        observe(&mut owner, original, &fixture.database, &mut observations),
    ))
    .catch_unwind()
    .await;
    let before_close = owner.report();
    let shutdown = owner.close().await;
    let written = save_cost_receipt(
        directory,
        "population-profile-native",
        &json!({
            "scope":owner.scope(),"query_identity":original,"source_dispatches":owner.queries(),
            "planned_source_statements":SOURCE_DISPATCHES,"runtime_sample_row_limit":RUNTIME_SAMPLE_ROWS,
            "observations":observations,"configuration_unchanged":observations.configuration_unchanged(),
            "runtime_sample_is_complete_population":false,
            "error":outcome.as_ref().ok().and_then(|result|result.as_ref().err()).map(|error|error.code),
            "error_detail":outcome.as_ref().ok().and_then(|result|result.as_ref().err()).and_then(|error|error.detail),
            "panicked":outcome.is_err(),"shutdown":shutdown_state(shutdown),
            "accounting_before_close":before_close,"accounting_after_close":owner.report(),
            "full_profile_including_preparation_and_observer_client_elapsed_ms":started.elapsed().as_secs_f64()*1000.0,
            "production_admission":"unqualified","native_cpu_io_cost_qualified":false,
            "limitation":"read-only observed configuration and a bounded historical fragment sample; missing or changing configuration remains unknown; a numeric replica group is not a physical member or a documented primary/disabled sentinel; no settings, flags, instrumentation or workload retries",
        }),
    );
    let result = match outcome {
        Ok(result) => result,
        Err(panic) => {
            if let Err(error) = written {
                receipt_failure(&error);
            }
            std::panic::resume_unwind(panic)
        }
    };
    if shutdown != SourceShutdown::Quiescent {
        return Err(CollectionStop::CancellationUnconfirmed.error());
    }
    match result {
        Err(original) => {
            if let Err(error) = written {
                receipt_failure(&error);
            }
            Err(original)
        }
        Ok(()) => {
            written?;
            assert_eq!(owner.queries(), SOURCE_DISPATCHES, "no follow-up discovery or retry");
            assert!(!owner.read_metrics().ok_or_else(invalid_data)?.has_unconfirmed_work());
            Ok(())
        }
    }
}

fn receipt_failure(error: &culprit_core::Error) {
    eprintln!("population-profile receipt unavailable: code={:?},detail={:?}", error.code, error.detail);
}

#[derive(Default, serde::Serialize)]
struct Observations {
    stage: Stage,
    before_rows: Option<usize>,
    before: Option<NativeProfilePoint>,
    runtime_rows: Option<usize>,
    runtime: Vec<RuntimeFragment>,
    after_rows: Option<usize>,
    after: Option<NativeProfilePoint>,
}
impl Observations {
    fn configuration_unchanged(&self) -> Option<bool> {
        self.before.as_ref()?.same_configuration(self.after.as_ref()?)
    }
}
#[derive(Default, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
enum Stage {
    #[default]
    NotStarted,
    Before,
    Runtime,
    After,
    Complete,
}

async fn observe<R: NativeRead>(
    owner: &mut R,
    original: &NativeQueryIdentity,
    database: &str,
    observations: &mut Observations,
) -> Result<()> {
    let query_id = original.query_id.parse::<i64>().ok().filter(|id| *id > 0).ok_or_else(invalid_data)?;
    if !original.is_valid() {
        return Err(invalid_data());
    }
    observations.stage = Stage::Before;
    read_profile(owner, database, &mut observations.before_rows, &mut observations.before).await?;
    observations.stage = Stage::Runtime;
    let rows = owner
        .query(&metadata::runtime_sql(), vec![Parameter::Integer(query_id)], RUNTIME_SAMPLE_ROWS)
        .await
        .map_err(ReadError::into_error)?;
    observations.runtime_rows = Some(rows.len());
    if rows.len() > RUNTIME_SAMPLE_ROWS {
        return Err(CollectionStop::RowLimit.error());
    }
    for row in rows {
        observations.runtime.push(RuntimeFragment::decode(&row, original)?);
    }
    observations.stage = Stage::After;
    read_profile(owner, database, &mut observations.after_rows, &mut observations.after).await?;
    observations.stage = Stage::Complete;
    Ok(())
}

/// Preserve each completed component before the next read. The production reader rejects multiple result sets.
async fn read_profile<R: NativeRead>(
    owner: &mut R,
    database: &str,
    observed_rows: &mut Option<usize>,
    point: &mut Option<NativeProfilePoint>,
) -> Result<()> {
    let mut rows = Vec::new();
    for (sql, limit) in [
        (metadata::metadata_sql(), 1),
        (metadata::TRACE_STATUS_SQL.into(), 1),
        (super::super::catalog_sql::REPLICAS.into(), MAX_CATALOG_ROWS),
    ] {
        let component = owner.query(&sql, vec![], limit).await.map_err(ReadError::into_error)?;
        if component.len() > limit {
            return Err(CollectionStop::RowLimit.error());
        }
        rows.extend(component);
        *observed_rows = Some(rows.len());
        let observed = NativeProfilePoint::decode(&rows)?;
        let binding = observed.require_database(database);
        *point = Some(observed);
        binding?;
    }
    Ok(())
}
