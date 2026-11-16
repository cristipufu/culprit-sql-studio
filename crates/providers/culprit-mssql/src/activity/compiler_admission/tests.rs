use super::*;
use crate::{
    activity::{
        memory::{
            focus::tests::original_focus,
            tests::{context, reader_with_identity},
        },
        tests::{details, metadata},
    },
    resources::test_support::replace,
    value::Val,
};

pub(crate) fn waiter_row() -> Rec {
    replace(
        &metadata(42),
        &[
            ("wait_type", Val::Text(crate::waits::COMPILATION_ADMISSION_WAIT.into())),
            ("blocking_session_id", Val::Int(0)),
        ],
    )
}
pub(crate) fn focus() -> ActivityRequestFocus {
    let mut focus = original_focus();
    focus.original = super::super::rows::record(&waiter_row()).unwrap().unwrap().facts;
    focus.statement_identity = focus.original.statement_identity.clone().unwrap();
    let source = CollectionDescription {
        target: focus.target.clone(),
        partition: focus.partition.clone(),
        source: focus.source.clone(),
        continuity: SourceContinuity::Unverified,
        observation: SourceObservation::Activity(ActivitySnapshotDescription {
            database_writable: focus.database_writable,
            epoch: Some(focus.epoch.clone()),
            at: focus.original_observed_at.clone(),
            observed_from: focus.original_observed_at.clone(),
            observed_until: focus.original_observed_at.clone(),
            population: ActivityPopulation::ActiveRequestsOpenTransactionsAndBlockers,
            scope: culprit_core::MeasurementScope::Database,
            native_rows: 1,
            observations: 1,
            coverage: StateSourceCoverage::observed(focus.source.clone(), 1, true),
        }),
    };
    let row =
        ExportedRecord::new(&source, &focus.parent.capture, TelemetryRecord::from(focus.original.clone())).unwrap();
    focus.parent = DiagnosticEvidenceRef::from_export(&source, &focus.parent.manifest_sha256, &row, 0).unwrap();
    focus
}
pub(crate) fn replies(row: Option<Rec>, enrich: bool) -> Vec<crate::read::ReadResult<Vec<Rec>>> {
    let mut replies = vec![Ok(vec![context()]), Ok(vec![row.unwrap_or_else(context)])];
    if enrich {
        replies.extend([
            Ok(vec![details(42, "SELECT * FROM dbo.orders WHERE id=42")]),
            Ok(vec![crate::compiler_gateways::tests::row()]),
        ]);
    }
    replies.push(Ok(vec![context()]));
    replies
}

#[tokio::test]
async fn compiler_waiter_uses_one_owned_reader_and_enriches_only_the_original_before_independent_gateways() {
    let original = focus();
    let mut reader = reader_with_identity(replies(Some(waiter_row()), true), IdentityDetail::AzureDatabase);
    let captured = reader.capture_focused_compiler_admission(&original, None).await.unwrap();
    let result = &captured.result;
    assert_eq!(result.membership, CompilationAdmissionMembership::Observed);
    assert_eq!(captured.observations.len(), 2);
    assert!(result.matches_observations(&captured.observations));
    let OperationalFact::CompilerWaiter(waiter) = &captured.observations[0].fact else { panic!() };
    assert!(waiter.request.statement.is_available());
    assert!(!result.focus.original.statement.is_available());
    assert_eq!(waiter.request.key, original.original.key);
    assert_eq!(reader.query_count(), 5);
    assert!(reader.io.calls[2].sql.contains("dm_exec_sql_text"));
    assert!(reader.io.calls[3].sql.contains("dm_exec_query_optimizer_memory_gateways"));
    assert!(captured.context.has_qualified_local_owner());
    assert_eq!(reader.close().await, SourceShutdown::Quiescent);
}

#[tokio::test]
async fn replaced_generation_statement_or_changed_wait_never_reads_sql_or_gateways_and_never_reports_recovery() {
    for changes in [
        vec![("request_generation", Val::Text("2020-01-01T07:58:01".into()))],
        vec![("statement_start", Val::Int(2))],
        vec![("wait_type", Val::Text("RESOURCE_SEMAPHORE".into()))],
    ] {
        let mut reader =
            reader_with_identity(replies(Some(replace(&waiter_row(), &changes)), false), IdentityDetail::AzureDatabase);
        let captured = reader.capture_focused_compiler_admission(&focus(), None).await.unwrap();
        assert_ne!(captured.result.membership, CompilationAdmissionMembership::Observed);
        assert!(captured.observations.is_empty());
        assert_eq!(reader.query_count(), 3);
        assert_eq!(reader.close().await, SourceShutdown::Quiescent);
    }
}

#[tokio::test]
async fn changed_owner_and_unknown_role_refuse_before_native_request_or_gateway_work() {
    for unknown_role in [false, true] {
        let mut focus = focus();
        let context = if unknown_role {
            focus.database_writable = None;
            context()
        } else {
            replace(&context(), &[("physical_incarnation", Val::Text("changed-owner".into()))])
        };
        let mut reader = reader_with_identity(vec![Ok(vec![context])], IdentityDetail::AzureDatabase);
        assert!(reader.capture_focused_compiler_admission(&focus, None).await.is_err());
        assert_eq!(reader.query_count(), u32::from(!unknown_role));
        assert_eq!(reader.close().await, SourceShutdown::Quiescent);
    }
}

#[tokio::test]
async fn gateway_failure_keeps_the_affected_waiter_partial_and_final_authority_failure_publishes_nothing() {
    let mut failed = replies(Some(waiter_row()), true);
    failed[3] = Err(ReadError::Source(Error::new(ErrorCode::PermissionDenied, "synthetic denied gateway")));
    let mut reader = reader_with_identity(failed, IdentityDetail::AzureDatabase);
    let captured = reader.capture_focused_compiler_admission(&focus(), None).await.unwrap();
    assert!(captured.gateway_failure.is_some());
    assert_eq!(captured.observations.len(), 1);
    assert_eq!(reader.close().await, SourceShutdown::Quiescent);
    let mut changed = replies(Some(waiter_row()), true);
    *changed.last_mut().unwrap() =
        Ok(vec![replace(&context(), &[("physical_incarnation", Val::Text("changed-final-owner".into()))])]);
    let mut reader = reader_with_identity(changed, IdentityDetail::AzureDatabase);
    assert!(reader.capture_focused_compiler_admission(&focus(), None).await.is_err());
    assert_eq!(reader.query_count(), 5);
    assert_eq!(reader.close().await, SourceShutdown::Quiescent);
}

#[tokio::test]
async fn bounded_gateway_work_never_resets_the_callers_row_budget_or_requires_sql_availability() {
    let mut values = replies(Some(waiter_row()), true);
    assert!(values.remove(3).is_ok());
    let mut reader = reader_with_identity(values, IdentityDetail::AzureDatabase);
    reader.options.budget.max_snapshot_rows = 1;
    let captured = reader.capture_focused_compiler_admission(&focus(), None).await.unwrap();
    assert_eq!(captured.observations.len(), 1);
    assert_eq!(CollectionStop::from_error(captured.gateway_failure.as_ref().unwrap()), CollectionStop::RowLimit);
    assert_eq!(reader.query_count(), 4, "no native gateway query fits after the original row");
    assert!(!reader.io.calls.iter().any(|call| call.sql.contains("query_optimizer_memory_gateways")));
    assert_eq!(reader.close().await, SourceShutdown::Quiescent);
    let mut values = replies(Some(waiter_row()), true);
    values[2] = Err(ReadError::Source(Error::new(ErrorCode::PermissionDenied, "synthetic denied statement")));
    let mut reader = reader_with_identity(values, IdentityDetail::AzureDatabase);
    let captured = reader.capture_focused_compiler_admission(&focus(), None).await.unwrap();
    let OperationalFact::CompilerWaiter(waiter) = &captured.observations[0].fact else { panic!() };
    assert!(!waiter.request.statement.is_available());
    assert_eq!(captured.observations.len(), 2);
    assert_eq!(captured.result.membership, CompilationAdmissionMembership::Observed);
    assert_eq!(reader.close().await, SourceShutdown::Quiescent);
}

#[tokio::test]
async fn terminal_native_stops_prevent_followup_and_unconfirmed_shutdown_stays_visible_to_the_owner() {
    for stop in [
        CollectionStop::Cancelled,
        CollectionStop::TimedOut,
        CollectionStop::QueryLimit,
        CollectionStop::CancellationUnconfirmed,
    ] {
        let mut values = replies(Some(waiter_row()), true);
        values[3] = Err(ReadError::Stopped(stop));
        let mut reader = reader_with_identity(values, IdentityDetail::AzureDatabase);
        if stop == CollectionStop::CancellationUnconfirmed {
            reader.io.shutdown = SourceShutdown::Unconfirmed;
        }
        let error = reader.capture_focused_compiler_admission(&focus(), None).await.err().unwrap();
        assert_eq!(CollectionStop::from_error(&error), stop);
        assert_eq!(reader.query_count(), 4, "no post-stop authority query or replacement reader");
        assert_eq!(
            reader.close().await,
            if stop == CollectionStop::CancellationUnconfirmed {
                SourceShutdown::Unconfirmed
            } else {
                SourceShutdown::Quiescent
            }
        );
        assert_eq!(reader.io.closed, 1);
    }
}
