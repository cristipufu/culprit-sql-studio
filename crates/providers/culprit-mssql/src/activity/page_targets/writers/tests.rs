use super::*;
use crate::{
    activity::page_targets::tests::{context, header, page_wait, participant, reader, successful_replies},
    read::{ReadError, ReadResult},
    resources::test_support::{FakeRead, replace},
    value::{Rec, Val},
};

const STATEMENT: &str = "INSERT INTO dbo.orders(id) VALUES (1)";

fn plan_xml(statement: &str, physical: &str, logical: &str, object: &str) -> String {
    let statement_type = if logical == "Insert" { "INSERT" } else { "SELECT" };
    format!(
        "<ShowPlanXML><BatchSequence><Batch><Statements><StmtSimple StatementText=\"{statement}\" StatementType=\"{statement_type}\"><QueryPlan><RelOp NodeId=\"1\" PhysicalOp=\"{physical}\" LogicalOp=\"{logical}\" EstimateRows=\"1\" EstimateRebinds=\"0\" EstimateRewinds=\"0\" EstimateCPU=\"0.001\" EstimateIO=\"0.01\" EstimatedTotalSubtreeCost=\"0.1\" Parallel=\"false\"><Update><Object Database=\"[shop]\" Schema=\"[dbo]\" Table=\"[{object}]\" Index=\"[PK_orders]\"/></Update></RelOp></QueryPlan></StmtSimple></Statements></Batch></BatchSequence></ShowPlanXML>"
    )
}
fn write_plan() -> String {
    plan_xml(STATEMENT, "Clustered Index Insert", "Insert", "orders")
}

pub(crate) async fn writer_focus() -> PageWaitWriterFocus {
    let original = page_wait(42, "PAGELATCH_EX", "7:1:4144");
    let waiter = super::super::test_focus(&original);
    let mut initial = reader(successful_replies(vec![original]));
    let captured = initial.capture_focused_page_wait_targets(&waiter).await.unwrap();
    assert_eq!(initial.close().await, SourceShutdown::Quiescent);
    let page = &captured.capture.snapshot.observations[0];
    let candidate = crate::activity::rows::record(&crate::activity::tests::metadata(43)).unwrap().unwrap().facts;
    let mut parent = waiter.participant().parent.clone();
    parent.native_key = NativeRecordIdentity::Activity(candidate.key.clone());
    parent.record_id = digest(b"original-candidate");
    parent.row_ordinal = 1;
    let mut reference = waiter.participant().parent.clone();
    reference.capture.observation_id = "original-page".into();
    reference.stream = TelemetryStream::OperationalSamples;
    reference.batch_id = batch_id(&reference.capture.observation_id, reference.stream).unwrap();
    reference.native_key = NativeRecordIdentity::Operational {
        source: crate::sources::PAGE_WAIT_TARGETS.name.into(),
        key: page.key().unwrap(),
    };
    reference.record_id = digest(b"original-page");
    let focus = PageWaitWriterFocus {
        waiter,
        page: reference,
        address: page.observed_address.unwrap(),
        target: page.target.clone().unwrap(),
        candidates: vec![PageWaitWriterRequest {
            parent,
            key: candidate.key,
            statement_identity: candidate.statement_identity.unwrap(),
        }],
    };
    assert!(focus.is_valid());
    focus
}

fn metadata_point(ordinal: usize, row: &Rec) -> Rec {
    let checked = participant(ordinal, row);
    let mut columns = row.columns().map(|(name, value)| (name, value.clone())).collect::<Vec<_>>();
    for (name, value) in checked.columns() {
        if let Some((_, existing)) = columns.iter_mut().find(|(key, _)| *key == name) {
            *existing = value.clone();
        } else {
            columns.push((name, value.clone()));
        }
    }
    Rec::of(&columns)
}
fn payload(statement: &str, plan: &str) -> Rec {
    Rec::of(&[
        ("ordinal", Val::Int(0)),
        ("statement_bytes", Val::Int((statement.encode_utf16().count() * 2) as i64)),
        ("statement", Val::Text(statement.into())),
        ("plan_bytes", Val::Int((plan.encode_utf16().count() * 2) as i64)),
        ("plan", Val::Text(plan.into())),
        ("encrypted", Val::Bool(false)),
    ])
}
pub(crate) fn replies() -> Vec<ReadResult<Vec<Rec>>> {
    let wait = page_wait(42, "PAGELATCH_EX", "7:1:4144");
    let writer = crate::activity::tests::metadata(43);
    let mut replies = successful_replies(vec![wait.clone()]);
    let healthy = replies[3].as_ref().unwrap().clone();
    replies.extend([
        Ok(vec![metadata_point(0, &writer), metadata_point(1, &wait)]),
        Ok(healthy.clone()),
        Ok(vec![payload(STATEMENT, &write_plan())]),
        Ok(vec![participant(0, &writer), participant(1, &wait)]),
        Ok(healthy),
        Ok(vec![header(0, 4144)]),
        Ok(vec![context()]),
    ]);
    replies
}

#[tokio::test]
async fn writer_investigation_reads_only_original_handles_and_keeps_waiters_separate_from_planned_writers() {
    let focus = writer_focus().await;
    let mut owner = reader(replies());
    let captured = owner.capture_page_wait_writers(&focus).await.unwrap();
    assert!(focus.accepts_observation(&captured.context, &captured.observation));
    let writer = &captured.observation.candidates[0];
    assert_eq!(writer.relation, PageWriterRelation::TargetObjectWrite);
    assert_eq!(
        writer.binding.as_ref().unwrap().accesses,
        [MssqlPageWriteAccess { node_id: 1, operation: PageWriteOperation::Insert }]
    );
    assert_eq!(writer.request.as_ref().unwrap().key.session.id, "43");
    assert_eq!(captured.observation.page.as_ref().unwrap().request.key.session.id, "42");
    let selected = &owner.io.calls[12];
    assert!(
        selected
            .sql
            .contains("sys.dm_exec_text_query_plan(r.plan_handle,r.statement_start_offset,r.statement_end_offset)")
    );
    assert!(!selected.sql.contains("sys.dm_exec_query_stats"));
    assert!(!selected.sql.contains("sys.dm_exec_plan_attributes"));
    assert!(selected.sql.contains("AS [statement_bytes]") && selected.sql.contains("END AS [statement]"));
    assert!(selected.sql.contains("AS [plan_bytes]") && selected.sql.contains("END AS [plan]"));
    assert!(
        matches!(&selected.parameters[0],Parameter::Text(xml) if xml.contains("session=\"43\"") && !xml.contains("session=\"42\""))
    );
    assert_eq!(owner.query_count(), 17);
    assert_eq!(owner.close().await, SourceShutdown::Quiescent);
    assert_eq!(owner.io.closed, 1);
}

#[tokio::test]
async fn changed_native_generations_or_plan_tokens_never_publish_a_write_binding() {
    let focus = writer_focus().await;
    for field in ["checked_session_generation", "checked_request_generation", "checked_begin", "checked_plan_handle"] {
        let mut supplied = replies();
        let row = &supplied[13].as_ref().unwrap()[0];
        let value = if field == "checked_begin" {
            Val::Int(2)
        } else {
            Val::Text(if field == "checked_plan_handle" { "EF".repeat(64) } else { "2020-01-02T08:00:00".into() })
        };
        supplied[13] =
            Ok(vec![replace(row, &[(field, value)]), participant(1, &page_wait(42, "PAGELATCH_EX", "7:1:4144"))]);
        let mut owner = reader(supplied);
        let result = owner.capture_page_wait_writers(&focus).await.unwrap();
        assert!(result.observation.candidates[0].binding.is_none(), "{field}");
        assert_ne!(result.observation.candidates[0].relation, PageWriterRelation::TargetObjectWrite, "{field}");
        assert_eq!(owner.close().await, SourceShutdown::Quiescent);
    }
}

#[tokio::test]
async fn extending_the_page_window_does_not_renew_a_replaced_waiter_plan_or_borrow_a_writer_plan() {
    let focus = writer_focus().await;
    for change in ["none", "before-and-after", "after-only"] {
        let mut supplied = replies();
        for phase in [10, 13] {
            if change == "none" || change == "after-only" && phase == 10 {
                continue;
            }
            let points = supplied[phase].as_ref().unwrap();
            supplied[phase] = Ok(vec![
                points[0].clone(),
                replace(&points[1], &[("checked_plan_handle", Val::Text("EF".repeat(64)))]),
            ]);
        }
        let mut owner = reader(supplied);
        let captured = owner.capture_page_wait_writers(&focus).await.unwrap();
        let page = captured.observation.page.as_ref().unwrap();
        assert_eq!(page.plan_identity.is_some(), change == "none", "{change}");
        assert_eq!(page.statement_identity.as_ref(), Some(&focus.waiter.participant().statement_identity));
        assert_eq!(captured.observation.target_state, PageWriterTargetState::CurrentMapped);
        assert_eq!(captured.observation.candidates[0].relation, PageWriterRelation::TargetObjectWrite);
        assert!(captured.observation.candidates[0].binding.is_some());
        assert_eq!(owner.query_count(), 17, "no new query to retain a plan link");
        assert_eq!(owner.close().await, SourceShutdown::Quiescent);
    }
}

#[tokio::test]
async fn vanished_wait_or_changed_mapping_withholds_contemporary_writers_and_early_loss_skips_plans() {
    let focus = writer_focus().await;
    for early in [true, false] {
        let mut supplied = replies();
        if early {
            let before = supplied[10].as_ref().unwrap();
            supplied[10] = Ok(vec![
                before[0].clone(),
                replace(&before[1], &[("checked_wait_type", Val::Null), ("checked_wait_resource", Val::Null)]),
            ]);
            supplied.truncate(11);
            supplied.push(Ok(vec![context()]));
        } else {
            supplied[15] = Ok(vec![replace(
                &header(0, 4144),
                &[("object_modified_without_offset", Val::Text("2020-01-02T00:00:00".into()))],
            )]);
        }
        let mut owner = reader(supplied);
        let captured = owner.capture_page_wait_writers(&focus).await.unwrap();
        assert_eq!(
            captured.observation.target_state,
            if early { PageWriterTargetState::NoLongerObserved } else { PageWriterTargetState::ChangedTarget }
        );
        assert!(
            captured
                .observation
                .candidates
                .iter()
                .all(|candidate| candidate.binding.is_none() && candidate.relation == PageWriterRelation::Unavailable)
        );
        if early {
            assert!(owner.io.calls.iter().all(|call| !call.sql.contains("dm_exec_text_query_plan")));
        }
        assert_eq!(owner.close().await, SourceShutdown::Quiescent);
    }
}

#[tokio::test]
async fn missing_current_index_catalog_name_is_unavailable_not_a_different_write_target() {
    let focus = writer_focus().await;
    let mut supplied = replies();
    supplied[15] = Ok(vec![replace(&header(0, 4144), &[("index_name", Val::Null)])]);
    let mut owner = reader(supplied);
    let captured = owner.capture_page_wait_writers(&focus).await.unwrap();
    assert_eq!(captured.observation.target_state, PageWriterTargetState::Unavailable);
    assert_eq!(captured.observation.candidates[0].relation, PageWriterRelation::Unavailable);
    assert!(captured.observation.candidates[0].binding.is_none());
    assert_eq!(owner.close().await, SourceShutdown::Quiescent);
}

#[tokio::test]
async fn echoed_final_selectors_cannot_relabel_a_different_native_header_as_the_original_page() {
    let focus = writer_focus().await;
    for (field, value) in [("database_id", 8), ("file_id", 2), ("page_id", 4145)] {
        let mut supplied = replies();
        // target_database_id/file_id/page_id still echo the original7:1:4144 selector.
        supplied[15] = Ok(vec![replace(&header(0, 4144), &[(field, Val::Int(value))])]);
        let mut owner = reader(supplied);
        let error = owner
            .capture_page_wait_writers(&focus)
            .await
            .err()
            .expect("conflicting native coordinates must withhold every provisional binding");
        assert_eq!(CollectionStop::from_error(&error), CollectionStop::InvalidPage, "{field}");
        assert_eq!(owner.query_count(), 16, "no final metadata read after native header corruption");
        assert_eq!(owner.close().await, SourceShutdown::Quiescent);
        assert_eq!(owner.io.closed, 1);
    }
}

#[tokio::test]
async fn stricter_sql_plan_and_aggregate_limits_are_independent_and_never_reset_per_candidate() {
    let focus = writer_focus().await;
    let mut observation = PageWaitWriterObservation {
        observed_from: "2020-01-01T06:00:00Z".into(),
        observed_until: "2020-01-01T06:00:00Z".into(),
        focus_key: focus.key().unwrap(),
        target_state: PageWriterTargetState::Unavailable,
        page: None,
        candidates: vec![unverified(&focus.candidates[0])],
    };
    let budget = CollectionBudget { max_sql_bytes: 2048, max_plan_bytes: 1024, ..Default::default() };
    let limit = payload_allowance(&observation, &budget).unwrap();
    assert_eq!(limit.statement, 2048);
    assert_eq!(limit.plan, 1024);
    let wide = CollectionBudget {
        page_bytes: 256 * 1024,
        max_sql_bytes: 64 * 1024,
        max_plan_bytes: 64 * 1024,
        ..Default::default()
    };
    let one = payload_allowance(&observation, &wide).unwrap();
    observation.candidates = vec![observation.candidates[0].clone(); MAX_PAGE_WAIT_WRITER_CANDIDATES];
    let eight = payload_allowance(&observation, &wide).unwrap();
    assert!(eight.statement < one.statement && eight.plan < one.plan);
    let mut owner = reader(replies());
    owner.options.budget.max_plan_bytes = 1024;
    owner.options.budget.validate().unwrap();
    assert!(write_plan().encode_utf16().count() * 2 > 1024);
    let supplied = &mut owner.io;
    // A real native length probe omits the oversized plan body; the exact length remains a fact.
    let mut scripted = replies();
    scripted[12] = Ok(vec![replace(&payload(STATEMENT, &write_plan()), &[("plan", Val::Null)])]);
    *supplied = FakeRead::new(scripted);
    let captured = owner.capture_page_wait_writers(&focus).await.unwrap();
    assert!(matches!(
        captured.observation.candidates[0].plan,
        DefinitionPayload::Unavailable { reason: PayloadUnavailable::Oversized }
    ));
    assert!(captured.observation.candidates[0].binding.is_none());
    assert!(matches!(owner.io.calls[12].parameters[2], Parameter::Integer(1024)));
    assert_eq!(owner.close().await, SourceShutdown::Quiescent);
}

#[tokio::test]
async fn budget_cancellation_and_final_role_change_stop_the_same_owner_before_publication() {
    let focus = writer_focus().await;
    let mut owner = reader(Vec::new());
    owner.io = FakeRead::new(replies()).with_query_limit(10);
    assert_eq!(
        CollectionStop::from_error(&owner.capture_page_wait_writers(&focus).await.err().unwrap()),
        CollectionStop::QueryLimit
    );
    assert_eq!(owner.query_count(), 10);
    assert_eq!(owner.close().await, SourceShutdown::Quiescent);
    let mut owner = reader(Vec::new());
    owner.io = FakeRead::new(replies()).pause_on_query(13);
    tokio::select! { biased; _=owner.capture_page_wait_writers(&focus)=>panic!("paused payload"),_=tokio::task::yield_now()=>{} }
    assert_eq!(owner.query_count(), 13);
    assert_eq!(owner.close().await, SourceShutdown::Quiescent);
    assert_eq!(owner.io.closed, 1);
    let mut scripted = replies();
    scripted[16] = Ok(vec![replace(&context(), &[("updateability", Val::Text("READ_ONLY".into()))])]);
    let mut owner = reader(scripted);
    assert_eq!(
        CollectionStop::from_error(&owner.capture_page_wait_writers(&focus).await.err().unwrap()),
        CollectionStop::SourceChanged
    );
    assert_eq!(owner.close().await, SourceShutdown::Quiescent);
}

#[tokio::test]
async fn native_timeout_or_unconfirmed_cancellation_never_triggers_a_followup_read() {
    let focus = writer_focus().await;
    for stop in [CollectionStop::TimedOut, CollectionStop::Cancelled, CollectionStop::CancellationUnconfirmed] {
        let mut scripted = replies();
        scripted[12] = Err(ReadError::Stopped(stop));
        let mut owner = reader(scripted);
        if stop == CollectionStop::CancellationUnconfirmed {
            owner.io.shutdown = SourceShutdown::Unconfirmed;
        }
        let error = owner.capture_page_wait_writers(&focus).await.err().unwrap();
        assert_eq!(CollectionStop::from_error(&error), stop);
        assert_eq!(owner.query_count(), 13);
        assert_eq!(
            owner.close().await,
            if stop == CollectionStop::CancellationUnconfirmed {
                SourceShutdown::Unconfirmed
            } else {
                SourceShutdown::Quiescent
            }
        );
        assert_eq!(owner.io.closed, 1);
    }
}

#[tokio::test]
async fn malformed_compact_locators_are_rejected_before_any_native_work() {
    let mut focus = writer_focus().await;
    focus.candidates[0].statement_identity.qualifiers.insert("statement_start_offset".into(), "invalid".into());
    let mut owner = reader(vec![]);
    assert!(owner.capture_page_wait_writers(&focus).await.is_err());
    assert_eq!(owner.query_count(), 0);
}

#[tokio::test]
async fn selected_native_access_binding_distinguishes_read_other_unsupported_and_nested_plans() {
    let focus = writer_focus().await;
    let native = crate::activity::database_context::test_azure_authority(&context()).unwrap();
    let context = NativeReadContext {
        target: focus.waiter.participant().target.clone(),
        partition: native.source_partition().unwrap(),
        observed_from: "2020-01-01T06:00:00Z".into(),
        observed_until: "2020-01-01T06:00:00Z".into(),
        database_writable: Some(true),
    };
    assert_eq!(
        plan::classify(&write_plan(), STATEMENT, &focus.target, &context).unwrap().relation,
        PageWriterRelation::TargetObjectWrite
    );
    let other = plan_xml(STATEMENT, "Clustered Index Insert", "Insert", "other");
    assert_eq!(
        plan::classify(&other, STATEMENT, &focus.target, &context).unwrap().relation,
        PageWriterRelation::OtherTarget
    );
    let read = plan_xml("SELECT id FROM dbo.orders", "Index Seek", "Index Seek", "orders")
        .replace("<Update>", "<IndexScan>")
        .replace("</Update>", "</IndexScan>");
    assert_eq!(
        plan::classify(&read, "SELECT id FROM dbo.orders", &focus.target, &context).unwrap().relation,
        PageWriterRelation::ReadOnlyPlan
    );
    for raw in [
        write_plan().replace("[shop]", "[other]"),
        write_plan().replace("NodeId=\"1\"", "NodeId=\"invalid\""),
        write_plan().replace("Clustered Index Insert", "Merge"),
        write_plan().replace("</QueryPlan>", "<UDF><QueryPlan/></UDF></QueryPlan>"),
        write_plan().replace("<QueryPlan>", "<QueryPlan><QueryPlan/>"),
        write_plan()
            .replace("<Statements>", "<Statements><StmtSimple StatementText=\"other\"><QueryPlan/></StmtSimple>"),
    ] {
        assert_ne!(
            plan::classify(&raw, STATEMENT, &focus.target, &context).unwrap().relation,
            PageWriterRelation::TargetObjectWrite
        );
    }
    assert_eq!(
        plan::classify(&write_plan(), "different statement", &focus.target, &context).unwrap().relation,
        PageWriterRelation::Unsupported
    );
    let missing_index = write_plan().replace(" Index=\"[PK_orders]\"", "");
    assert_eq!(
        plan::classify(&missing_index, STATEMENT, &focus.target, &context).unwrap().relation,
        PageWriterRelation::Unsupported,
        "missing index metadata is not proof of another target"
    );
}
