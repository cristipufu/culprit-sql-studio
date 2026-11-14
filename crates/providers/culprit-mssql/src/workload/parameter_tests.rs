//! Exercise native identity and variant enrichment through the real runtime-to-workload producer.
use super::identity_tests::{complete_replies, length_row, options, plan_row, runtime_replies};
use super::*;
use crate::{
    query_store::{
        Reader,
        tests::{definition_row, summary_row, variant_definition_row},
    },
    read::{ReadError, ReadResult},
    resources::test_support::{FakeRead, replace},
    value::{Rec, Val},
};
use culprit_core::{
    SourceState,
    telemetry::{CollectionStop, SourceShutdown},
};

const SQL: &str = "SELECT @p";

fn compiled_plan(id: i64, value: &str) -> Rec {
    let xml = format!(
        r#"<ShowPlanXML><BatchSequence><Batch><Statements>
<StmtSimple StatementText="SELECT @p" StatementSubTreeCost="0"><QueryPlan>
<ParameterList><ColumnReference Column="@p" ParameterCompiledValue="({value})" /></ParameterList>
<RelOp NodeId="0" PhysicalOp="Constant Scan" LogicalOp="Constant Scan" EstimateRows="1" EstimatedTotalSubtreeCost="0"><ConstantScan /></RelOp>
</QueryPlan></StmtSimple></Statements></Batch></BatchSequence></ShowPlanXML>"#
    );
    replace(
        &plan_row(),
        &[
            ("id", Val::Int(id)),
            ("definition_plan_id", Val::Int(id)),
            ("source_bytes", Val::Int((xml.encode_utf16().count() * 2) as i64)),
            ("payload", Val::Text(xml)),
        ],
    )
}

fn second_summary() -> Rec {
    replace(
        &summary_row(),
        &[("plan_id", Val::Int(5)), ("definition_plan_id", Val::Int(5)), ("duration_us", Val::Float(2000.0))],
    )
}

fn two_plans(definition: Rec) -> Vec<ReadResult<Vec<Rec>>> {
    let mut replies = complete_replies(definition_row(SQL), Some(compiled_plan(4, "2")), summary_row());
    replies[2] = Ok(vec![summary_row(), second_summary()]);
    let position = replies.len() - 1;
    replies.splice(position..position, [Ok(vec![length_row(&definition)]), Ok(vec![definition])]);
    replies
}

async fn assert_retired(reader: &mut Reader<FakeRead>, expected_reads: u32, stop: CollectionStop) {
    let error = reader.read_rows("SELECT /* culprit */ 1").await.unwrap_err();
    assert_eq!(CollectionStop::from_error(&error), stop);
    assert_eq!(reader.queries(), expected_reads, "no SQL after a terminal native identity/admission failure");
    assert_eq!(reader.close().await, SourceShutdown::Quiescent);
}

#[tokio::test]
async fn supplementary_compiled_values_require_the_original_query_and_plan_lifetimes() {
    let definition = replace(
        &compiled_plan(5, "999"),
        &[
            ("query_initial_compile_at", Val::Text("2020-01-01T08:00:00.000000000+02:00".into())),
            ("plan_initial_compile_at", Val::Text("2020-01-01T06:00:00.0000000Z".into())),
        ],
    );
    let mut reader = Reader::new(FakeRead::new(two_plans(definition)));
    let result = from_query_store(&mut reader, &options()).await.unwrap();
    let query = &result.workload.queries[0];
    assert_eq!(query.executions, 4.0);
    assert_eq!(query.total_time_ms, 6.0);
    assert_eq!(query.plans.iter().find(|plan| plan.id == "4").unwrap().compiled.as_deref(), Some("@p = (2)"));
    assert_eq!(query.plans.iter().find(|plan| plan.id == "5").unwrap().compiled.as_deref(), Some("@p = (999)"));
    assert_eq!(reader.queries(), 13);
    assert_eq!(reader.close().await, SourceShutdown::Quiescent);
}

#[tokio::test]
async fn captured_plan_parameters_do_not_get_assigned_to_the_heaviest_replaced_plan() {
    let mut replies = two_plans(compiled_plan(4, "2"));
    let newer = replace(
        &second_summary(),
        &[
            ("first_seen", Val::Text("2020-01-01T06:00:02Z".into())),
            ("last_seen", Val::Text("2020-01-01T06:00:03Z".into())),
        ],
    );
    replies[2] = Ok(vec![summary_row(), newer]);
    let captured = compiled_plan(5, "999");
    replies[6] = Ok(vec![length_row(&captured)]);
    replies[7] = Ok(vec![captured]);
    let mut reader = Reader::new(FakeRead::new(replies));
    let result = from_query_store(&mut reader, &options()).await.unwrap();
    let query = &result.workload.queries[0];
    assert_eq!(query.plan_id.as_deref(), Some("5"));
    assert_eq!(query.plans[0].id, "4", "runtime ordering remains unchanged");
    assert_eq!(query.plans[0].compiled.as_deref(), Some("@p = (2)"));
    assert_eq!(query.plans[1].compiled.as_deref(), Some("@p = (999)"));
    assert_eq!(reader.close().await, SourceShutdown::Quiescent);
}

#[tokio::test]
async fn recycled_missing_and_reparented_supplementary_definitions_stop_before_remaining_statistics() {
    for (field, value) in [
        ("query_initial_compile_at", Val::Text("2020-01-01T06:00:01Z".into())),
        ("query_hash", Val::Text("FFFFFFFFFFFFFFFF".into())),
        ("query_id", Val::Int(99)),
        ("definition_plan_id", Val::Null),
        ("plan_hash", Val::Text("FFFFFFFFFFFFFFFF".into())),
        ("plan_initial_compile_at", Val::Text("2020-01-01T06:00:01Z".into())),
    ] {
        let definition = replace(&compiled_plan(5, "999"), &[(field, value)]);
        let mut reader = Reader::new(FakeRead::new(two_plans(definition)));
        let error = from_query_store(&mut reader, &options()).await.err().expect(field);
        assert_eq!(CollectionStop::from_error(&error), CollectionStop::SourceChanged, "{field}");
        assert_retired(&mut reader, 12, CollectionStop::SourceChanged).await;
    }
}

#[tokio::test]
async fn unavailable_supplementary_text_does_not_invent_parameters_or_discard_runtime() {
    for reason in ["restricted", "encrypted"] {
        let definition = replace(&compiled_plan(5, "999"), &[(reason, Val::Bool(true)), ("payload", Val::Null)]);
        let mut reader = Reader::new(FakeRead::new(two_plans(definition)));
        let result = from_query_store(&mut reader, &options()).await.unwrap();
        assert_eq!(result.workload.queries[0].plans.len(), 2);
        assert_eq!(result.workload.queries[0].plans[1].compiled, None);
        assert_eq!(result.workload.total_time_ms, 6.0);
        assert_eq!(reader.close().await, SourceShutdown::Quiescent);
    }
}

#[tokio::test]
async fn selected_child_resolves_dispatcher_without_parent_runtime_and_never_recounts_it() {
    let mut replies = complete_replies(definition_row(SQL), Some(compiled_plan(4, "2")), summary_row());
    replies[5] = Ok(vec![variant_definition_row(SQL)]);
    let mut reader = Reader::new(FakeRead::new(replies));
    let result = from_query_store(&mut reader, &options()).await.unwrap();
    let query = &result.workload.queries[0];
    let family = query.variant_family.as_ref().unwrap();
    assert!(family.is_valid());
    assert_eq!(family.variant.query_id, "3");
    assert_eq!(family.parent.query_id, "1");
    assert_eq!(family.dispatcher.plan_id, "2");
    assert_eq!(result.workload.query_count, 1.0);
    assert_eq!(result.workload.total_executions, 2.0);
    assert_eq!(result.workload.total_time_ms, 4.0);
    assert_eq!(query.plans.len(), 1);
    assert_eq!(query.plans[0].executions, 2.0);
    assert_eq!(query.costs.as_ref().unwrap().failed, Some(1.0));
    assert_eq!(reader.queries(), 11, "family metadata shares the original definition read");
    assert_eq!(reader.close().await, SourceShutdown::Quiescent);
}

#[tokio::test]
async fn stale_or_incoherent_variant_relationships_cannot_label_the_original_runtime() {
    for changes in [
        vec![("query_initial_compile_at", Val::Text("2020-01-01T06:00:01Z".into()))],
        vec![("parent_query_id", Val::Int(3)), ("dispatcher_query_id", Val::Int(3))],
        vec![("dispatcher_query_id", Val::Int(99))],
        vec![("dispatcher_definition_plan_id", Val::Null)],
        vec![("parent_query_initial_compile_at", Val::Null)],
        vec![("query_id", Val::Int(99))],
    ] {
        let mut replies = complete_replies(definition_row(SQL), Some(compiled_plan(4, "2")), summary_row());
        replies[5] = Ok(vec![replace(&variant_definition_row(SQL), &changes)]);
        let mut reader = Reader::new(FakeRead::new(replies));
        let error = from_query_store(&mut reader, &options()).await.err().expect("unqualified native relationship");
        assert_eq!(CollectionStop::from_error(&error), CollectionStop::SourceChanged);
        assert_retired(&mut reader, 6, CollectionStop::SourceChanged).await;
    }
}

#[tokio::test]
async fn duplicate_relationship_rows_retire_the_reader_without_double_counting() {
    let mut replies = two_plans(compiled_plan(5, "999"));
    replies[5] = Ok(vec![variant_definition_row(SQL), variant_definition_row(SQL)]);
    let mut reader = Reader::new(FakeRead::new(replies));
    let error = from_query_store(&mut reader, &options()).await.err().expect("one relationship per native child");
    assert_eq!(CollectionStop::from_error(&error), CollectionStop::SourceChanged);
    assert_retired(&mut reader, 6, CollectionStop::SourceChanged).await;
}

#[tokio::test]
async fn unsupported_native_layout_skips_relationship_sql_and_keeps_unknown_family() {
    let mut replies = complete_replies(definition_row(SQL), None, summary_row());
    let original = replies[0].as_ref().unwrap()[0].clone();
    replies[0] = Ok(vec![replace(&original, &[("variants", Val::Int(0))])]);
    let mut reader = Reader::new(FakeRead::new(replies));
    let result = from_query_store(&mut reader, &SnapshotOptions { plans: 0, ..options() }).await.unwrap();
    assert!(result.workload.queries[0].variant_family.is_none());
    assert_eq!(result.workload.total_time_ms, 4.0);
    assert_eq!(reader.queries(), 9);
    assert_eq!(reader.close().await, SourceShutdown::Quiescent);
}

#[tokio::test]
async fn unverified_relationship_visibility_preserves_runtime_with_partial_coverage_no_fallback() {
    let mut replies = complete_replies(definition_row(SQL), None, summary_row());
    let original = replies[0].as_ref().unwrap()[0].clone();
    replies[0] = Ok(vec![replace(&original, &[("variants", Val::Int(2))])]);
    let mut reader = Reader::new(FakeRead::new(replies));
    let result = from_query_store(&mut reader, &SnapshotOptions { plans: 0, ..options() }).await.unwrap();
    assert!(result.workload.queries[0].variant_family.is_none());
    assert_eq!(result.workload.total_time_ms, 4.0);
    assert!(
        result
            .coverage
            .outcomes()
            .iter()
            .any(|source| source.source == crate::sources::QUERY_STORE.name && source.state == SourceState::Partial)
    );
    assert_eq!(reader.queries(), 9);
    assert_eq!(reader.close().await, SourceShutdown::Quiescent);
}

#[tokio::test]
async fn terminal_relationship_reads_never_fall_back_or_continue_statistics() {
    for stop in [CollectionStop::TimedOut, CollectionStop::Cancelled] {
        let mut replies = complete_replies(definition_row(SQL), None, summary_row());
        replies[5] = Err(ReadError::Stopped(stop));
        let mut reader = Reader::new(FakeRead::new(replies));
        let error = from_query_store(&mut reader, &SnapshotOptions { plans: 0, ..options() }).await.err().unwrap();
        assert_eq!(CollectionStop::from_error(&error), stop);
        assert_retired(&mut reader, 6, stop).await;
    }
}

#[test]
fn relationship_metadata_has_no_runtime_population_or_payload_traversal() {
    let sql = crate::query_store::catalog_sql::query_definitions(true);
    assert!(sql.contains("v.query_variant_query_id=ids.id"));
    assert!(sql.contains("OPTION (MAXDOP 1)"));
    for absent in ["query_store_runtime_stats", "query_store_runtime_stats_interval", "SUM(", "GROUP BY", "query_plan)"]
    {
        assert!(!sql.contains(absent), "{absent}");
    }
}

#[tokio::test]
async fn missing_original_plan_identity_stops_before_any_definition_read() {
    for field in ["definition_plan_id", "plan_hash", "plan_initial_compile_at"] {
        let mut reader =
            Reader::new(FakeRead::new(runtime_replies(vec![replace(&summary_row(), &[(field, Val::Null)])])));
        let error = from_query_store(&mut reader, &options()).await.err().expect("original plan identity required");
        assert_eq!(CollectionStop::from_error(&error), CollectionStop::SourceChanged);
        assert_retired(&mut reader, 3, CollectionStop::SourceChanged).await;
    }
}
