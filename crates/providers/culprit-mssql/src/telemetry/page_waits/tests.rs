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

#[test]
fn public_page_factory_clamps_both_native_and_frozen_budgets_before_any_native_work() {
    for strict in [false, true] {
        let mut request = request();
        request.budget = if strict {
            CollectionBudget {
                max_snapshot_rows: 3,
                page_rows: 2,
                max_queries: 4,
                max_sql_bytes: 2048,
                statement_timeout_ms: 125,
                total_timeout_ms: 500,
                ..Default::default()
            }
        } else {
            CollectionBudget::default()
        };
        let bounded = bounded_request(&request).unwrap();
        assert_eq!(bounded.budget.max_queries, if strict { 4 } else { 18 });
        assert_eq!(bounded.budget.statement_timeout_ms, if strict { 125 } else { 1000 });
        assert_eq!(bounded.budget.total_timeout_ms, if strict { 500 } else { 5000 });
        assert_eq!(bounded.budget.max_sql_bytes, if strict { 2048 } else { 16 * 1024 });
        assert_eq!(bounded.budget.max_snapshot_rows, if strict { 3 } else { 65 });
        assert_eq!(bounded.budget.page_rows, if strict { 2 } else { 64 });
        assert!(bounded.budget.validate().is_ok());
        let source = PageWaitSource::new(
            Arc::new(Pool::new(tiberius::Config::new(), None)),
            &request,
            "fixture.invalid".into(),
            1433,
        )
        .unwrap();
        assert_eq!(source.query_count(), 0);
        assert_eq!(request.budget.total_timeout_ms, if strict { 500 } else { 60_000 });
    }
    let mut invalid = request();
    invalid.budget.page_rows = 0;
    assert!(
        PageWaitSource::new(
            Arc::new(Pool::new(tiberius::Config::new(), None)),
            &invalid,
            "fixture.invalid".into(),
            1433
        )
        .is_err()
    );
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

#[tokio::test]
async fn scans_and_frozen_collection_export_the_same_exact_index_point_and_qualification() {
    let rows = vec![page_wait(42, "PAGELATCH_EX", "7:1:4144")];
    let mut scan_reader = reader(crate::activity::page_targets::tests::index_operations::replies(rows.clone()));
    let (scan, coverage) = crate::page_waits::collect_from(&mut scan_reader).await;
    let scan = scan.unwrap();
    assert_eq!(coverage.outcomes()[0].state, culprit_core::SourceState::Partial);
    assert_eq!(crate::activity::memory::tests::closed(&scan_reader), 1);
    let mut supplied = crate::activity::page_targets::tests::index_operations::replies(rows);
    supplied.push(Ok(vec![context()]));
    let mut source = PageWaitSource::from_reader(reader(supplied), &request()).unwrap();
    let description = source.describe().await.unwrap();
    let CollectionRead::Page { records, next_cursor: None } =
        source.read_page(TelemetryStream::OperationalSamples, None).await.unwrap()
    else {
        panic!()
    };
    let exported = ExportedRecord::new(
        &description,
        &CaptureIdentity { observation_id: "index-point".into(), revision: None },
        records[0].clone(),
    )
    .unwrap();
    let TelemetryRecord::Operational(operational) = exported.record else { panic!() };
    let OperationalFact::PageWait(point) = operational.fact else { panic!() };
    assert_eq!(*point, scan.observations[0]);
    assert!(matches!(
        point.index_operations,
        Some(PageIndexOperations::Observed { relation: PageIndexOperationRelation::CheckedTarget, .. })
    ));
    assert_eq!(source.finish().await.unwrap(), SourceCompletion::Verified);
    assert_eq!(source.close().await, SourceShutdown::Quiescent);
}

#[tokio::test]
async fn outer_caller_timeout_cannot_turn_unconfirmed_native_shutdown_into_a_verified_capture() {
    let native = crate::activity::page_targets::tests::index_operations::paused_point_reader();
    let mut source = PageWaitSource::from_reader(native, &request()).unwrap();
    assert!(tokio::time::timeout(std::time::Duration::from_millis(5), source.describe()).await.is_err());
    let queries = source.query_count();
    assert_eq!(source.close().await, SourceShutdown::Unconfirmed);
    assert!(source.finish().await.is_err());
    assert_eq!(source.query_count(), queries, "no authority recheck after a closed uncertain native owner");
}
