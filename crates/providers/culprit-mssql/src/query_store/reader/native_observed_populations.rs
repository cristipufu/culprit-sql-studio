//! Compatibility of positive window membership on a disposable endpoint; no replica role or cost qualification.
use super::*;
use crate::{
    read::measurement::{MeasuredRead, shutdown_state},
    test_support::{
        OwnedSourceFixture,
        owned_case::{finish_case, until_cancelled},
        query_store_probes::{self, ProbePolicy},
        receipt_directory, save_cost_receipt,
    },
};
use culprit_collector::capture::CollectionCancellation;
use culprit_core::{RuntimePopulationMembership, telemetry::digest};
use futures_util::FutureExt;
use serde_json::{Value, json};
use std::{panic::AssertUnwindSafe, path::Path, time::Duration};
use tokio::time::Instant;

const CASE_TIMEOUT: Duration = Duration::from_secs(60);
const SOURCE_QUERIES: u32 = 7;

#[tokio::test]
#[ignore = "requires explicit nonce-owned loopback SQL Server with READ_WRITE/ALL Query Store, an empty native replica catalog, and fresh CULPRIT_NATIVE_COST_RECEIPT_DIR; select query_store::reader::native_observed_populations explicitly"]
async fn owned_empty_replica_catalog_retains_the_real_application_group_without_inventing_its_role() {
    let directory = receipt_directory();
    let cancellation = CollectionCancellation::default();
    let started = Instant::now();
    let (expired, outcome) = finish_case(
        started + CASE_TIMEOUT,
        &cancellation,
        AssertUnwindSafe(run(&directory, &cancellation)).catch_unwind(),
    )
    .await;
    save_cost_receipt(&directory, "observed-populations-outcome", &json!({
        "case_deadline_exceeded":expired,"cancellation_requested":cancellation.is_cancelled(),
        "completed":!expired && matches!(&outcome,Ok(Ok(()))),
        "full_case_client_elapsed_ms":started.elapsed().as_secs_f64()*1000.0,
        "production_admission":"unqualified","native_cpu_io_cost_qualified":false,
        "active_template_cancellation":"unexercised; compatibility does not qualify cancellation of this exact fast template",
        "cleanup":"same setup and source owners settle or fence before returning; supervisor owns the disposable database and one-row table teardown",
    })).expect("private outcome receipt");
    match outcome {
        Ok(result) => result.expect("actual empty-catalog compatibility"),
        Err(panic) => std::panic::resume_unwind(panic),
    }
    assert!(!expired, "a late result is not successful compatibility");
}

async fn run(directory: &Path, cancellation: &CollectionCancellation) -> Result<()> {
    let fixture = OwnedSourceFixture::open("culprit-observed-populations").await?;
    let originals = query_store_probes::seed(
        &fixture,
        directory,
        cancellation,
        1,
        ProbePolicy::OwnedApplication,
        "observed-populations-setup",
    )
    .await?;
    if originals.len() != 1 {
        return Err(invalid_data());
    }
    let original = originals.into_values().next().ok_or_else(invalid_data)?;
    let budget = CollectionBudget {
        max_queries: SOURCE_QUERIES,
        statement_timeout_ms: 1500,
        total_timeout_ms: 10_000,
        ..Default::default()
    };
    // The case deadline only cancels a borrowed read; this owner survives until explicit native close.
    let io =
        MeasuredRead::open(fixture.pool.clone(), fixture.port, fixture.password(), &fixture.database, budget).await?;
    let mut reader = Reader::new(io);
    let started = Instant::now();
    let outcome = AssertUnwindSafe(until_cancelled(cancellation, project(&mut reader, &original))).catch_unwind().await;
    let shutdown = reader.close().await;
    let quiet = reader.io.is_quiet().await;
    let written = save_cost_receipt(
        directory,
        "observed-populations-native",
        &json!({
            "scope":reader.io.scope(),"accounting":reader.io.report(),"source_queries":reader.queries(),
            "projection":outcome.as_ref().ok().and_then(|value|value.as_ref().ok()),
            "error":outcome.as_ref().ok().and_then(|value|value.as_ref().err()).map(|error|error.code),
            "error_detail":outcome.as_ref().ok().and_then(|value|value.as_ref().err()).and_then(|error|error.detail),
            "panicked":outcome.is_err(),"shutdown":shutdown_state(shutdown),
            "independently_observed_quiet":quiet.as_ref().ok(),
            "full_source_including_observer_client_elapsed_ms":started.elapsed().as_secs_f64()*1000.0,
            "production_admission":"unqualified","native_cpu_io_cost_qualified":false,
            "limitation":"only positively observed application groups in this native interval population; no complete catalog, role, physical member, history-wide absence or cross-group pooled total",
        }),
    );
    let result = match outcome {
        Ok(result) => result,
        Err(panic) => {
            if let Err(error) = written {
                receipt_error(&error);
            }
            std::panic::resume_unwind(panic)
        }
    };
    if let Err(original) = result {
        if let Err(error) = written {
            receipt_error(&error);
        }
        return Err(original);
    }
    written?;
    if shutdown != SourceShutdown::Quiescent || quiet.ok() != Some(true) {
        return Err(CollectionStop::CancellationUnconfirmed.error());
    }
    Ok(())
}

async fn project(
    reader: &mut Reader<MeasuredRead>,
    original: &culprit_core::telemetry::NativeQueryIdentity,
) -> Result<Value> {
    let catalog = reader.prepare_population_batch(1, 60).await?;
    if catalog.has_complete_membership()
        || !matches!(catalog.membership, RuntimePopulationMembership::ObservedWindow { .. })
        || catalog.populations.iter().any(|row| row.native_properties.keys().any(|key| key != "replica_group_id"))
    {
        return Err(invalid_data());
    }
    let mut groups = Vec::new();
    let mut original_rows = 0;
    for population in &catalog.populations {
        reader.select_batch_population(Some(&population.identity)).await?;
        let rows = reader.summary(60).await?;
        if rows.is_empty()
            || rows.iter().any(|row| row.population.identity().ok().flatten().as_ref() != Some(&population.identity))
        {
            return Err(invalid_data());
        }
        let matching = rows.iter().filter(|row| &row.identity == original).count();
        original_rows += matching;
        groups.push(json!({"population":population.identity,"summary_rows":rows.len(),"original_query_rows":matching}));
    }
    if original_rows == 0 || reader.queries() != 4 {
        return Err(invalid_data());
    }
    let (window, verification) = reader.finish_population_batch().await?;
    let final_catalog = reader.population_catalog().ok_or_else(invalid_data)?;
    let native = reader.native_layout.ok_or_else(invalid_data)?;
    let template = runtime::summary_sql(
        native.tempdb,
        native.log_bytes,
        native.page_server_reads,
        super::super::replicas::Population::AllGroups,
    );
    let metrics = reader.read_metrics().ok_or_else(invalid_data)?;
    let template_id = digest(template.as_bytes());
    let allowed = [
        (digest(runtime::LAYOUT.as_bytes()), 2),
        (digest(super::super::catalog_sql::REPLICAS.as_bytes()), 2),
        (digest(super::super::window_sql::metadata_sql(native.waits, false, native.edition).as_bytes()), 1),
        (digest(super::super::window_sql::metadata_sql(native.waits, true, native.edition).as_bytes()), 1),
        (template_id.clone(), 1),
    ];
    if allowed.iter().any(|(id, count)| {
        !metrics
            .templates
            .iter()
            .any(|row| &row.template_id == id && row.completed_tasks == *count && row.failures == 0)
    }) || metrics.templates.iter().any(|row| !allowed.iter().any(|(id, _)| &row.template_id == id))
    {
        return Err(invalid_data());
    }
    if verification != super::super::window_sql::WindowVerification::Unchanged
        || catalog.populations != final_catalog.populations
        || final_catalog.has_complete_membership()
        || !final_catalog.membership.matches_window(window.start.as_deref(), window.end.as_deref())
        || reader.queries() != SOURCE_QUERIES
        || !metrics
            .templates
            .iter()
            .any(|row| row.template_id == template_id && row.completed_tasks == 1 && row.failures == 0)
        || metrics.has_unconfirmed_work()
    {
        return Err(invalid_data());
    }
    Ok(
        json!({"membership":final_catalog.membership,"groups":groups,"original_query_rows":original_rows,"source_template_sha256":template_id}),
    )
}

fn receipt_error(error: &culprit_core::Error) {
    eprintln!("owned observed-population receipt unavailable: code={:?},detail={:?}", error.code, error.detail);
}
