use super::*;
use crate::activity::tempdb::tests::{context, reader, replies, task};

fn request() -> CollectionRequest {
    CollectionRequest {
        subject: CollectionSubject::Database,
        selection: CollectionSelection::Snapshot { dataset: SnapshotDataset::TaskSpace },
        budget: CollectionBudget { page_rows: 1, ..Default::default() },
    }
}

#[tokio::test]
async fn frozen_task_pages_reuse_native_rows_and_require_final_identity_and_confirmed_close() {
    let mut sequence = replies(vec![task(0), task(1)]);
    sequence.push(Ok(vec![context()]));
    let mut source = TaskSpaceSource::from_reader(reader(sequence), &request()).unwrap();
    let description = source.describe().await.unwrap();
    assert_eq!(description.observation.snapshot().unwrap().dataset, SnapshotDataset::TaskSpace);
    let CollectionRead::Page { records, next_cursor } =
        source.read_page(TelemetryStream::OperationalSamples, None).await.unwrap()
    else {
        panic!()
    };
    assert_eq!(records.len(), 1);
    assert!(records[0].is_valid());
    assert!(next_cursor.is_some());
    assert_eq!(source.query_count(), 6);
    let CollectionRead::Page { records, next_cursor } =
        source.read_page(TelemetryStream::OperationalSamples, next_cursor.as_deref()).await.unwrap()
    else {
        panic!()
    };
    assert_eq!(records.len(), 1);
    assert!(next_cursor.is_none());
    assert_eq!(source.finish().await.unwrap(), SourceCompletion::Verified);
    assert_eq!(source.query_count(), 7);
    assert_eq!(source.close().await, SourceShutdown::Quiescent);
}
