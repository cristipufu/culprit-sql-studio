//! Exact boxed SQL2022 collection through the real source gate, lease, Outbox and canonical readback.
use crate::{
    read::{invalid_data, measurement::shutdown_state},
    test_support::{
        OwnedSourceFixture, owned_captures::OwnedCaptures, owned_case::finish_case,
        query_store_probes::seed_closed_interval, receipt_directory, save_cost_receipt,
    },
};
use culprit_collector::capture::CollectionCancellation;
use culprit_core::{Result, telemetry::*};
use futures_util::FutureExt;
use serde_json::json;
use std::{panic::AssertUnwindSafe, path::Path, time::Duration};
use tokio::time::Instant;
const ADMISSION_TIMEOUT: Duration = Duration::from_secs(10);
const CASE_TIMEOUT: Duration = Duration::from_secs(120);
#[tokio::test]
#[ignore = "requires fresh explicit nonce-owned loopback SQL2022 with READ_WRITE/ALL Query Store and CULPRIT_NATIVE_COST_RECEIPT_DIR; owned-only one-minute configuration/probe; select telemetry::native_history explicitly"]
async fn owned_boxed_history_keeps_native_acquisition_separate_from_data_identity_and_replays_canonically() {
    let directory = receipt_directory();
    let cancellation = CollectionCancellation::default();
    let (expired, fixture) = finish_case(
        Instant::now() + ADMISSION_TIMEOUT,
        &cancellation,
        OwnedSourceFixture::open("culprit-query-history-native"),
    )
    .await;
    let fixture = fixture.expect("exact nonce-owned loopback source");
    assert!(!expired, "fixture admission settled after deadline");
    let started = Instant::now();
    let (expired, result) = finish_case(
        started + CASE_TIMEOUT,
        &cancellation,
        AssertUnwindSafe(Box::pin(run(&fixture, &directory, &cancellation))).catch_unwind(),
    )
    .await;
    save_cost_receipt(&directory,"query-history-native-outcome",&json!({"completed":!expired&&matches!(&result,Ok(Ok(()))),"case_deadline_exceeded":expired,
        "cancelled":cancellation.is_cancelled(),"source_shutdown":shutdown_state(fixture.pool.source_shutdown()),
        "client_elapsed_ms":started.elapsed().as_secs_f64()*1000.0,"production_admission":"unqualified","native_cpu_io_cost_qualified":false,
        "cleanup":"all original setup/source owners settle or fence; supervisor owns nonce database and one-row probe-table teardown; no retry or external replay"})).expect("private native outcome");
    match result {
        Ok(result) => result.expect("actual bounded boxed history capture"),
        Err(panic) => std::panic::resume_unwind(panic),
    };
    assert!(!expired, "same original owners settled after absolute case deadline");
}
async fn run(fixture: &OwnedSourceFixture, directory: &Path, cancellation: &CollectionCancellation) -> Result<()> {
    let (query, interval) = seed_closed_interval(fixture, directory, cancellation).await?;
    let mut captures = OwnedCaptures::from_pool(fixture.pool.clone(), fixture.port)?;
    let capture = captures
        .capture_with_cancellation(
            CollectionSelection::QueryHistory { from: interval.from.clone(), until: interval.until.clone() },
            cancellation,
        )
        .await?;
    let source = capture.manifest.description.as_ref().ok_or_else(invalid_data)?;
    let history = source.query_history().ok_or_else(invalid_data)?;
    let owner = history.acquisition_owner.as_ref().ok_or_else(invalid_data)?;
    if history.interval != interval
        || source.partition != SourcePartition::Target
        || source.target.database().is_none_or(|database| database.incarnation.is_some())
        || !owner.has_qualified_local_owner()
        || owner.database_writable != Some(true)
        || !ObservationBounds::new(&owner.observed_from, &owner.observed_until)
            .is_some_and(|bounds| bounds.has_positive_duration())
        || NativeSourceReadScope::from_context(owner).is_ok()
        || !matches!(capture.manifest.status, CollectionStatus::Complete)
    {
        return Err(invalid_data());
    }
    let definition = capture
        .records
        .iter()
        .find_map(|row| match &row.record {
            TelemetryRecord::QueryDefinition(definition) if definition.identity == query => Some(definition),
            _ => None,
        })
        .ok_or_else(invalid_data)?;
    if definition.id != query.definition_id(source)? || definition.origin != StatementOrigin::Application {
        return Err(invalid_data());
    }
    let runtime = capture
        .records
        .iter()
        .filter_map(|row| match &row.record {
            TelemetryRecord::QueryRuntime(runtime)
                if runtime.key.references.query.as_ref().is_some_and(|reference| {
                    reference.native == query.native() && reference.definition_id.as_ref() == Some(&definition.id)
                }) =>
            {
                Some(runtime)
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    if runtime.is_empty() || runtime.iter().any(|row| !row.key.references.is_resolved()) {
        return Err(invalid_data());
    }
    for row in &runtime {
        history.execution_population(&row.key.native.qualifiers)?;
        // Same genuine source rows and native query; no observer read or fixture SQL is added here.
        if row.metrics.len() != 12
            || row.metrics.iter().any(|metric| {
                !metric.is_valid()
                    || metric.key == RuntimeMetricKey::PhysicalReadOperations
                        && (metric.standard_deviation.is_some() || metric.unit != RuntimeMetricUnit::Count)
            })
        {
            return Err(invalid_data());
        }
        if row
            .metric(&RuntimeMetricKey::PhysicalReads)
            .is_none_or(|metric| metric.unit != RuntimeMetricUnit::Pages { bytes_per_page: 8192 })
            || row.page_server_reads.as_deref().is_none_or(|statistics| !statistics.is_valid())
        {
            return Err(invalid_data());
        }
    }
    let native_waits = capture
        .records
        .iter()
        .filter_map(|row| match &row.record {
            TelemetryRecord::QueryWaits(wait)
                if wait.key.references.query.as_ref().is_some_and(|reference| {
                    reference.native == query.native() && reference.definition_id.as_ref() == Some(&definition.id)
                }) =>
            {
                Some(wait)
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    if native_waits.iter().any(|wait| !wait.is_valid()) {
        return Err(invalid_data());
    }
    captures.verify_retained(&capture).await?;
    let metrics = capture.manifest.read_metrics.as_ref().ok_or_else(invalid_data)?;
    if !metrics.is_valid()
        || metrics.has_unconfirmed_work()
        || fixture.pool.source_shutdown() != SourceShutdown::Quiescent
    {
        return Err(CollectionStop::CancellationUnconfirmed.error());
    }
    save_cost_receipt(
        directory,
        "query-history-native-capture",
        &json!({"source_queries":capture.manifest.source_queries,"read_metrics":metrics,
        "native_interval":history.interval,"acquisition_owner":owner,"logical_partition":source.partition,
        "global_database_incarnation":source.target.database().and_then(|database|database.incarnation.as_ref()),
        "population_binding":history.population_binding,"population_catalog":history.population_catalog,
        "retained_rows":capture.records.len(),"owned_query_wait_fragments":native_waits.len(),"native_wait_nonempty_population_observed":!native_waits.is_empty(),"native_wait_field_value_counts":{"last":native_waits.iter().filter(|row|row.last.is_some()).count(),"standard_deviation":native_waits.iter().filter(|row|row.standard_deviation.is_some()).count(),"category_description":native_waits.iter().filter(|row|row.category_description.is_some()).count(),"outcome_description":native_waits.iter().filter(|row|row.outcome_description.is_some()).count()},"owned_wait_native_fields":native_waits.iter().map(|row|json!({"category":row.category,"native":row.key.native,"last":row.last,"standard_deviation":row.standard_deviation,"category_description":row.category_description,"outcome_description":row.outcome_description})).collect::<Vec<_>>(),"owned_runtime_native_statistics":runtime.iter().map(|row|&row.metrics).collect::<Vec<_>>(),"owned_query_runtime_fragments":runtime.len(),"native_runtime_statistic_families":12,"native_statistics_nullable_means_and_exact_endpoints_verified":true,"physical_read_operations_distinct_from_pages":true,"original_reference_resolved":true,
        "canonical_outbox_verified":true,"capture_export_client_elapsed_ms":capture.capture_export_ms,
        "production_admission":"unqualified","native_cpu_io_cost_qualified":false}),
    )?;
    Ok(())
}
