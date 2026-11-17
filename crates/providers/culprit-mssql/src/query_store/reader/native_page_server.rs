//! Actual changed scan projection on an admitted disposable endpoint. Boxed compatibility never qualifies
//! Hyperscale positive counters, an Azure-only collection route or production cost admission.
use super::*;
use crate::{
    read::measurement::{MeasuredRead, shutdown_state},
    test_support::{
        OwnedSourceFixture,
        query_store_probes::{self, ProbePolicy},
        receipt_directory, save_cost_receipt,
    },
};
use culprit_collector::capture::CollectionCancellation;
use culprit_core::telemetry::digest;
use futures_util::FutureExt;
use serde_json::{Value, json};
use std::{panic::AssertUnwindSafe, path::Path, time::Instant};

const SOURCE_TIMEOUT_MS: u32 = 10_000;
const STATEMENT_TIMEOUT_MS: u32 = 1_500;
const MAX_SOURCE_QUERIES: u32 = 8;

#[tokio::test]
#[ignore = "requires explicit nonce-owned loopback SQL Server with READ_WRITE/ALL Query Store and fresh CULPRIT_NATIVE_COST_RECEIPT_DIR; select query_store::reader::native_page_server explicitly"]
async fn owned_page_server_runtime_projection_keeps_native_profile_and_settles_the_original_reader() {
    let directory = receipt_directory();
    let fixture = OwnedSourceFixture::open("culprit-page-server-runtime").await.expect("owned loopback admission");
    prepare(&fixture, &directory).await.expect("bounded owned Query Store probe");
    let budget = CollectionBudget {
        max_queries: MAX_SOURCE_QUERIES,
        statement_timeout_ms: STATEMENT_TIMEOUT_MS,
        total_timeout_ms: SOURCE_TIMEOUT_MS,
        ..Default::default()
    };
    let started = Instant::now();
    // No outer timeout drops this source owner. The existing native reader owns its absolute source budget,
    // Attention/drain and bounded close; the disposable supervisor owns final database teardown.
    let io = MeasuredRead::open(fixture.pool.clone(), fixture.port, fixture.password(), &fixture.database, budget)
        .await
        .expect("bounded measured reader preparation");
    let mut reader = Reader::new(io);
    let result = AssertUnwindSafe(projection(&mut reader)).catch_unwind().await;
    let shutdown = reader.close().await;
    let quiet = reader.io.is_quiet().await;
    let receipt = json!({
        "scope":reader.io.scope(),"shutdown":shutdown_state(shutdown),"independently_observed_quiet":quiet.as_ref().ok(),
        "source_queries":reader.queries(),"accounting":reader.io.report(),
        "full_profile_including_preparation_and_observer_client_elapsed_ms":started.elapsed().as_secs_f64()*1000.0,
        "projection":result.as_ref().ok().and_then(|result|result.as_ref().ok()),
        "error":result.as_ref().ok().and_then(|result|result.as_ref().err()).map(|error|error.code),
        "fixture_admission_client_elapsed_ms":fixture.admission_elapsed_ms,
        "production_admission":"unqualified","native_cpu_io_cost_qualified":false,
        "coverage":"owned boxed scan projection only; positive Hyperscale statistics and Azure collection remain unqualified",
        "bounds":"bounded metadata and one fully drained runtime summary for the native window; observed membership requires seven dispatches and stays incomplete",
        "cleanup":"original reader closes or fences before this receipt; shared setup creates one owned one-row table and one application probe; supervisor owns final nonce database and table teardown",
    });
    save_cost_receipt(&directory, "page-server-runtime-native", &receipt).expect("private compatibility receipt");
    match result {
        Ok(result) => result.expect("actual page-server projection"),
        Err(panic) => std::panic::resume_unwind(panic),
    };
    assert_eq!(shutdown, SourceShutdown::Quiescent, "unconfirmed shutdown is not compatibility success");
    assert!(quiet.unwrap());
    assert!(!reader.read_metrics().unwrap().has_unconfirmed_work());
}

async fn projection(reader: &mut Reader<MeasuredRead>) -> Result<Value> {
    let layout = reader.native_layout().await?;
    if layout.page_server_reads != super::super::page_server::Availability::NotApplicable {
        return Err(invalid_data());
    }
    let summaries = reader.summary(60).await?;
    if summaries.is_empty()
        || summaries.iter().any(|summary| {
            !summary.page_server_reads.is_valid()
                || summary.page_server_reads.statistics().availability != layout.page_server_reads
                || summary.page_server_reads.statistics().has_values()
        })
    {
        return Err(invalid_data());
    }
    let selected = reader.population().await?;
    let population = if reader.population_catalog().is_some_and(|catalog| {
        matches!(catalog.membership, culprit_core::RuntimePopulationMembership::ObservedWindow { .. })
    }) {
        super::super::replicas::Population::AllGroups
    } else {
        selected
    };
    let template = runtime::summary_sql(layout.tempdb, layout.log_bytes, layout.page_server_reads, population);
    if reader.verify_window().await? != super::super::window_sql::WindowVerification::Unchanged {
        return Err(CollectionStop::SourceChanged.error());
    }
    let metrics = reader.read_metrics().ok_or_else(invalid_data)?;
    let template_id = digest(template.as_bytes());
    if !metrics
        .templates
        .iter()
        .any(|metric| metric.template_id == template_id && metric.completed_tasks == 1 && metric.failures == 0)
    {
        return Err(invalid_data());
    }
    let allowed = [
        digest(runtime::LAYOUT.as_bytes()),
        digest(super::super::catalog_sql::REPLICAS.as_bytes()),
        digest(super::super::window_sql::metadata_sql(layout.waits, false, layout.edition).as_bytes()),
        digest(super::super::window_sql::metadata_sql(layout.waits, true, layout.edition).as_bytes()),
        template_id.clone(),
    ];
    if metrics.templates.iter().any(|metric| !allowed.contains(&metric.template_id)) {
        return Err(invalid_data());
    }
    Ok(
        json!({"engine_edition":layout.edition,"native_profile":layout.page_server_reads.native_property(),"plans":summaries.len(),"source_template_sha256":template_id,"native_fragment_summaries":summaries.iter().map(|summary|&summary.page_server_reads).collect::<Vec<_>>() }),
    )
}

async fn prepare(fixture: &OwnedSourceFixture, directory: &Path) -> Result<()> {
    let originals = query_store_probes::seed(
        fixture,
        directory,
        &CollectionCancellation::default(),
        1,
        ProbePolicy::OwnedApplication,
        "page-server-runtime-native-setup",
    )
    .await?;
    if originals.len() != 1 {
        return Err(invalid_data());
    }
    Ok(())
}
