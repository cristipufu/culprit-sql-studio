//! Explicit owned endpoint qualification of the changed catalog statement, never production cost approval.
use super::*;
use crate::{
    read::{NativeRead, measurement::shutdown_state},
    test_support::{
        OwnedSourceFixture,
        owned_case::{finish_case, until_cancelled},
        receipt_directory, save_cost_receipt,
    },
};
use culprit_collector::capture::CollectionCancellation;
use futures_util::FutureExt;
use serde_json::json;
use std::{panic::AssertUnwindSafe, path::Path, time::Duration};
use tokio::time::Instant;

#[tokio::test]
#[ignore = "requires explicit nonce-owned loopback SQL Server, CULPRIT_SMOKE_OWNER / DATABASE, MSSQL source port/password and fresh CULPRIT_NATIVE_COST_RECEIPT_DIR; select database_properties::native_tests explicitly"]
async fn owned_selected_database_catalog_template_retains_one_point_and_closes_the_original_reader() {
    let directory = receipt_directory();
    let cancellation = CollectionCancellation::default();
    let (expired, fixture) = finish_case(
        Instant::now() + Duration::from_secs(10),
        &cancellation,
        OwnedSourceFixture::open("culprit-database-properties-source"),
    )
    .await;
    let fixture = fixture.expect("nonce-owned endpoint admission");
    assert!(!expired, "original admission has settled");
    let (expired, result) =
        finish_case(Instant::now() + Duration::from_secs(30), &cancellation, run(&fixture, &directory, &cancellation))
            .await;
    result.expect("owned database-properties source");
    assert!(!expired, "the same bounded native owner settles before the case returns");
}

async fn run(fixture: &OwnedSourceFixture, directory: &Path, cancellation: &CollectionCancellation) -> Result<()> {
    let mut io = crate::test_support::measured_read(
        fixture.pool.clone(),
        fixture.port,
        fixture.password(),
        &fixture.database,
        CollectionBudget { max_queries: 1, statement_timeout_ms: 1500, total_timeout_ms: 5000, ..Default::default() },
    )
    .await?;
    let sql = query();
    let template = digest(sql.as_bytes());
    let watch = match io.watch_template(&template) {
        Ok(watch) => watch,
        Err(error) => {
            io.close().await;
            return Err(error);
        }
    };
    let started = Instant::now();
    let outcome = AssertUnwindSafe(until_cancelled(cancellation, read(&mut io))).catch_unwind().await;
    // Close the original owner on native error, case cancellation and panic before inspecting any result.
    let shutdown = io.close().await;
    let quiet = io.is_quiet().await;
    save_cost_receipt(
        directory,
        "database-properties-native",
        &json!({
            "scope":io.scope(), "accounting":io.report(), "source_queries":io.queries(),
            "template_id":template, "shutdown":shutdown_state(shutdown),
            "independently_observed_quiet":quiet.as_ref().ok(),
            "client_elapsed_ms":started.elapsed().as_secs_f64()*1000.0,
            "fixture_admission_client_elapsed_ms":fixture.admission_elapsed_ms,
            "error":outcome.as_ref().ok().and_then(|result|result.as_ref().err()).map(|error|error.code),
            "observed_rows":outcome.as_ref().ok().and_then(|result|result.as_ref().ok()).map(|_|1),
            "exact_template_active_observed":watch.was_active(),
            "active_template_cancellation":"unexercised; this metadata statement may complete before a native active sample",
            "production_admission":"unqualified", "native_cpu_io_cost_qualified":false,
            "scope_limit":"only this owned endpoint's actual native edition/version; no Azure SQL or Hyperscale qualification",
            "bounds":"two-row ambiguity witness; selected database catalog only; output bounds are not server CPU bounds",
            "cleanup":"same reader closed before this receipt; supervisor owns disposable database teardown; no schema or data mutation",
        }),
    )?;
    let result = match outcome {
        Ok(result) => result,
        Err(panic) => std::panic::resume_unwind(panic),
    };
    if shutdown != SourceShutdown::Quiescent {
        return Err(CollectionStop::CancellationUnconfirmed.error());
    }
    let frame = result?;
    if !quiet? {
        return Err(CollectionStop::SourceChanged.error());
    }
    let point = frame.properties?;
    assert!(point.is_valid());
    assert_eq!(point.database, fixture.database);
    assert_eq!(io.scope().target.database().unwrap().database, point.database);
    assert!(point.catalog_state.code.is_some());
    assert!(point.compatibility_level.is_some());
    assert!(
        point.query_store_enabled.is_some(),
        "actual supported column must not disappear behind capability projection"
    );
    assert_eq!(io.queries(), 1, "ordinary settings and Operational detail reuse this exact one-dispatch reader");
    let metrics = io.read_metrics().unwrap();
    let phase = metrics.templates.iter().find(|phase| phase.template_id == template).expect("actual changed template");
    assert_eq!(phase.completed_tasks, 1);
    assert_eq!(phase.failures, 0);
    Ok(())
}
