use super::*;
use crate::{
    activity::page_targets::tests::{context, page_wait, reader, successful_replies},
    resources::test_support::replace,
    value::{Rec, Val},
};

pub(crate) fn focus(row: &Rec) -> ActivityWaitFocus {
    let original = crate::activity::rows::record(row).unwrap().unwrap().facts;
    let context =
        ActivityContext::decode(row, "fixture.invalid", 1433, super::super::super::IdentityDetail::AzureDatabase)
            .unwrap();
    let capture = CaptureIdentity {
        observation_id: "original-page-wait".into(),
        revision: Some(CaptureRevision {
            outbox_id: "focused-page-fixture".into(),
            owner_epoch: "1".into(),
            sequence: "1".into(),
        }),
    };
    ActivityWaitFocus {
        parent: DiagnosticEvidenceRef {
            batch_id: batch_id(&capture.observation_id, TelemetryStream::ActivityObservations).unwrap(),
            capture,
            manifest_sha256: digest(b"original manifest"),
            source_scope_id: digest(b"original native scope"),
            stream: TelemetryStream::ActivityObservations,
            native_key: NativeRecordIdentity::Activity(original.key.clone()),
            record_id: digest(b"original row"),
            payload_sha256: digest(b"original payload"),
            representation_version: REPRESENTATION_VERSION,
            row_ordinal: 0,
        },
        target: context.database.into(),
        source: culprit_core::source_id::SESSIONS.into(),
        epoch: context.epoch.unwrap(),
        original_observed_at: context.at,
        statement_identity: original.statement_identity.clone().unwrap(),
        resource: original.wait.as_ref().unwrap().identified_resources[0].clone(),
        original,
    }
}

#[tokio::test]
async fn focus_uses_only_the_original_native_request_and_retains_baseline_context_separately() {
    let original_row = page_wait(42, "PAGELATCH_EX", "7:1:4144");
    let focus = focus(&original_row);
    let current_row = replace(&original_row, &[("program_name", Val::Text("later native client".into()))]);
    let mut native = reader(successful_replies(vec![current_row]));
    let captured = native.capture_focused_page_wait_targets(&focus).await.unwrap();
    assert_eq!(captured.result.focus, focus);
    assert!(captured.result.permits_page_lookup());
    let point = &captured.capture.snapshot.observations[0];
    assert!(point.target.is_some());
    assert_eq!(point.request.client.as_ref().unwrap().application.as_deref(), Some("later native client"));
    assert_eq!(captured.result.focus.original.client.as_ref().unwrap().application.as_deref(), Some("app"));
    let query = &native.io.calls[1];
    assert!(query.sql.contains("r.session_id=@P2 AND r.request_id=@P3"));
    assert!(!query.sql.contains("LIKE 'PAGELATCH"));
    assert_eq!(query.parameters.len(), 3);
    for (parameter, expected) in query.parameters.iter().zip([1, 42, 0]) {
        assert!(matches!(parameter, Parameter::Integer(value) if *value == expected));
    }
    assert_eq!(native.query_count(), 10, "same bounded deep phases, without a broad population read");
    assert_eq!(native.close().await, SourceShutdown::Quiescent);
}

#[tokio::test]
async fn boxed_baseline_native_recovery_epoch_is_accepted_by_the_same_focused_reader() {
    let original = page_wait(42, "PAGELATCH_EX", "7:1:4144");
    let local = replace(&original, &[("edition", Val::Int(3))]);
    let platform = crate::instance::test_support::metadata();
    let ticks = crate::instance::test_support::epoch(100);
    let authority_row = crate::activity::page_targets::tests::boxed_context();
    let mut authority_reader =
        reader(vec![Ok(vec![platform.clone()]), Ok(vec![ticks.clone()]), Ok(vec![authority_row.clone()])]);
    authority_reader.options.identity = crate::activity::IdentityDetail::Endpoint;
    let authority = crate::activity::database_context::DatabaseContext::initial(&mut authority_reader)
        .await
        .unwrap()
        .capture_context()
        .unwrap();
    let mut focus = focus(&original);
    focus.target = authority.database.into();
    focus.epoch = authority.epoch.unwrap();
    let mut supplied = successful_replies(vec![local]);
    supplied.remove(6).unwrap();
    supplied.remove(3).unwrap();
    supplied[0] = Ok(vec![authority_row.clone()]);
    let last = supplied.len() - 1;
    supplied[last] = Ok(vec![authority_row]);
    supplied.insert(0, Ok(vec![ticks.clone()]));
    supplied.insert(0, Ok(vec![platform.clone()]));
    let last = supplied.len() - 1;
    supplied.insert(last, Ok(vec![platform]));
    supplied.insert(last + 1, Ok(vec![ticks]));
    let mut native = reader(supplied);
    native.options.identity = crate::activity::IdentityDetail::Endpoint;
    let captured = native.capture_focused_page_wait_targets(&focus).await.unwrap();
    assert_eq!(captured.result.focus, focus);
    assert!(captured.capture.snapshot.observations[0].target.is_some());
    assert!(captured.capture.context.database.incarnation.is_none());
    assert_eq!(
        captured.capture.snapshot.observations[0].target.as_ref().unwrap().database.qualifiers["database_guid"],
        "native-database-guid"
    );
    assert_eq!(native.query_count(), 12);
    assert!(native.io.calls.iter().all(|call| !call.sql.contains("sys.dm_user_db_resource_governance")));
    assert_eq!(native.close().await, SourceShutdown::Quiescent);
}

#[tokio::test]
async fn changed_generation_statement_or_wait_cannot_trigger_header_or_replacement_reads() {
    let original = page_wait(42, "PAGELATCH_EX", "7:1:4144");
    let focus = focus(&original);
    for (field, value, request, wait) in [
        (
            "session_generation",
            Val::Text("2020-01-01T07:01:00".into()),
            PageWaitRequestState::Changed,
            PageWaitRelation::Unqualified,
        ),
        (
            "request_generation",
            Val::Text("2020-01-01T07:59:00".into()),
            PageWaitRequestState::Changed,
            PageWaitRelation::Unqualified,
        ),
        ("statement_start", Val::Int(2), PageWaitRequestState::Changed, PageWaitRelation::Unqualified),
        ("statement_start", Val::Null, PageWaitRequestState::Unverified, PageWaitRelation::Unqualified),
        ("wait_type", Val::Null, PageWaitRequestState::SameStatement, PageWaitRelation::NoLongerObserved),
        (
            "wait_resource",
            Val::Text("7:1:4145".into()),
            PageWaitRequestState::SameStatement,
            PageWaitRelation::Different,
        ),
    ] {
        let changed = replace(&original, &[(field, value)]);
        let mut native = reader(vec![Ok(vec![context()]), Ok(vec![changed]), Ok(vec![context()])]);
        let captured = native.capture_focused_page_wait_targets(&focus).await.unwrap();
        assert_eq!(captured.result.request, request, "{field}");
        assert_eq!(captured.result.wait, wait, "{field}");
        assert_eq!(captured.result.focus, focus);
        assert!(captured.capture.snapshot.observations.is_empty());
        assert_eq!(native.query_count(), 3);
        assert!(
            native
                .io
                .calls
                .iter()
                .all(|call| !call.sql.contains("sys.dm_db_page_info(") && !call.sql.contains("sys.dm_exec_sql_text("))
        );
        assert_eq!(native.close().await, SourceShutdown::Quiescent);
    }
}

#[tokio::test]
async fn absent_original_does_not_get_replaced_by_another_active_page_waiter() {
    let focus = focus(&page_wait(42, "PAGELATCH_EX", "7:1:4144"));
    let mut native = reader(vec![Ok(vec![context()]), Ok(vec![context()]), Ok(vec![context()])]);
    let captured = native.capture_focused_page_wait_targets(&focus).await.unwrap();
    assert_eq!(captured.result.request, PageWaitRequestState::Missing);
    assert!(!captured.result.permits_page_lookup());
    assert!(captured.capture.snapshot.observations.is_empty());
    assert_eq!(native.query_count(), 3);
    assert!(native.io.calls[1].sql.contains("r.session_id=@P2 AND r.request_id=@P3"));
}

#[tokio::test]
async fn source_epoch_or_target_change_stops_before_the_original_target_query() {
    let original = page_wait(42, "PAGELATCH_EX", "7:1:4144");
    let focus = focus(&original);
    for (field, value) in [("physical_incarnation", "replacement"), ("database_name", "another-db")] {
        let mut native = reader(vec![Ok(vec![replace(&context(), &[(field, Val::Text(value.into()))])])]);
        let error = native.capture_focused_page_wait_targets(&focus).await.err().unwrap();
        assert_eq!(CollectionStop::from_error(&error), CollectionStop::SourceChanged);
        assert_eq!(native.query_count(), 1);
        assert_eq!(native.close().await, SourceShutdown::Quiescent);
    }
}

#[tokio::test]
async fn a_non_native_statement_token_or_source_is_rejected_before_any_work() {
    let focus = focus(&page_wait(42, "PAGELATCH_EX", "7:1:4144"));
    for source in [false, true] {
        let mut focus = focus.clone();
        if source {
            focus.source = "foreign-native-source".into();
        } else {
            focus.statement_identity = NativeIdentity::new("looks like a statement");
            focus.original.statement_identity = Some(focus.statement_identity.clone());
        }
        assert!(focus.is_valid());
        let mut native = reader(vec![]);
        assert!(native.capture_focused_page_wait_targets(&focus).await.is_err());
        assert_eq!(native.query_count(), 0);
    }
}

#[tokio::test]
async fn changed_request_at_the_pre_header_check_cannot_be_mapped_as_the_original_wait() {
    let row = page_wait(42, "PAGELATCH_EX", "7:1:4144");
    let focus = focus(&row);
    let mut supplied = successful_replies(vec![row]);
    let checked = supplied[2].as_ref().unwrap()[0].clone();
    supplied[2] = Ok(vec![replace(&checked, &[("checked_begin", Val::Int(2))])]);
    supplied.truncate(3);
    supplied.push(Ok(vec![context()]));
    let mut native = reader(supplied);
    let captured = native.capture_focused_page_wait_targets(&focus).await.unwrap();
    let point = &captured.capture.snapshot.observations[0];
    assert!(point.target.is_none());
    assert_eq!(point.before_check.as_ref().unwrap().request, PageWaitRequestState::Changed);
    assert_eq!(captured.result.request, PageWaitRequestState::Changed);
    assert!(!captured.result.permits_page_lookup());
    assert_eq!(native.query_count(), 4);
    assert!(native.io.calls.iter().all(|call| !call.sql.contains("OUTER APPLY sys.dm_db_page_info")));
}
