use super::*;
use crate::{
    activity::page_targets::holders::{test_focus, test_replies},
    read::ReadError,
    value::{Rec, Val},
};

async fn request() -> CollectionRequest {
    CollectionRequest {
        subject: CollectionSubject::Database,
        selection: CollectionSelection::PageTaskHolders { focus: Box::new(test_focus().await) },
        budget: CollectionBudget::default(),
    }
}

fn with_verify() -> Vec<crate::read::ReadResult<Vec<Rec>>> {
    let mut replies = test_replies();
    replies.push(Ok(vec![context()]));
    replies
}

#[tokio::test]
async fn holder_recipe_is_selector_required_investigation_only_lazy_and_respects_every_stricter_dimension() {
    let mut request = request().await;
    request.budget = CollectionBudget {
        page_rows: 1,
        max_snapshot_rows: 1,
        page_bytes: 1024,
        total_bytes: 2048,
        max_sql_bytes: 1024,
        max_plan_bytes: 1024,
        max_pages: 1,
        max_queries: 2,
        statement_timeout_ms: 20,
        total_timeout_ms: 40,
    };
    let bounded = bounded_request(&request).unwrap();
    assert_eq!(bounded.budget, request.budget);
    let source = PageWaitSource::new(
        Arc::new(Pool::new(tiberius::Config::new(), None)),
        &request,
        "fixture.invalid".into(),
        1433,
    )
    .unwrap();
    assert_eq!(source.query_count(), 0);
    let recipe =
        crate::telemetry_recipes().into_iter().find(|row| row.id == "mssql.focused-page-task-holders").unwrap();
    assert_eq!(recipe.selector, RecipeSelector::FocusedPageTaskHolders);
    assert_eq!(recipe.purpose, RecipePurpose::Investigation);
    assert_eq!(recipe.cost, RecipeCostQualification::Unqualified);
    assert_eq!(recipe.subjects, [RecipeSubject::Database]);
    assert_eq!(recipe.budget.max_queries, 28);
    assert_eq!(recipe.budget.statement_timeout_ms, 1000);
    assert_eq!(recipe.budget.total_timeout_ms, 5000);
    request.selection = CollectionSelection::Snapshot { dataset: SnapshotDataset::PageTaskHolders };
    assert!(request.validate().is_err());
    assert!(
        PageWaitSource::new(
            Arc::new(Pool::new(tiberius::Config::new(), None)),
            &request,
            "fixture.invalid".into(),
            1433
        )
        .is_err()
    );
}

#[tokio::test]
async fn one_partial_aggregate_exports_exact_task_holder_identity_and_is_verified_on_the_same_native_owner() {
    let request = request().await;
    let mut source = PageWaitSource::from_reader(reader(with_verify()), &request).unwrap();
    let description = source.describe().await.unwrap();
    assert!(description.matches_request(&request));
    let SourceObservation::PageTaskHolders { snapshot, focus, database_writable } = &description.observation else {
        panic!("holder source")
    };
    assert_eq!(snapshot.dataset, SnapshotDataset::PageTaskHolders);
    assert_eq!(snapshot.records, 1);
    assert_eq!(snapshot.sources[0].state, culprit_core::SourceState::Partial);
    assert!(!snapshot.sources[0].complete_scope);
    assert_eq!(*database_writable, focus.participant.database_writable);
    assert_eq!(source.query_count(), 15);
    let CollectionRead::Page { records, next_cursor: None } =
        source.read_page(TelemetryStream::OperationalSamples, None).await.unwrap()
    else {
        panic!("holder row")
    };
    assert_eq!(records.len(), 1);
    let canonical = ExportedRecord::new(
        &description,
        &CaptureIdentity { observation_id: "holder-export".into(), revision: None },
        records[0].clone(),
    )
    .unwrap();
    let TelemetryRecord::Operational(row) = &canonical.record else { panic!("operational row") };
    let OperationalFact::PageTaskHolders(point) = &row.fact else { panic!("task holder body") };
    assert_eq!(point.before.holder.relation, PageTaskHolderRelation::QualifiedRequest);
    assert_eq!(point.before.holder.request.as_ref().unwrap().key.session.id, "43");
    assert_eq!(point.page.as_ref().unwrap().request.key.session.id, "42");
    assert_eq!(source.finish().await.unwrap(), SourceCompletion::Verified);
    assert_eq!(source.query_count(), 16);
    assert_eq!(source.close().await, SourceShutdown::Quiescent);
}

#[tokio::test]
async fn optional_sql_permission_and_size_failures_preserve_native_actor_and_distinct_payload_and_coverage_reasons() {
    let request = request().await;
    for oversized in [false, true] {
        let mut replies = with_verify();
        replies[10] = if oversized {
            let row = &replies[10].as_ref().unwrap()[0];
            Ok(vec![replace(row, &[("source_bytes", Val::Int(1_000_000)), ("payload", Val::Null)])])
        } else {
            Err(ReadError::Source(culprit_core::Error::new(
                culprit_core::ErrorCode::PermissionDenied,
                "controlled SQL access denial",
            )))
        };
        let mut source = PageWaitSource::from_reader(reader(replies), &request).unwrap();
        let description = source.describe().await.unwrap();
        let coverage = &description.observation.snapshot().unwrap().sources[0];
        assert_eq!(
            coverage.stopped,
            Some(if oversized { CollectionStop::ByteLimit } else { CollectionStop::SourceUnavailable })
        );
        assert_eq!(coverage.error_code, (!oversized).then_some(culprit_core::ErrorCode::PermissionDenied));
        let CollectionRead::Page { records, .. } =
            source.read_page(TelemetryStream::OperationalSamples, None).await.unwrap()
        else {
            panic!("row")
        };
        let TelemetryRecord::Operational(row) = &records[0] else { panic!("operational") };
        let OperationalFact::PageTaskHolders(point) = &row.fact else { panic!("holder") };
        assert_eq!(point.before.wait, PageWaitRelation::OriginalObserved);
        assert_eq!(point.before.holder.relation, PageTaskHolderRelation::QualifiedRequest);
        assert!(matches!(&point.before.holder.request.as_ref().unwrap().statement,
            DefinitionPayload::Unavailable {reason} if *reason==if oversized {PayloadUnavailable::Oversized} else {PayloadUnavailable::SourceUnavailable}));
        assert_eq!(source.finish().await.unwrap(), SourceCompletion::Verified);
        assert_eq!(source.close().await, SourceShutdown::Quiescent);
    }
}

#[tokio::test]
async fn denied_task_visibility_is_unavailable_not_disappearance_and_performs_no_payload_or_page_work() {
    let request = request().await;
    let replies = test_replies();
    let supplied = &replies[1].as_ref().unwrap()[0];
    let hidden = Rec::of(
        &supplied
            .columns()
            .map(|(name, value)| {
                (
                    name,
                    if name == "task_visibility" {
                        Val::Bool(false)
                    } else if name.starts_with("task_") || name.starts_with("checked_task_") {
                        Val::Null
                    } else {
                        value.clone()
                    },
                )
            })
            .collect::<Vec<_>>(),
    );
    let mut source = PageWaitSource::from_reader(
        reader(vec![
            Ok(vec![context()]),
            Ok(vec![hidden.clone()]),
            Ok(vec![context()]),
            Ok(vec![hidden]),
            Ok(vec![context()]),
            Ok(vec![context()]),
        ]),
        &request,
    )
    .unwrap();
    let description = source.describe().await.unwrap();
    let snapshot = description.observation.snapshot().unwrap();
    assert_eq!(snapshot.sources[0].stopped, Some(CollectionStop::SourceUnavailable));
    assert_eq!(snapshot.sources[0].error_code, None);
    assert_eq!(snapshot.records, 1);
    assert_eq!(source.query_count(), 5);
    let CollectionRead::Page { records, .. } =
        source.read_page(TelemetryStream::OperationalSamples, None).await.unwrap()
    else {
        panic!("row")
    };
    let TelemetryRecord::Operational(row) = &records[0] else { panic!("operational") };
    let OperationalFact::PageTaskHolders(point) = &row.fact else { panic!("holder") };
    assert_eq!(point.target_state, PageWriterTargetState::Unavailable);
    assert!(point.page.is_none());
    assert_eq!(point.before.wait, PageWaitRelation::Unqualified);
    assert_eq!(point.after.wait, PageWaitRelation::Unqualified);
    assert_eq!(point.before.holder.relation, PageTaskHolderRelation::Unavailable);
    assert_eq!(source.finish().await.unwrap(), SourceCompletion::Verified);
    assert_eq!(source.query_count(), 6);
    assert_eq!(source.close().await, SourceShutdown::Quiescent);
}

#[tokio::test]
async fn cancelled_or_unconfirmed_holder_source_never_verifies_or_reopens_the_reader() {
    let request = request().await;
    let mut native = FakeRead::new(with_verify()).pause_on_query(2);
    native.shutdown = SourceShutdown::Unconfirmed;
    let mut source = PageWaitSource::from_reader(reader_with_native(native), &request).unwrap();
    assert!(tokio::time::timeout(std::time::Duration::from_millis(5), source.describe()).await.is_err());
    assert_eq!(source.query_count(), 2);
    assert_eq!(source.close().await, SourceShutdown::Unconfirmed);
    assert!(source.describe().await.is_err());
    assert!(source.finish().await.is_err());
    assert!(source.read_page(TelemetryStream::OperationalSamples, None).await.is_err());
    assert_eq!(source.query_count(), 2);
}
