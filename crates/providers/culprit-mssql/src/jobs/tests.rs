use super::*;
use crate::{
    instance::test_support::{epoch, metadata},
    read::ReadResult,
    resources::test_support::{FakeRead, instance_pressure_row, replace},
    value::Val,
};
mod fixtures;
mod permissions;
use fixtures::*;
#[tokio::test]
async fn scan_and_snapshot_share_one_bounded_native_reader_with_independent_instance_and_agent_context() {
    let mut io = FakeRead::new(replies());
    let budget = bounded_budget(&CollectionBudget::upper_limits());
    let (captured, coverage) = collect_from(&mut io, &budget, "fixture.invalid", 1433).await;
    let captured = captured.unwrap();
    assert!(captured.is_valid_read(true));
    assert_eq!(io.queries(), 13);
    assert_eq!(io.closed, 1);
    assert!(matches!(captured.context.target, CollectionTarget::Shared(_)));
    assert_eq!(captured.context.database_writable, None);
    assert_eq!(coverage.outcomes().len(), 4);
    assert_eq!(
        captured.sources.iter().find(|source| source.source == MSSQL_AGENT_HISTORY_SOURCE).unwrap().state,
        culprit_core::SourceState::Partial
    );
    assert!(io.calls.iter().all(|call| call.sql.contains("/* culprit */")));
    assert!(io.calls.iter().all(|call| !call.sql.contains(".command")
        && !call.sql.contains(".message")
        && !call.sql.contains("owner_sid")
        && !call.sql.contains("sp_start_job")));
    let request = request();
    let mut reader =
        Reader { io: FakeRead::new(replies()), host: "fixture.invalid".into(), port: 1433, verified: None };
    let observed = reader.observe(&request).await.unwrap();
    assert_eq!(reader.io.queries(), 13);
    assert!(reader.verify(&observed.description).await.unwrap());
    assert_eq!(reader.io.queries(), 13, "finish does not issue a rescue query");
    assert_eq!(observed.records.len(), captured.observations.len());
    assert_eq!(reader.close().await, SourceShutdown::Quiescent);
    assert_eq!(reader.io.closed, 1);
}
#[tokio::test]
async fn azure_sql_never_dispatches_msdb_and_express_retains_a_truthful_not_applicable_profile() {
    let azure =
        replace(&metadata(), &[("edition", Val::Int(5)), ("machine_name", Val::Null), ("process_id", Val::Null)]);
    let logical = crate::query_store::window_sql::tests::azure_row();
    let mut io =
        FakeRead::new(vec![Ok(vec![azure.clone()]), Ok(vec![logical.clone()]), Ok(vec![logical]), Ok(vec![azure])]);
    let read = capture(
        &mut io,
        ReadSubject::Selected(&CollectionSubject::Database),
        &bounded_budget(&CollectionBudget::upper_limits()),
        "fixture.invalid",
        1433,
    )
    .await
    .unwrap();
    assert!(read.sources.iter().all(|source| source.state == culprit_core::SourceState::NotApplicable));
    assert!(read.observations.is_empty());
    assert_eq!(io.queries(), 4);
    assert!(io.calls.iter().all(|call| !call.sql.to_ascii_lowercase().contains("msdb")));
    let express = replace(&metadata(), &[("edition", Val::Int(4))]);
    let mut io =
        FakeRead::new(vec![Ok(vec![express.clone()]), Ok(vec![epoch(100)]), Ok(vec![express]), Ok(vec![epoch(100)])]);
    let read = capture(
        &mut io,
        ReadSubject::Connected,
        &bounded_budget(&CollectionBudget::upper_limits()),
        "fixture.invalid",
        1433,
    )
    .await
    .unwrap();
    assert_eq!(io.queries(), 4);
    assert_eq!(read.metadata.availability, MssqlAgentAvailability::NotApplicable);
    assert!(read.observations.is_empty());
    assert!(io.calls.iter().all(|call| !call.sql.contains("msdb")));
}
#[tokio::test]
async fn denied_capability_and_missing_columns_never_become_healthy_empty_or_exception_fallback_reads() {
    let denied = layout_rows()
        .iter()
        .map(|row| {
            replace(
                row,
                &[("object_select", Val::Int(0)), ("service_visibility", Val::Int(0)), ("is_sysadmin", Val::Int(0))],
            )
        })
        .collect::<Vec<_>>();
    let mut io = FakeRead::new(vec![
        Ok(vec![metadata()]),
        Ok(vec![epoch(100)]),
        Ok(vec![instance_pressure_row()]),
        Ok(denied.clone()),
        Ok(vec![instance_pressure_row()]),
        Ok(denied),
        Ok(vec![metadata()]),
        Ok(vec![epoch(100)]),
    ]);
    let read = capture(
        &mut io,
        ReadSubject::Connected,
        &bounded_budget(&CollectionBudget::upper_limits()),
        "fixture.invalid",
        1433,
    )
    .await
    .unwrap();
    assert_eq!(io.queries(), 8);
    assert!(read.sources.iter().all(|source| source.state == culprit_core::SourceState::Unavailable
        && source.error_code == Some(ErrorCode::PermissionDenied)));
    assert!(read.observations.is_empty());
    let mut rows = layout_rows();
    let index = rows.iter().position(|row| text(row, "column_name").unwrap() == "run_requested_source").unwrap();
    rows[index] = replace(&rows[index], &[("native_type", Val::Null), ("native_nullable", Val::Null)]);
    let mut replies = replies();
    replies[3] = Ok(rows.clone());
    replies[9] = Ok(rows);
    let mut io = FakeRead::new(replies);
    let read = capture(
        &mut io,
        ReadSubject::Connected,
        &bounded_budget(&CollectionBudget::upper_limits()),
        "fixture.invalid",
        1433,
    )
    .await
    .unwrap();
    assert!(!read.metadata.available(MssqlAgentObject::Activity));
    assert!(!io.calls[6].sql.contains("sysjobactivity"));
    assert_eq!(io.queries(), 13);
}
#[tokio::test]
async fn native_null_status_bounded_retained_history_and_final_layout_change_remain_raw_and_partial() {
    let mut inputs = replies();
    let context = replace(&context_row(), &[("service_status", Val::Null)]);
    inputs[5] = Ok(vec![context.clone()]);
    inputs[10] = Ok(vec![context]);
    let mut io = FakeRead::new(inputs);
    let read = capture(
        &mut io,
        ReadSubject::Connected,
        &bounded_budget(&CollectionBudget::upper_limits()),
        "fixture.invalid",
        1433,
    )
    .await
    .unwrap();
    assert!(
        read.observations
            .iter()
            .filter_map(|row| if let OperationalFact::MssqlAgentContext(point) = &row.fact {
                Some(point)
            } else {
                None
            })
            .all(|point| point.services[0].status.is_none())
    );
    let mut inputs = replies();
    let changed = layout_rows().iter().map(|row| replace(row, &[("object_id", Val::Int(999))])).collect();
    inputs[9] = Ok(changed);
    assert!(inputs.remove(10).is_ok(), "omit the scripted payload read for the denied source");
    let mut io = FakeRead::new(inputs);
    let read = capture(
        &mut io,
        ReadSubject::Connected,
        &bounded_budget(&CollectionBudget::upper_limits()),
        "fixture.invalid",
        1433,
    )
    .await
    .unwrap();
    assert_eq!(read.metadata.recheck, MssqlAgentMetadataRecheck::Changed);
    assert_eq!(io.queries(), 12);
    assert!(read.sources.iter().all(|source| !source.complete_scope));
}
#[tokio::test]
async fn terminal_failures_stop_without_followup_and_close_uncertainty_withholds_positive_scan_rows() {
    for stop in [
        CollectionStop::Cancelled,
        CollectionStop::TimedOut,
        CollectionStop::QueryLimit,
        CollectionStop::RowLimit,
        CollectionStop::ByteLimit,
        CollectionStop::CancellationUnconfirmed,
    ] {
        let mut inputs = replies();
        inputs[6] = Err(ReadError::Stopped(stop));
        inputs.truncate(7);
        let mut io = FakeRead::new(inputs);
        let (read, coverage) =
            collect_from(&mut io, &bounded_budget(&CollectionBudget::upper_limits()), "fixture.invalid", 1433).await;
        assert!(read.is_none());
        assert_eq!(io.queries(), 7, "{stop:?}");
        assert_eq!(io.closed, 1);
        assert!(coverage.outcomes().iter().all(|source| source.state == culprit_core::SourceState::Unavailable));
    }
    let mut io = FakeRead::new(replies());
    io.shutdown = SourceShutdown::Unconfirmed;
    let (read, _) =
        collect_from(&mut io, &bounded_budget(&CollectionBudget::upper_limits()), "fixture.invalid", 1433).await;
    assert!(read.is_none());
    assert_eq!(io.closed, 1);
    let mut io = FakeRead::new(replies()).with_query_limit(7);
    let (read, _) =
        collect_from(&mut io, &bounded_budget(&CollectionBudget::upper_limits()), "fixture.invalid", 1433).await;
    assert!(read.is_none());
    assert_eq!(io.queries(), 7);
    assert_eq!(io.closed, 1);
}
#[tokio::test]
async fn changed_instance_epoch_duplicate_history_and_wrong_agent_session_never_publish_rebound_facts() {
    for change in ["epoch", "instance", "duplicate", "activity-session"] {
        let mut inputs = replies();
        match change {
            "epoch" => inputs[12] = Ok(vec![epoch(101)]),
            "instance" => {
                inputs[11] = Ok(vec![replace(&metadata(), &[("machine_name", Val::Text("different".into()))])])
            }
            "duplicate" => inputs[8] = Ok(vec![history_row(), history_row()]),
            _ => {
                let job = replace(&job_row(), &[("activity_session_id", Val::Int(8))]);
                inputs[6] = Ok(vec![job]);
            }
        }
        let mut io = FakeRead::new(inputs);
        let result = capture(
            &mut io,
            ReadSubject::Connected,
            &bounded_budget(&CollectionBudget::upper_limits()),
            "fixture.invalid",
            1433,
        )
        .await;
        assert!(result.is_err(), "{change}");
    }
}

#[tokio::test]
async fn permission_failure_of_one_native_family_preserves_other_facts_and_never_retries_the_family() {
    let mut inputs = replies();
    inputs[6] = Err(ReadError::Source(Error::new(
        ErrorCode::PermissionDenied,
        culprit_core::message!("view.mssql_agent_jobs.source.gap"),
    )));
    let mut io = FakeRead::new(inputs);
    let read = capture(
        &mut io,
        ReadSubject::Connected,
        &bounded_budget(&CollectionBudget::upper_limits()),
        "fixture.invalid",
        1433,
    )
    .await
    .unwrap();
    assert_eq!(io.queries(), 13);
    let outcome = read.sources.iter().find(|source| source.source == MSSQL_AGENT_JOBS_SOURCE).unwrap();
    assert_eq!(outcome.state, culprit_core::SourceState::Unavailable);
    assert_eq!(outcome.error_code, Some(ErrorCode::PermissionDenied));
    assert_eq!(outcome.records, 0);
    assert!(read.observations.iter().any(|row| matches!(row.fact, OperationalFact::MssqlAgentHistory(_))));
}
#[tokio::test]
async fn bounded_history_tail_uses_one_overflow_witness_and_retains_the_original_admitted_limit() {
    let mut inputs = replies();
    inputs[8] = Ok((1..=257).map(|id| replace(&history_row(), &[("instance_id", Val::Int(id))])).collect());
    let mut io = FakeRead::new(inputs);
    let read = capture(
        &mut io,
        ReadSubject::Connected,
        &bounded_budget(&CollectionBudget::upper_limits()),
        "fixture.invalid",
        1433,
    )
    .await
    .unwrap();
    assert_eq!(read.metadata.history_limit, 256);
    let history = read.sources.iter().find(|source| source.source == MSSQL_AGENT_HISTORY_SOURCE).unwrap();
    assert_eq!(history.records, 256);
    assert!(!history.complete_scope);
    assert_eq!(io.calls[8].limit, 257);
    assert_eq!(io.queries(), 13);
}
#[tokio::test]
async fn native_system_object_ids_keep_their_signed_identity_and_incomplete_metadata_does_not_promote_visibility() {
    let mut inputs = replies();
    let native = layout_rows()
        .iter()
        .map(|row| {
            if integer(row, "kind").unwrap() == 5 {
                replace(row, &[("object_id", Val::Int(-100))])
            } else {
                row.clone()
            }
        })
        .collect::<Vec<_>>();
    inputs[3] = Ok(native.clone());
    inputs[9] = Ok(native);
    let mut io = FakeRead::new(inputs);
    let read = capture(
        &mut io,
        ReadSubject::Connected,
        &bounded_budget(&CollectionBudget::upper_limits()),
        "fixture.invalid",
        1433,
    )
    .await
    .unwrap();
    assert_eq!(read.metadata.capability(MssqlAgentObject::Services).unwrap().object_id.as_deref(), Some("-100"));
    let mut inputs = replies();
    inputs[9] = Err(ReadError::Source(Error::new(
        ErrorCode::PermissionDenied,
        culprit_core::message!("view.mssql_agent_jobs.source.gap"),
    )));
    assert!(inputs.remove(10).is_ok(), "omit the scripted payload read for the denied source");
    let mut io = FakeRead::new(inputs);
    let read = capture(
        &mut io,
        ReadSubject::Connected,
        &bounded_budget(&CollectionBudget::upper_limits()),
        "fixture.invalid",
        1433,
    )
    .await
    .unwrap();
    assert!(matches!(
        read.metadata.recheck,
        MssqlAgentMetadataRecheck::Unavailable { error_code: ErrorCode::PermissionDenied }
    ));
    assert!(read.sources.iter().all(|source| !source.complete_scope));
    assert_eq!(io.queries(), 12);
}

#[tokio::test]
async fn both_instance_resource_checkpoints_fail_closed_without_dispatching_agent_payload_or_retrying() {
    assert_eq!(bounded_budget(&CollectionBudget::upper_limits()).max_queries, 13);
    for index in [2usize, 4] {
        for stop in [
            CollectionStop::Pressure,
            CollectionStop::PressureUnavailable,
            CollectionStop::Cancelled,
            CollectionStop::TimedOut,
        ] {
            let mut inputs = replies();
            inputs[index] = Err(ReadError::Stopped(stop));
            inputs.truncate(index + 1);
            let mut io = FakeRead::new(inputs);
            let (read, _) =
                collect_from(&mut io, &bounded_budget(&CollectionBudget::upper_limits()), "fixture.invalid", 1433)
                    .await;
            assert!(read.is_none());
            assert_eq!(io.queries() as usize, index + 1);
            assert_eq!(io.closed, 1);
            assert!(io.calls.iter().all(|call| !call.sql.contains("FROM msdb.dbo.sysjob")));
        }
        let mut inputs = replies();
        inputs[index] = Ok(vec![]);
        inputs.truncate(index + 1);
        let mut io = FakeRead::new(inputs);
        let error = capture(
            &mut io,
            ReadSubject::Connected,
            &bounded_budget(&CollectionBudget::upper_limits()),
            "fixture.invalid",
            1433,
        )
        .await
        .unwrap_err();
        assert_eq!(CollectionStop::from_error(&error), CollectionStop::PressureUnavailable);
        assert_eq!(io.queries() as usize, index + 1);
    }
    let mut io = FakeRead::new(replies()).with_query_limit(12);
    let (read, _) =
        collect_from(&mut io, &bounded_budget(&CollectionBudget::upper_limits()), "fixture.invalid", 1433).await;
    assert!(read.is_none());
    assert_eq!(io.queries(), 12);
    assert_eq!(io.closed, 1);
}
