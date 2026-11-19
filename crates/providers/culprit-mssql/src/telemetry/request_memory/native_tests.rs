//! Exercise the production constructor on an admitted empty boxed database without application execution.
use super::*;
use crate::test_support::OwnedSourceFixture;

#[tokio::test]
#[ignore = "requires the nonce-owned loopback SQL Server fixture; read-only source lifecycle, not cost qualification"]
async fn production_memory_source_accepts_the_native_boxed_owner_and_finishes_its_original_capture() {
    let fixture = OwnedSourceFixture::open("culprit").await.unwrap();
    let requested = CollectionRequest {
        subject: CollectionSubject::Database,
        selection: CollectionSelection::Snapshot { dataset: SnapshotDataset::CurrentRequestMemory },
        budget: CollectionBudget {
            max_queries: 9,
            max_snapshot_rows: 64,
            page_rows: 64,
            statement_timeout_ms: 3_000,
            total_timeout_ms: 10_000,
            ..Default::default()
        },
    };
    let mut source = MemorySource::new(fixture.pool.clone(), &requested, "127.0.0.1".into(), fixture.port).unwrap();
    let before = source.query_count();
    let captured: Result<_> = async {
        let description = source.describe().await?;
        let page = source.read_page(TelemetryStream::OperationalSamples, None).await?;
        let completion = source.finish().await?;
        Ok((description, page, completion, source.query_count()))
    }
    .await;
    // Settle the original owner before reporting a collection failure or an assertion failure.
    let shutdown = source.close().await;
    let metrics = source.read_metrics();
    let (description, page, completion, queries) = captured.expect("production native memory-source lifecycle");
    assert_eq!(shutdown, SourceShutdown::Quiescent);
    assert_eq!(before, 0, "the production constructor must remain lazy");
    assert_eq!(completion, SourceCompletion::Verified);
    assert_eq!(queries, 8, "three initial owner reads, inventory, empty grants, three final owner reads");
    assert_eq!(source.query_count(), queries, "shutdown must not issue another collection query");
    assert_eq!(description.target.database().unwrap().database, fixture.database);
    assert!(description.target.database().unwrap().incarnation.is_none());
    assert!(matches!(
        &description.partition,
        SourcePartition::InstanceLocal { member: Some(member), epoch: Some(_) }
            if member.kind == SharedResourceKind::Instance
    ));
    assert!(NativeReadContext::from_snapshot(&description).unwrap().has_qualified_local_owner());
    let snapshot = description.observation.snapshot().unwrap();
    assert_eq!(snapshot.dataset, SnapshotDataset::CurrentRequestMemory);
    assert_eq!(snapshot.records, 0, "the owned baseline has no application grant actors");
    let CollectionRead::Page { records, next_cursor } = page else { panic!("one empty frozen source page") };
    assert!(records.is_empty() && next_cursor.is_none());
    let metrics = metrics.expect("the original bounded reader retains its work accounting");
    assert_eq!(metrics.active_tasks, 0);
    assert_eq!(metrics.untracked_tasks, 0);
    assert!(!metrics.has_unconfirmed_work());
}
