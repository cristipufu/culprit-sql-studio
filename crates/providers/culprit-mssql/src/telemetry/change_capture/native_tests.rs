//! Read-only native capability/retained-history acceptance through the existing real factory and sealed Outbox.
use super::*;
use crate::test_support::{
    OwnedSourceFixture, owned_captures::OwnedCaptures, owned_case::finish_case, receipt_directory, save_cost_receipt,
};
use culprit_collector::capture::CollectionCancellation;
use futures_util::FutureExt;
use serde_json::json;
use std::{panic::AssertUnwindSafe, path::Path, time::Duration};
use tokio::time::Instant;
#[tokio::test]
#[ignore = "requires nonce-owned loopback SQL2022+ with change-feed DMV layouts; no feature enablement or cost qualification"]
async fn owned_change_feed_native_source_keeps_available_populations_independent_and_the_sealed_original() {
    let directory = receipt_directory();
    let cancellation = CollectionCancellation::default();
    let (expired, fixture) = finish_case(
        Instant::now() + Duration::from_secs(10),
        &cancellation,
        OwnedSourceFixture::open("culprit-change-feed-native"),
    )
    .await;
    let fixture = fixture.expect("exact nonce-owned database admission");
    assert!(!expired);
    let started = Instant::now();
    let (expired, outcome) = finish_case(
        started + Duration::from_secs(30),
        &cancellation,
        AssertUnwindSafe(Box::pin(run(&fixture, &directory, &cancellation))).catch_unwind(),
    )
    .await;
    save_cost_receipt(&directory,"change-feed-source-outcome",&json!({"completed":!expired&&matches!(&outcome,Ok(Ok(()))),"deadline_exceeded":expired,"cancelled":cancellation.is_cancelled(),
        "source_shutdown":crate::read::measurement::shutdown_state(fixture.pool.source_shutdown()),"elapsed_ms":started.elapsed().as_secs_f64()*1000.0,
        "fixture_preparation":"existing positively nonce-owned admission; no workload/feature configuration or background actors", "source_total_dispatch_cap":13,
        "native_runtime_progress_proven":false,"native_cpu_io_cost_qualified":false,"production_admission":"unqualified"})).expect("private owned outcome");
    match outcome {
        Ok(result) => result.expect("real configured native source/Outbox"),
        Err(panic) => std::panic::resume_unwind(panic),
    }
    assert!(!expired);
}
async fn run(fixture: &OwnedSourceFixture, directory: &Path, cancellation: &CollectionCancellation) -> Result<()> {
    let mut captures = OwnedCaptures::from_pool(fixture.pool.clone(), fixture.port)?;
    let capture = captures
        .capture_with_cancellation(CollectionSelection::Snapshot { dataset: SnapshotDataset::ChangeFeed }, cancellation)
        .await?;
    let source = capture.source()?;
    let header = source.observation.snapshot().ok_or_else(invalid_data)?;
    let rows = capture
        .records
        .iter()
        .map(|record| {
            let TelemetryRecord::Operational(row) = &record.record else { return Err(invalid_data()) };
            Ok(row)
        })
        .collect::<Result<Vec<_>>>()?;
    let report = culprit_checks::change_feed::explain(culprit_checks::change_feed::ChangeFeedEvidence {
        source,
        observations: &rows,
        read_complete: true,
    })?;
    // Preserve the authoritative original before asserting native availability and independent original population checks.
    save_cost_receipt(
        directory,
        "change-feed-source-original",
        &json!({"manifest":capture.manifest,"rows":capture.records,"report":report}),
    )
    .map_err(|_| invalid_data())?;
    captures.verify_retained(&capture).await?;
    let availability = rows
        .iter()
        .find_map(|row| match &row.fact {
            OperationalFact::MssqlChangeFeed(p) => match &p.data {
                MssqlChangeFeedData::Availability(a) => Some(a),
                _ => None,
            },
            _ => None,
        })
        .ok_or_else(invalid_data)?;
    if capture.manifest.status != CollectionStatus::Complete
        || capture.manifest.source_queries > 13
        || header.records as usize != capture.records.len()
        || !availability.scan_layout_available
        || !availability.error_layout_available
        || availability.has_permission() != Some(true)
        || !matches!(
            header.sources[1].state,
            culprit_core::SourceState::Collected
                | culprit_core::SourceState::Empty
                | culprit_core::SourceState::Partial
        )
        || !matches!(
            header.sources[2].state,
            culprit_core::SourceState::Collected
                | culprit_core::SourceState::Empty
                | culprit_core::SourceState::Partial
        )
        || report.entries.len() + 1 != capture.records.len()
        || !rows.iter().all(|row| matches!(&row.fact,OperationalFact::MssqlChangeFeed(p) if p.fits_source(source)))
        || source.target.database().is_none_or(|db| db.database != fixture.database || db.incarnation.is_some())
        || !source.is_partition_qualified()
        || source.continuity != SourceContinuity::Unverified
        || fixture.pool.source_shutdown() != SourceShutdown::Quiescent
    {
        return Err(invalid_data());
    }
    Ok(())
}
