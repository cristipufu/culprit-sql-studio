use super::*;
mod holders;
mod writers;
use crate::{
    activity::page_targets::tests::{context, page_wait, reader, reader_with_native, successful_replies},
    resources::test_support::{FakeRead, replace},
    value::Val,
};

fn request() -> CollectionRequest {
    CollectionRequest {
        subject: CollectionSubject::Database,
        selection: CollectionSelection::Snapshot { dataset: SnapshotDataset::PageWaitTargets },
        budget: CollectionBudget { page_rows: 1, ..Default::default() },
    }
}

#[tokio::test]
async fn unavailable_task_visibility_keeps_frozen_coordinator_rows_and_empty_scope_partial() {
    for present in [true, false] {
        let mut replies = if present {
            successful_replies(vec![replace(
                &page_wait(42, "PAGELATCH_EX", "7:1:4144"),
                &[("task_visibility", Val::Bool(false))],
            )])
        } else {
            let empty = replace(&context(), &[("task_visibility", Val::Bool(false))]);
            vec![Ok(vec![context()]), Ok(vec![empty]), Ok(vec![context()])]
        };
        replies.push(Ok(vec![context()]));
        let mut source = PageWaitSource::from_reader(reader(replies), &request()).unwrap();
        let description = source.describe().await.unwrap();
        let snapshot = description.observation.snapshot().unwrap();
        assert_eq!(snapshot.records, u32::from(present));
        assert_eq!(snapshot.sources[0].state, culprit_core::SourceState::Partial);
        assert!(!snapshot.sources[0].complete_scope);
        assert_eq!(snapshot.sources[0].stopped, Some(CollectionStop::SourceUnavailable));
        let CollectionRead::Page { records, next_cursor } =
            source.read_page(TelemetryStream::OperationalSamples, None).await.unwrap()
        else {
            panic!()
        };
        assert!(next_cursor.is_none());
        assert_eq!(records.len(), usize::from(present));
        if present {
            let TelemetryRecord::Operational(OperationalObservation { fact: OperationalFact::PageWait(row), .. }) =
                &records[0]
            else {
                panic!()
            };
            assert!(row.task_wait.is_none());
            assert_eq!(row.resolution, PageWaitResolution::ObservedAtBothBounds);
        }
        assert_eq!(source.finish().await.unwrap(), SourceCompletion::Verified);
        assert_eq!(source.close().await, SourceShutdown::Quiescent);
    }
}

fn focused_request() -> CollectionRequest {
    CollectionRequest {
        subject: CollectionSubject::Database,
        selection: CollectionSelection::FocusedPageWait {
            focus: Box::new(crate::activity::page_targets::test_focus(&page_wait(42, "PAGELATCH_EX", "7:1:4144"))),
        },
        budget: CollectionBudget::default(),
    }
}

#[test]
fn focused_factory_is_lazy_and_preserves_stricter_caller_bounds() {
    let mut request = focused_request();
    request.budget.max_queries = 2;
    request.budget.statement_timeout_ms = 50;
    request.budget.total_timeout_ms = 100;
    let bounded = bounded_request(&request).unwrap();
    assert_eq!(bounded.budget.max_snapshot_rows, 1);
    assert_eq!(bounded.budget.page_rows, 1);
    assert_eq!(bounded.budget.max_queries, 2);
    assert_eq!(bounded.budget.statement_timeout_ms, 50);
    assert_eq!(bounded.budget.total_timeout_ms, 100);
    let source = PageWaitSource::new(
        Arc::new(Pool::new(tiberius::Config::new(), None)),
        &request,
        "fixture.invalid".into(),
        1433,
    )
    .unwrap();
    assert_eq!(source.query_count(), 0);
}

#[tokio::test]
async fn disappeared_focus_remains_a_published_typed_outcome_with_the_original_parent() {
    let request = focused_request();
    let mut source = PageWaitSource::from_reader(
        reader(vec![Ok(vec![context()]), Ok(vec![context()]), Ok(vec![context()]), Ok(vec![context()])]),
        &request,
    )
    .unwrap();
    let description = source.describe().await.unwrap();
    assert!(description.matches_request(&request));
    let SourceObservation::FocusedPageWait { snapshot, result } = &description.observation else {
        panic!("focused result")
    };
    assert_eq!(snapshot.records, 0);
    assert_eq!(snapshot.sources[0].state, culprit_core::SourceState::Partial);
    assert_eq!(result.request, PageWaitRequestState::Missing);
    let CollectionSelection::FocusedPageWait { focus } = &request.selection else { panic!() };
    assert_eq!(result.focus, **focus);
    assert_eq!(source.query_count(), 3);
    assert!(matches!(source.read_page(TelemetryStream::OperationalSamples, None).await.unwrap(),
        CollectionRead::Page { records, next_cursor: None } if records.is_empty()));
    assert_eq!(source.query_count(), 3, "paging the retained terminal outcome performs no native work");
    assert_eq!(source.finish().await.unwrap(), SourceCompletion::Verified);
    assert_eq!(source.query_count(), 4);
    assert_eq!(source.close().await, SourceShutdown::Quiescent);
}

#[tokio::test]
async fn focused_page_rows_and_parent_descriptor_have_exact_export_identity() {
    let mut supplied = successful_replies(vec![page_wait(42, "PAGELATCH_EX", "7:1:4144")]);
    supplied.push(Ok(vec![context()]));
    let request = focused_request();
    let mut source = PageWaitSource::from_reader(reader(supplied), &request).unwrap();
    let description = source.describe().await.unwrap();
    assert!(description.matches_request(&request));
    assert!(matches!(description.observation.range().unwrap(), TelemetryRange::Focused { .. }));
    let CollectionRead::Page { records, next_cursor: None } =
        source.read_page(TelemetryStream::OperationalSamples, None).await.unwrap()
    else {
        panic!()
    };
    assert_eq!(records.len(), 1);
    let exported = ExportedRecord::new(
        &description,
        &CaptureIdentity { observation_id: "focused-child".into(), revision: None },
        records[0].clone(),
    )
    .unwrap();
    assert!(exported.record.is_valid());
    let TelemetryRecord::Operational(row) = exported.record else { panic!() };
    let OperationalFact::PageWait(page) = row.fact else { panic!() };
    assert!(page.target.is_some());
    assert_eq!(page.statement_identity.as_ref(), page.request.statement_identity.as_ref());
    assert_eq!(source.finish().await.unwrap(), SourceCompletion::Verified);
    assert_eq!(source.close().await, SourceShutdown::Quiescent);
}

#[tokio::test]
async fn a_retired_focused_native_request_cannot_authorize_any_later_sql_or_publication() {
    let mut native = FakeRead::new(vec![Ok(vec![context()])]).pause_on_query(2);
    native.shutdown = SourceShutdown::Unconfirmed;
    let mut source = PageWaitSource::from_reader(reader_with_native(native), &focused_request()).unwrap();
    assert!(tokio::time::timeout(std::time::Duration::from_millis(5), source.describe()).await.is_err());
    assert_eq!(source.query_count(), 2);
    assert_eq!(source.close().await, SourceShutdown::Unconfirmed);
    assert!(source.describe().await.is_err());
    assert!(source.finish().await.is_err());
    assert_eq!(source.query_count(), 2);
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
    let (scan, coverage) = crate::page_waits::collect_from(&mut scan_reader, false).await;
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

#[tokio::test]
async fn original_page_source_retains_only_the_already_read_native_database_role_without_more_queries() {
    for (value, expected) in
        [(Val::Text("READ_WRITE".into()), Some(true)), (Val::Text("READ_ONLY".into()), Some(false)), (Val::Null, None)]
    {
        let native = replace(&context(), &[("updateability", value)]);
        let replies =
            vec![Ok(vec![native.clone()]), Ok(vec![native.clone()]), Ok(vec![native.clone()]), Ok(vec![native])];
        let mut source = PageWaitSource::from_reader(reader(replies), &request()).unwrap();
        assert_eq!(source.query_count(), 0);
        let description = source.describe().await.unwrap_or_else(|error| {
            panic!("empty native role {expected:?}, queries {}: {error:?}", source.query_count())
        });
        let SourceObservation::PageWaitTargets { snapshot, database_writable, .. } = &description.observation else {
            panic!("native page role header")
        };
        assert_eq!(*database_writable, expected);
        assert_eq!(NativeReadContext::from_snapshot(&description).unwrap().database_writable, expected);
        assert!(description.matches_request(&request()));
        assert_eq!(snapshot.records, 0);
        assert!(!snapshot.sources[0].complete_scope);
        assert_eq!(source.query_count(), 3);
        let CollectionRead::Page { records, next_cursor } =
            source.read_page(TelemetryStream::OperationalSamples, None).await.unwrap()
        else {
            panic!("the empty native stream must still be drained")
        };
        assert!(records.is_empty());
        assert!(next_cursor.is_none());
        assert_eq!(source.finish().await.unwrap(), SourceCompletion::verified_if(expected.is_some()));
        assert_eq!(source.query_count(), 4, "only the original final guard owns one existing query");
        assert_eq!(source.close().await, SourceShutdown::Quiescent);
    }
}

#[tokio::test]
async fn original_mapped_page_body_and_its_native_role_share_the_existing_final_guard_and_close() {
    let mut replies = successful_replies(vec![page_wait(42, "PAGELATCH_EX", "7:1:4144")]);
    replies.push(Ok(vec![context()]));
    let mut source = PageWaitSource::from_reader(reader(replies), &request()).unwrap();
    let description = source.describe().await.unwrap();
    assert!(matches!(
        description.observation,
        SourceObservation::PageWaitTargets { database_writable: Some(true), .. }
    ));
    let CollectionRead::Page { records, next_cursor: None } =
        source.read_page(TelemetryStream::OperationalSamples, None).await.unwrap()
    else {
        panic!("native mapped page")
    };
    assert_eq!(records.len(), 1);
    assert_eq!(source.query_count(), 10);
    for record in records {
        ExportedRecord::new(
            &description,
            &CaptureIdentity { observation_id: "actual-native-page-role".into(), revision: None },
            record,
        )
        .unwrap();
    }
    assert_eq!(source.finish().await.unwrap(), SourceCompletion::Verified);
    assert_eq!(source.query_count(), 11);
    assert_eq!(source.close().await, SourceShutdown::Quiescent);
}

#[tokio::test]
async fn paired_task_population_echoes_the_original_capture_in_the_same_reader_without_more_native_calls() {
    let CollectionSelection::FocusedPageWait { focus } = focused_request().selection else { panic!() };
    let activity = focus.participant.parent.capture_ref();
    let request = CollectionRequest {
        subject: CollectionSubject::Database,
        selection: CollectionSelection::PageWaitTasks {
            activity: activity.clone(),
            original_requests: vec![ActivityRequestSelector {
                key: focus.participant.original.key.clone(),
                statement_identity: focus.participant.statement_identity.clone(),
            }],
            original_context: Box::new(NativeReadContext {
                target: focus.participant.target.clone(),
                partition: focus.participant.partition.clone(),
                observed_from: focus.participant.original_observed_at.clone(),
                observed_until: focus.participant.original_observed_at.clone(),
                database_writable: focus.participant.database_writable,
            }),
        },
        budget: CollectionBudget::default(),
    };
    let mut source =
        PageWaitSource::from_reader(reader((0..4).map(|_| Ok(vec![context()])).collect()), &request).unwrap();
    assert_eq!(source.query_count(), 0);
    let description = source.describe().await.unwrap();
    assert!(
        matches!(&description.observation, SourceObservation::PageWaitTargets { original_activity: Some(original), .. } if original == &activity)
    );
    assert!(description.matches_request(&request));
    assert!(matches!(description.observation.range().unwrap(), TelemetryRange::Focused { .. }));
    assert!(!description.matches_request(&super::tests::request()));
    let mut other = request.clone();
    let CollectionSelection::PageWaitTasks { activity, .. } = &mut other.selection else { panic!() };
    activity.manifest_sha256 = digest(b"different-original-capture");
    assert!(!description.matches_request(&other));
    assert_eq!(source.query_count(), 3);
    let CollectionRead::Page { records, next_cursor } =
        source.read_page(TelemetryStream::OperationalSamples, None).await.unwrap()
    else {
        panic!("the empty paired stream must still be drained")
    };
    assert!(records.is_empty());
    assert!(next_cursor.is_none());
    assert_eq!(source.finish().await.unwrap(), SourceCompletion::Verified);
    assert_eq!(source.query_count(), 4);
    assert_eq!(source.close().await, SourceShutdown::Quiescent);
    let recipe =
        crate::telemetry_recipes().into_iter().find(|recipe| recipe.selector == RecipeSelector::PageWaitTasks).unwrap();
    assert_eq!(recipe.purpose, RecipePurpose::Investigation);
    assert_eq!(recipe.cost, RecipeCostQualification::Unqualified);
    assert_eq!(recipe.subjects, [RecipeSubject::Database]);
}

#[tokio::test]
async fn known_empty_native_population_has_a_valid_partial_header_before_frozen_paging() {
    let mut owner = PageWaitReader { reader: reader((0..4).map(|_| Ok(vec![context()])).collect()), guard: None };
    let frame = culprit_read::ObservationReader::observe(&mut owner, &request())
        .await
        .unwrap_or_else(|error| panic!("empty native population, queries {}: {error:?}", owner.reader.query_count()));
    assert!(frame.records.is_empty());
    assert!(frame.description.partition_is_consistent(), "{:?}", frame.description);
    assert!(frame.description.observation.covers_request(), "{:?}", frame.description);
    assert!(frame.description.matches_request(&request()), "{:?}", frame.description);
    assert_eq!(owner.reader.query_count(), 3);
    assert!(culprit_read::ObservationReader::verify(&mut owner, &frame.description).await.unwrap());
    assert_eq!(owner.reader.query_count(), 4);
    assert_eq!(owner.reader.close().await, SourceShutdown::Quiescent);
}
