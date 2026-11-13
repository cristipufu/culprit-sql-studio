use super::*;
use crate::{
    activity::page_targets::tests::{context, page_wait, reader, successful_replies},
    resources::test_support::replace,
    value::Val,
};

fn request() -> CollectionRequest {
    CollectionRequest {
        subject: CollectionSubject::Database,
        selection: CollectionSelection::Snapshot { dataset: SnapshotDataset::PageWaitTargets },
        budget: CollectionBudget { page_rows: 1, ..Default::default() },
    }
}

#[test]
fn unsupported_session_allocations_are_rejected_by_public_dispatch_before_native_work() {
    let mut request = request();
    request.selection = CollectionSelection::Snapshot { dataset: SnapshotDataset::SessionMemory };
    let pool = Arc::new(Pool::new(tiberius::Config::new(), None));
    let error = crate::telemetry::source(pool, &request, "fixture.invalid".into(), 1433).err().unwrap();
    assert_eq!(error.code, culprit_core::ErrorCode::Unsupported);
}

#[tokio::test]
async fn explicit_page_dataset_exports_the_same_waiters_targets_and_redacted_sql_then_rechecks_authority() {
    let mut replies = successful_replies(vec![
        page_wait(42, "PAGELATCH_EX", "7:1:4144"),
        page_wait(43, "PAGEIOLATCH_SH", "7:1:4144"),
    ]);
    replies.push(Ok(vec![context()]));
    let mut source = PageWaitSource::from_reader(reader(replies), &request()).unwrap();
    let description = source.describe().await.unwrap();
    let snapshot = description.observation.snapshot().unwrap();
    assert_eq!(snapshot.dataset, SnapshotDataset::PageWaitTargets);
    assert_eq!(snapshot.records, 2);
    assert_eq!(snapshot.sources[0].state, culprit_core::SourceState::Partial);
    assert!(!snapshot.sources[0].complete_scope);
    assert_eq!(source.query_count(), 10);
    assert!(source.finish().await.is_err());
    assert_eq!(source.query_count(), 10);
    let mut cursor = None;
    let mut pages = 0;
    loop {
        let CollectionRead::Page { records, next_cursor } =
            source.read_page(TelemetryStream::OperationalSamples, cursor.as_deref()).await.unwrap()
        else {
            panic!()
        };
        assert_eq!(records.len(), 1);
        let exported = ExportedRecord::new(
            &description,
            &CaptureIdentity { observation_id: "page-fixture".into(), revision: None },
            records[0].clone(),
        )
        .unwrap();
        let TelemetryRecord::Operational(operational) = exported.record else { panic!() };
        let OperationalFact::PageWait(row) = operational.fact else { panic!() };
        assert!(row.is_valid());
        assert_eq!(row.resolution, PageWaitResolution::ObservedAtBothBounds);
        assert_eq!(row.request.client.as_ref().unwrap().application.as_deref(), Some("app"));
        assert!(!serde_json::to_string(&row).unwrap().contains("private-page-canary"));
        pages += 1;
        cursor = next_cursor;
        if cursor.is_none() {
            break;
        }
    }
    assert_eq!(pages, 2);
    assert_eq!(source.finish().await.unwrap(), SourceCompletion::Verified);
    assert_eq!(source.query_count(), 11);
    assert_eq!(source.close().await, SourceShutdown::Quiescent);
}

#[tokio::test]
async fn source_changes_after_frozen_page_delivery_cannot_verify_the_capture() {
    let mut replies = successful_replies(vec![page_wait(42, "PAGELATCH_EX", "7:1:4144")]);
    replies.push(Ok(vec![replace(&context(), &[("physical_incarnation", Val::Text("replacement".into()))])]));
    let mut source = PageWaitSource::from_reader(reader(replies), &request()).unwrap();
    source.describe().await.unwrap();
    assert!(matches!(
        source.read_page(TelemetryStream::OperationalSamples, None).await.unwrap(),
        CollectionRead::Page { next_cursor: None, .. }
    ));
    assert_eq!(source.finish().await.unwrap(), SourceCompletion::Changed);
    assert_eq!(source.close().await, SourceShutdown::Quiescent);
}
