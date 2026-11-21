//! A real original CPU profile, not a utilization/production-cost qualification.
use super::*;
use crate::test_support::{owned_captures::OwnedCaptures, receipt_directory, save_cost_receipt};
use culprit_collector::capture::{CollectionCancellation, SourceFactory};
struct Factory<'a>(&'a MssqlProvider);
#[async_trait::async_trait]
impl SourceFactory for Factory<'_> {
    async fn open(&self, request: &CollectionRequest) -> Result<Box<dyn TelemetrySource>> {
        self.0.telemetry_source(request)
    }
}
#[tokio::test]
#[ignore = "requires the positively nonce-owned SQL Server provider fixture and private receipt directory"]
async fn owned_cpu_factory_retains_complete_native_pool_totals_and_truthful_retained_ring_availability() {
    Box::pin(capture_original_profile()).await;
}

async fn capture_original_profile() {
    let fixture = Box::pin(OwnedSourceFixture::open("culprit")).await.unwrap();
    let member = provider(fixture.port, &fixture.database, fixture.password());
    let mut preparation = Box::new(crate::read::ReadSession::application(member.pool.clone()));
    let observed = Box::pin(crate::instance::read(preparation.as_mut())).await;
    let preparation_queries = preparation.queries();
    let shutdown = preparation.close().await;
    assert_eq!(shutdown, SourceShutdown::Quiescent);
    let (owner, _) = observed.unwrap();
    assert_eq!(preparation_queries, 2);
    let mut originals = Box::new(OwnedCaptures::from_pool(member.pool.clone(), fixture.port).unwrap());
    let cancellation = CollectionCancellation::default();
    let capture = Box::pin(originals.capture_with_subject(
        CollectionSelection::Snapshot { dataset: SnapshotDataset::ProcessCpu },
        CollectionSubject::Shared { resource: owner.identity.clone() },
        &cancellation,
        &Factory(&member),
    ))
    .await
    .unwrap();
    Box::pin(originals.verify_retained(&capture)).await.unwrap();
    assert_eq!(member.source_shutdown(), SourceShutdown::Quiescent);
    let source = capture.manifest.description.as_ref().unwrap();
    assert!(cpu_profile_header_is_valid(source));
    assert_eq!(capture.manifest.status, CollectionStatus::Complete);
    assert_eq!(capture.manifest.source_queries, 9);
    assert_eq!(capture.manifest.request.budget.max_queries, 10);
    let header = source.observation.snapshot().unwrap();
    let pools = header.sources.iter().find(|coverage| coverage.source == MSSQL_CPU_POOLS_SOURCE).unwrap();
    assert_eq!(pools.state, SourceState::Collected);
    assert!(pools.complete_scope && pools.records > 0 && pools.records <= MAX_MSSQL_CPU_POOLS);
    let context = NativeReadContext::from_snapshot(source).unwrap();
    let rows = capture
        .records
        .iter()
        .map(|row| {
            let TelemetryRecord::Operational(row) = &row.record else { panic!() };
            assert!(cpu_profile_observation_fits(row, source));
            row.clone()
        })
        .collect::<Vec<_>>();
    for row in rows.iter().filter(|row| row.source == MSSQL_CPU_POOLS_SOURCE) {
        let OperationalFact::CounterSet(sample) = &row.fact else { panic!() };
        let point = MssqlCpuPoolPoint::from_original(sample, &context).unwrap();
        assert!(point.total_ms().is_some());
        assert!(sample.counters.iter().all(|counter| counter.reset == culprit_core::ResetMarker::Unknown));
    }
    let history = header.sources.iter().find(|coverage| coverage.source == MSSQL_CPU_HISTORY_SOURCE).unwrap();
    assert!(matches!(history.state, SourceState::Collected | SourceState::Empty));
    assert!(history.complete_scope);
    assert!(history.records <= 1);
    for row in rows.iter().filter(|row| row.source == MSSQL_CPU_HISTORY_SOURCE) {
        let OperationalFact::CounterSet(sample) = &row.fact else { panic!() };
        let point = MssqlCpuHistoryPoint::from_original(sample, &context).unwrap();
        if point.platform() == Some("Linux") {
            assert_eq!(point.idle_percent(), None);
        }
    }
    let report = culprit_checks::instance_resources::explain_source(source, &rows, true).unwrap();
    assert_eq!(report.process_cpu.as_ref().unwrap().populations.len(), 2);
    assert!(report.process_cpu_interval.is_none());
    let retained_ring_present = history.records == 1;
    save_cost_receipt(&receipt_directory(),"boxed-cpu-source",&serde_json::json!({"manifest":capture.manifest,"records":capture.records,"report":report,"preparation_reader_queries":2,"preparation_setup_query_allowance":1,"source_reader_queries":9,"source_setup_query_allowance":1,"total_paid_query_allowance":13,"retained_ring_present":retained_ring_present,"retained_ring_empty_is_not_a_sample":!retained_ring_present,"native_utilization_qualified":false,"native_cost_approved":false})).unwrap();
}

#[test]
fn original_cpu_capture_body_keeps_native_owners_off_the_poll_stack() {
    fn assert_bounded<F: std::future::Future>(_: impl FnOnce() -> F) {
        const MAX_INLINE_PHASE_BYTES: usize = 4 * 1024;
        let bytes = std::mem::size_of::<F>();
        assert!(bytes <= MAX_INLINE_PHASE_BYTES, "the original CPU capture body stores {bytes} bytes inline");
    }
    // Infer the actual future without constructing a fixture, outbox or connection.
    assert_bounded(capture_original_profile);
}
