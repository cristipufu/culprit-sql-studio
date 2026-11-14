use super::*;
use crate::{
    query_store::{
        Reader,
        tests::{definition_row, summary_row},
        window_sql::tests::row as window_row,
    },
    read::{ReadError, ReadResult},
    resources::test_support::{FakeRead, replace},
    value::{Rec, Val},
};
use culprit_core::{
    SourceState, source_id,
    telemetry::{CollectionStop, SourceShutdown},
};

const SQL: &str = "SELECT 1";
const PLAN: &str = r#"<ShowPlanXML><BatchSequence><Batch><Statements>
<StmtSimple StatementText="SELECT 1" StatementSubTreeCost="0">
<QueryPlan><RelOp NodeId="0" PhysicalOp="Constant Scan" LogicalOp="Constant Scan"
EstimateRows="1" EstimatedTotalSubtreeCost="0"><ConstantScan /></RelOp></QueryPlan>
</StmtSimple></Statements></Batch></BatchSequence></ShowPlanXML>"#;

pub(super) fn options() -> SnapshotOptions {
    SnapshotOptions { window_minutes: 60, plans: 1, ..Default::default() }
}

fn unavailable_waits_window() -> Rec {
    replace(&window_row(), &[("wait_stats_capture_mode_desc", Val::Null)])
}

pub(super) fn runtime_replies(summary: Vec<Rec>) -> Vec<ReadResult<Vec<Rec>>> {
    vec![
        Ok(vec![Rec::of(&[
            ("tempdb", Val::Int(0)),
            ("log_bytes", Val::Int(0)),
            ("waits", Val::Int(0)),
            ("variants", Val::Int(1)),
        ])]),
        Ok(vec![unavailable_waits_window()]),
        Ok(summary),
    ]
}

pub(super) fn length_row(definition: &Rec) -> Rec {
    Rec::of(
        &definition
            .columns()
            .filter(|(name, _)| matches!(*name, "id" | "source_bytes"))
            .map(|(name, value)| (name, value.clone()))
            .collect::<Vec<_>>(),
    )
}

fn definition_replies(definition: Rec) -> Vec<ReadResult<Vec<Rec>>> {
    let mut replies = runtime_replies(vec![summary_row()]);
    replies.extend([
        Ok(vec![]), // Optional missing-index askers.
        Ok(vec![length_row(&definition)]),
        Ok(vec![definition]),
    ]);
    replies
}

pub(super) fn plan_row() -> Rec {
    let row = replace(&definition_row(PLAN), &[("id", Val::Int(4))]);
    let mut fields = row.columns().map(|(name, value)| (name, value.clone())).collect::<Vec<_>>();
    fields.extend([
        ("definition_plan_id", Val::Int(4)),
        ("plan_hash", Val::Text("0011223344556677".into())),
        ("plan_initial_compile_at", Val::Text("2020-01-01T06:00:00Z".into())),
    ]);
    Rec::of(&fields)
}

pub(super) fn complete_replies(definition: Rec, plan: Option<Rec>, summary: Rec) -> Vec<ReadResult<Vec<Rec>>> {
    let mut replies = definition_replies(definition);
    replies[2] = Ok(vec![summary]);
    if let Some(plan) = plan {
        replies.extend([Ok(vec![length_row(&plan)]), Ok(vec![plan])]);
    }
    replies.extend([
        Ok(vec![]), // Timeline interval layout.
        Ok(vec![]), // No timeline population.
        Ok(vec![unavailable_waits_window()]),
    ]);
    replies
}

#[tokio::test]
async fn original_runtime_identity_authorizes_definition_plan_and_same_population_costs() {
    let definition = replace(
        &definition_row(SQL),
        &[("query_initial_compile_at", Val::Text("2020-01-01T08:00:00.000000000+02:00".into()))],
    );
    let mut reader = Reader::new(FakeRead::new(complete_replies(definition, Some(plan_row()), summary_row())));
    let result = from_query_store(&mut reader, &options()).await.unwrap();
    let query = &result.workload.queries[0];
    assert_eq!(query.id, "3");
    assert_eq!(query.text, SQL);
    assert_eq!(query.executions, 2.0);
    assert_eq!(query.total_time_ms, 4.0);
    assert_eq!(query.costs.as_ref().unwrap().failed, Some(1.0));
    assert_eq!(query.plan_id.as_deref(), Some("4"));
    assert!(query.plan.is_some());
    assert_eq!(result.plans_parsed, 1);
    assert_eq!(result.plan_errors, 0);
    let original = crate::query_store::decode::query_identity(&summary_row()).unwrap();
    assert_eq!(result.query_identities.get("3"), Some(&original));
    assert_eq!(result.workload.window_hours, 1.0);
    assert_eq!(result.workload.window_end.as_deref(), Some("2020-01-01T07:00:00Z"));
    assert_eq!(reader.queries(), 11, "outcomes and log volume require no later runtime read");
    assert_eq!(reader.close().await, SourceShutdown::Quiescent);
}

#[tokio::test]
async fn conflicting_summary_identities_stop_before_definitions_or_other_statistics() {
    for (field, changed) in [
        ("context_settings_id", Val::Int(6)),
        ("object_id", Val::Int(99)),
        ("query_hash", Val::Text("FEDCBA9876543210".into())),
        ("query_initial_compile_at", Val::Text("2020-01-01T06:00:01Z".into())),
    ] {
        let conflicting =
            replace(&summary_row(), &[("plan_id", Val::Int(5)), ("definition_plan_id", Val::Int(5)), (field, changed)]);
        let mut reader = Reader::new(FakeRead::new(runtime_replies(vec![summary_row(), conflicting])));
        let error = from_query_store(&mut reader, &options()).await.err().expect("conflicting native lifetime");
        assert_eq!(CollectionStop::from_error(&error), CollectionStop::SourceChanged, "{field}");
        assert!(reader.read_rows("SELECT /* culprit */ 1").await.is_err());
        assert_eq!(reader.queries(), 3);
        assert_eq!(reader.close().await, SourceShutdown::Quiescent);
    }
}

#[tokio::test]
async fn missing_summary_identity_does_not_borrow_a_later_definition() {
    for field in ["query_id", "context_settings_id", "object_id", "query_hash", "query_initial_compile_at"] {
        let row = replace(&summary_row(), &[(field, Val::Null)]);
        let mut reader = Reader::new(FakeRead::new(runtime_replies(vec![row])));
        let error = from_query_store(&mut reader, &options()).await.err().expect("unknown native lifetime");
        assert_eq!(CollectionStop::from_error(&error), CollectionStop::SourceChanged, "{field}");
        assert!(reader.read_rows("SELECT /* culprit */ 1").await.is_err());
        assert_eq!(reader.queries(), 3);
        assert_eq!(reader.close().await, SourceShutdown::Quiescent);
    }
}

#[tokio::test]
async fn changed_definition_identity_stops_before_any_plan_read_even_when_query_ids_match() {
    for (field, changed) in [
        ("context_settings_id", Val::Int(6)),
        ("object_id", Val::Int(99)),
        ("query_hash", Val::Text("FEDCBA9876543210".into())),
        ("query_initial_compile_at", Val::Text("2020-01-01T06:00:01Z".into())),
    ] {
        let definition = replace(&definition_row(SQL), &[(field, changed)]);
        let mut reader = Reader::new(FakeRead::new(definition_replies(definition)));
        let error = from_query_store(&mut reader, &options()).await.err().expect("query lifetime changed");
        assert_eq!(CollectionStop::from_error(&error), CollectionStop::SourceChanged, "{field}");
        assert!(reader.read_rows("SELECT /* culprit */ 1").await.is_err());
        assert_eq!(reader.queries(), 6);
        assert_eq!(reader.close().await, SourceShutdown::Quiescent);
    }
}

#[tokio::test]
async fn missing_definition_identity_with_no_sql_is_not_a_match() {
    for field in ["query_id", "context_settings_id", "object_id", "query_hash", "query_initial_compile_at"] {
        let missing =
            replace(&definition_row(SQL), &[(field, Val::Null), ("source_bytes", Val::Null), ("payload", Val::Null)]);
        let mut reader = Reader::new(FakeRead::new(definition_replies(missing)));
        let error = from_query_store(&mut reader, &options()).await.err().expect("original definition disappeared");
        assert_eq!(CollectionStop::from_error(&error), CollectionStop::SourceChanged, "{field}");
        assert!(reader.read_rows("SELECT /* culprit */ 1").await.is_err());
        assert_eq!(reader.queries(), 6);
        assert_eq!(reader.close().await, SourceShutdown::Quiescent);
    }
}

#[tokio::test]
async fn restricted_encrypted_or_missing_text_does_not_erase_a_known_native_identity() {
    for metadata in [
        vec![("encrypted", Val::Bool(true)), ("payload", Val::Null)],
        vec![("restricted", Val::Bool(true)), ("payload", Val::Null)],
        vec![("source_bytes", Val::Null), ("payload", Val::Null)],
    ] {
        let definition = replace(&definition_row(SQL), &metadata);
        let mut reader = Reader::new(FakeRead::new(complete_replies(definition, None, summary_row())));
        let result = from_query_store(&mut reader, &SnapshotOptions { plans: 0, ..options() }).await.unwrap();
        let query = &result.workload.queries[0];
        assert!(query.text.is_empty());
        assert!(query.plan.is_none());
        assert_eq!(query.executions, 2.0);
        assert_eq!(query.total_time_ms, 4.0);
        assert_eq!(query.costs.as_ref().unwrap().failed, Some(1.0));
        assert_eq!(result.query_identities.len(), 1);
        assert_eq!(reader.queries(), 9);
        assert_eq!(reader.close().await, SourceShutdown::Quiescent);
    }
}

#[tokio::test]
async fn a_plan_reassigned_after_the_text_read_cannot_attach_to_original_runtime() {
    let changed = replace(&plan_row(), &[("query_initial_compile_at", Val::Text("2020-01-01T06:00:01Z".into()))]);
    let mut replies = definition_replies(definition_row(SQL));
    replies.extend([Ok(vec![length_row(&changed)]), Ok(vec![changed])]);
    let mut reader = Reader::new(FakeRead::new(replies));
    let error = from_query_store(&mut reader, &options()).await.err().expect("plan now names another lifetime");
    assert_eq!(CollectionStop::from_error(&error), CollectionStop::SourceChanged);
    assert!(reader.read_rows("SELECT /* culprit */ 1").await.is_err());
    assert_eq!(reader.queries(), 8);
    assert_eq!(reader.close().await, SourceShutdown::Quiescent);
}

#[tokio::test]
async fn terminal_definition_failure_preserves_shutdown_and_forbids_followup() {
    for (stop, shutdown) in [
        (CollectionStop::TimedOut, SourceShutdown::Quiescent),
        (CollectionStop::QueryLimit, SourceShutdown::Quiescent),
        (CollectionStop::CancellationUnconfirmed, SourceShutdown::Unconfirmed),
    ] {
        let mut replies = runtime_replies(vec![summary_row()]);
        replies.extend([Ok(vec![]), Err(ReadError::Stopped(stop))]);
        let mut native = FakeRead::new(replies);
        native.shutdown = shutdown;
        let mut reader = Reader::new(native);
        let error = from_query_store(&mut reader, &options()).await.err().expect("bounded source stopped");
        assert_eq!(CollectionStop::from_error(&error), stop);
        assert!(reader.read_rows("SELECT /* culprit */ 1").await.is_err());
        assert_eq!(reader.queries(), 5);
        assert_eq!(reader.close().await, shutdown);
    }
}

fn with_log_capability(mut replies: Vec<ReadResult<Vec<Rec>>>) -> Vec<ReadResult<Vec<Rec>>> {
    replies[0] = Ok(vec![Rec::of(&[
        ("tempdb", Val::Int(0)),
        ("log_bytes", Val::Int(1)),
        ("waits", Val::Int(0)),
        ("variants", Val::Int(1)),
    ])]);
    replies
}

#[tokio::test]
async fn equal_query_identity_and_counts_after_reset_cannot_replace_the_original_execution_costs() {
    let original = replace(&summary_row(), &[("log_bytes", Val::Float(100.0))]);
    let reset = replace(
        &original,
        &[
            ("failed", Val::Int(0)),
            ("duration_us", Val::Float(2000.0)),
            ("log_bytes", Val::Float(900.0)),
            ("first_seen", Val::Text("2020-01-01T06:00:02Z".into())),
            ("last_seen", Val::Text("2020-01-01T06:00:02.1Z".into())),
        ],
    );
    let before = crate::query_store::runtime::PlanSummary::decode(&original).unwrap();
    let after = crate::query_store::runtime::PlanSummary::decode(&reset).unwrap();
    assert_eq!(before.identity, after.identity);
    assert_eq!(before.moments.executions, after.moments.executions);
    assert_ne!(before.first_seen, after.first_seen);
    // Subsequent reads may observe a reset source. Neither the first producer nor the next capture can borrow
    // the other's outcomes/log numerator; each keeps sufficient statistics from its own original summary.
    let mut replies = with_log_capability(complete_replies(definition_row(SQL), None, original));
    replies.extend(complete_replies(definition_row(SQL), None, reset).into_iter().skip(2));
    let mut reader = Reader::new(FakeRead::new(replies));
    let first = from_query_store(&mut reader, &SnapshotOptions { plans: 0, ..options() }).await.unwrap();
    assert_eq!(reader.queries(), 9, "no independent later cost query after the original summary");
    let second = from_query_store(&mut reader, &SnapshotOptions { plans: 0, ..options() }).await.unwrap();
    let first_query = &first.workload.queries[0];
    let second_query = &second.workload.queries[0];
    assert_eq!(first_query.executions, second_query.executions);
    assert_eq!(first.query_identities, second.query_identities);
    assert_eq!(first_query.costs.as_ref().unwrap().failed, Some(1.0));
    assert_eq!(first_query.costs.as_ref().unwrap().log_bytes, Some(100.0));
    assert_eq!(first_query.costs.as_ref().unwrap().log_share, Some(1.0));
    assert_eq!(second_query.costs.as_ref().unwrap().failed, Some(0.0));
    assert_eq!(second_query.costs.as_ref().unwrap().log_bytes, Some(900.0));
    assert_eq!(second_query.costs.as_ref().unwrap().log_share, Some(1.0));
    assert_eq!(reader.queries(), 16, "cached layout/window reuse needs no separate family or runtime-cost pass");
    assert_eq!(reader.close().await, SourceShutdown::Quiescent);
}

#[tokio::test]
async fn incomplete_unselected_log_population_withholds_shares_but_keeps_known_query_volume() {
    let own = replace(&summary_row(), &[("log_bytes", Val::Float(100.0))]);
    let other = replace(
        &summary_row(),
        &[
            ("query_id", Val::Int(7)),
            ("plan_id", Val::Int(8)),
            ("definition_plan_id", Val::Int(8)),
            ("duration_us", Val::Float(1000.0)),
            ("log_complete", Val::Int(0)),
        ],
    );
    let mut replies = with_log_capability(complete_replies(definition_row(SQL), None, own.clone()));
    replies[2] = Ok(vec![own, other]);
    let mut reader = Reader::new(FakeRead::new(replies));
    let result =
        from_query_store(&mut reader, &SnapshotOptions { plans: 0, top_queries: 1, ..options() }).await.unwrap();
    assert_eq!(result.workload.query_count, 2.0);
    assert_eq!(result.workload.queries.len(), 1);
    let costs = result.workload.queries[0].costs.as_ref().unwrap();
    assert_eq!(costs.log_bytes, Some(100.0));
    assert_eq!(costs.log_share, None, "SUM cannot skip unknown rows and label the result a complete denominator");
    assert_eq!(costs.failed, Some(1.0));
    assert!(
        result
            .coverage
            .outcomes()
            .iter()
            .any(|source| { source.source == source_id::QUERY_COSTS && source.state == SourceState::Partial })
    );
    assert_eq!(reader.queries(), 9);
    assert_eq!(reader.close().await, SourceShutdown::Quiescent);
}

#[tokio::test]
async fn same_population_costs_do_not_bypass_retained_native_interval_verification() {
    let mut replies = complete_replies(definition_row(SQL), None, summary_row());
    *replies.last_mut().unwrap() =
        Ok(vec![replace(&unavailable_waits_window(), &[("intervals_retained", Val::Int(0))])]);
    let mut reader = Reader::new(FakeRead::new(replies));
    let error = from_query_store(&mut reader, &SnapshotOptions { plans: 0, ..options() })
        .await
        .err()
        .expect("interval membership must still be retained");
    assert_eq!(CollectionStop::from_error(&error), CollectionStop::SourceChanged);
    assert!(reader.read_rows("SELECT /* culprit */ 1").await.is_err());
    assert_eq!(reader.queries(), 9);
    assert_eq!(reader.close().await, SourceShutdown::Quiescent);
}
