use super::*;
use crate::{
    read::NativeRead,
    test_support::{owned_captures::OwnedCaptures, receipt_directory, save_cost_receipt},
};
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
async fn owned_numa_node_factory_retains_both_native_populations_through_sealed_outbox_without_pressure_or_database_attribution()
 {
    let fixture = OwnedSourceFixture::open("culprit").await.unwrap();
    let member = provider(fixture.port, &fixture.database, fixture.password());
    let mut preparation = crate::read::ReadSession::application(member.pool.clone());
    let observed = crate::instance::read(&mut preparation).await;
    let preparation_reads = preparation.queries();
    let preparation_close = preparation.close().await;
    assert_eq!(preparation_close, SourceShutdown::Quiescent);
    let (owner, _) = observed.unwrap();
    assert_eq!(preparation_reads, 2);
    let mut originals = OwnedCaptures::from_pool(member.pool.clone(), fixture.port).unwrap();
    let cancellation = CollectionCancellation::default();
    let captured = Box::pin(originals.capture_with_subject(
        CollectionSelection::Snapshot { dataset: SnapshotDataset::NumaNodes },
        CollectionSubject::Shared { resource: owner.identity.clone() },
        &cancellation,
        &Factory(&member),
    ))
    .await
    .unwrap();
    originals.verify_retained(&captured).await.unwrap();
    let description = captured.manifest.description.as_ref().unwrap();
    assert!(numa_node_header_is_valid(description));
    assert_eq!(captured.manifest.status, CollectionStatus::Complete);
    // Reader SQL is10; the possible session SET occupies the same declared11 total, not a free extra slot.
    assert_eq!(captured.manifest.source_queries, 10);
    assert_eq!(captured.manifest.request.budget.max_queries, 11);
    assert_eq!(member.source_shutdown(), SourceShutdown::Quiescent);
    let header = description.observation.snapshot().unwrap();
    for kind in MssqlNumaNodeKind::ALL {
        let coverage = header.sources.iter().find(|source| source.source == kind.source()).unwrap();
        assert_eq!(coverage.state, SourceState::Collected);
        assert!(coverage.records > 0 && coverage.complete_scope);
        for row in captured
            .records
            .iter()
            .filter(|row| matches!(&row.record,TelemetryRecord::Operational(record) if record.source==kind.source()))
        {
            let TelemetryRecord::Operational(record) = &row.record else { panic!() };
            let OperationalFact::CounterSet(sample) = &record.fact else { panic!() };
            let point = MssqlNumaNodePoint::from_sample(sample).unwrap();
            assert_eq!(point.kind(), kind);
            assert_eq!(sample.scope, MeasurementScope::Server);
        }
    }
    let rows = captured
        .records
        .iter()
        .map(|row| match &row.record {
            TelemetryRecord::Operational(row) => row.clone(),
            _ => panic!(),
        })
        .collect::<Vec<_>>();
    let report = culprit_checks::instance_resources::explain_source(description, &rows, true).unwrap();
    assert_eq!(report.numa_nodes.as_ref().unwrap().populations.len(), 2);
    assert!(report.groups.is_empty() && report.observed_at.is_none() && report.native_populations_complete.is_none());
    save_cost_receipt(&receipt_directory(),"numa-node-source",&serde_json::json!({"manifest":captured.manifest,"records":captured.records,"report":report,
        "preparation_reader_queries":preparation_reads,"preparation_setup_query_allowance":1,"source_reader_queries":10,"source_setup_query_allowance":1,"total_paid_query_allowance":14,"preparation_shutdown":format!("{preparation_close:?}"),"native_cost_approved":false})).unwrap();
}
