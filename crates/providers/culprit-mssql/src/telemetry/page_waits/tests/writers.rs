use super::*;
use crate::activity::page_targets::writers::{test_focus, test_replies};

async fn request() -> CollectionRequest {
    CollectionRequest {
        subject: CollectionSubject::Database,
        selection: CollectionSelection::PageWaitWriters { focus: Box::new(test_focus().await) },
        budget: CollectionBudget::default(),
    }
}

#[tokio::test]
async fn writer_source_is_lazy_and_catalog_envelope_preserves_each_stricter_caller_limit() {
    let mut request = request().await;
    request.budget.max_queries = 2;
    request.budget.max_sql_bytes = 2048;
    request.budget.max_plan_bytes = 1024;
    request.budget.statement_timeout_ms = 40;
    request.budget.total_timeout_ms = 80;
    let bounded = bounded_request(&request).unwrap();
    assert_eq!(bounded.budget.max_snapshot_rows, 1);
    assert_eq!(bounded.budget.page_rows, 1);
    assert_eq!(bounded.budget.max_queries, 2);
    assert_eq!(bounded.budget.max_sql_bytes, 2048);
    assert_eq!(bounded.budget.max_plan_bytes, 1024);
    assert_eq!(bounded.budget.statement_timeout_ms, 40);
    assert_eq!(bounded.budget.total_timeout_ms, 80);
    let source = PageWaitSource::new(
        Arc::new(Pool::new(tiberius::Config::new(), None)),
        &request,
        "fixture.invalid".into(),
        1433,
    )
    .unwrap();
    assert_eq!(source.query_count(), 0);
    let recipe =
        crate::telemetry_recipes().into_iter().find(|recipe| recipe.id == "mssql.focused-page-wait-writers").unwrap();
    assert_eq!(recipe.selector, RecipeSelector::FocusedPageWaitWriters);
    assert_eq!(recipe.purpose, RecipePurpose::Investigation);
    assert_eq!(recipe.cost, RecipeCostQualification::Unqualified);
    assert_eq!(recipe.subjects, [RecipeSubject::Database]);
}

#[tokio::test]
async fn writer_source_publishes_one_partial_aggregate_and_verifies_the_same_owner_before_close() {
    let request = request().await;
    let mut replies = test_replies();
    replies.push(Ok(vec![context()]));
    let mut source = PageWaitSource::from_reader(reader(replies), &request).unwrap();
    let description = source.describe().await.unwrap();
    assert!(description.matches_request(&request));
    let SourceObservation::PageWaitWriters { snapshot, focus, database_writable } = &description.observation else {
        panic!("writer header")
    };
    assert_eq!(snapshot.records, 1);
    assert!(!snapshot.sources[0].complete_scope);
    assert_eq!(snapshot.sources[0].state, culprit_core::SourceState::Partial);
    assert_eq!(*database_writable, focus.waiter.participant().database_writable);
    let CollectionRead::Page { records, next_cursor } =
        source.read_page(TelemetryStream::OperationalSamples, None).await.unwrap()
    else {
        panic!("native row")
    };
    assert_eq!(records.len(), 1);
    assert!(next_cursor.is_none());
    assert_eq!(source.query_count(), 17);
    assert!(
        matches!(&records[0],TelemetryRecord::Operational(OperationalObservation {fact:OperationalFact::PageWaitWriters(row),..}) if row.candidates[0].relation==PageWriterRelation::TargetObjectWrite)
    );
    assert_eq!(source.finish().await.unwrap(), SourceCompletion::Verified);
    assert_eq!(source.query_count(), 18);
    assert_eq!(source.close().await, SourceShutdown::Quiescent);
}
