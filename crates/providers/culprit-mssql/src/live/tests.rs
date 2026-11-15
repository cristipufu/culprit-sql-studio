use super::*;
use crate::{
    activity::{ActivityReader, ActivityScope, IdentityDetail, ReadOptions},
    read::ReadError,
    resources::test_support::{FakeRead, replace},
    value::Val,
};
use culprit_core::{Provider, telemetry::*};
pub(super) mod support;
use support::{authority, platform, target};

fn activity_reader(replies: Vec<crate::read::ReadResult<Vec<crate::value::Rec>>>) -> ActivityReader<FakeRead> {
    ActivityReader::new(
        FakeRead::new(replies),
        ReadOptions {
            budget: CollectionBudget::default(),
            scope: ActivityScope::Visible,
            identity: IdentityDetail::Endpoint,
        },
        "fixture.invalid".into(),
        1433,
    )
}

#[tokio::test]
async fn production_live_reads_keep_the_original_query_count_and_do_not_acquire_action_authority() {
    let mut reader = activity_reader(vec![
        Ok(vec![crate::activity::tests::metadata(42)]),
        Ok(vec![crate::activity::tests::details(42, "SELECT original_statement")]),
    ]);
    let live = collect_qualified_from(&mut reader, false).await.unwrap();
    assert_eq!(reader.query_count(), 2);
    assert!(live.sessions[0].action_target.is_none());
    assert!(live.session_actions.is_empty());
    assert!(live.session_action_route.is_none());
    assert_eq!(live.sessions[0].sql.as_deref(), Some("SELECT original_statement"));
}

#[tokio::test]
async fn nonproduction_targets_keep_the_original_native_key_and_verified_member_not_an_endpoint_epoch() {
    let mut reader = activity_reader(vec![
        Ok(vec![platform()]),
        Ok(vec![authority()]),
        Ok(vec![crate::activity::tests::metadata(42)]),
        Ok(vec![crate::activity::tests::details(42, "SELECT original_statement")]),
        Ok(vec![authority()]),
    ]);
    let live = collect_qualified_from(&mut reader, true).await.unwrap();
    let action = live.sessions[0].action_target.as_ref().unwrap();
    assert_eq!(action.activity, target().activity);
    assert!(action.context.has_qualified_local_owner());
    assert_eq!(action.context, target().context);
    assert!(action.statement.is_some(), "retain the original native handle and offsets");
    assert_eq!(live.session_actions, vec![SessionAction::Terminate]);
    assert_eq!(reader.query_count(), 5);
}

#[tokio::test]
async fn missing_member_or_denied_authority_keeps_live_positives_without_an_action_target() {
    for denied in [false, true] {
        let unknown = replace(&authority(), &[("replica_id", Val::Null)]);
        let mut replies = if denied {
            vec![Err(ReadError::Source(culprit_core::Error::new(culprit_core::ErrorCode::PermissionDenied, "fixture")))]
        } else {
            vec![Ok(vec![platform()]), Ok(vec![unknown.clone()])]
        };
        let frame = replace(
            &crate::activity::tests::metadata(42),
            &[(
                "replica_id",
                if denied { Val::Text("91487244-730d-4b83-8e49-d482241c7288".into()) } else { Val::Null },
            )],
        );
        replies.extend([Ok(vec![frame]), Ok(vec![crate::activity::tests::details(42, "SELECT original_statement")])]);
        if !denied {
            replies.push(Ok(vec![unknown]));
        }
        let mut reader = activity_reader(replies);
        let live = collect_qualified_from(&mut reader, true).await.unwrap();
        assert_eq!(live.sessions.len(), 1);
        assert_eq!(live.sessions[0].sql.as_deref(), Some("SELECT original_statement"));
        assert!(live.sessions[0].action_target.is_none());
        assert!(live.session_actions.is_empty());
        assert!(!live.notes.is_empty());
    }
}

#[tokio::test]
async fn foreign_database_sessions_remain_visible_without_borrowing_the_selected_database_target() {
    let foreign = replace(
        &crate::activity::tests::metadata(42),
        &[("session_database", Val::Text("other_database".into())), ("scope_basis", Val::Int(5))],
    );
    let mut reader = activity_reader(vec![
        Ok(vec![platform()]),
        Ok(vec![authority()]),
        Ok(vec![foreign]),
        Ok(vec![crate::activity::tests::details(42, "SELECT foreign_context")]),
        Ok(vec![authority()]),
    ]);
    let live = collect_qualified_from(&mut reader, true).await.unwrap();
    assert_eq!(live.sessions[0].database.as_deref(), Some("other_database"));
    assert!(live.sessions[0].action_target.is_none());
    assert!(live.session_actions.is_empty());
}

#[tokio::test]
async fn malformed_native_authority_is_not_hidden_as_an_unavailable_action() {
    let mut reader = activity_reader(vec![
        Ok(vec![platform()]),
        Ok(vec![replace(&authority(), &[("database_name", Val::Text(String::new()))])]),
    ]);
    let error = collect_qualified_from(&mut reader, true).await.unwrap_err();
    assert_eq!(CollectionStop::from_error(&error), CollectionStop::InvalidPage);
    assert_eq!(reader.query_count(), 2);
}

#[tokio::test]
async fn direct_provider_actions_still_refuse_production_before_any_native_work() {
    let spec = culprit_core::ConnectionSpec {
        engine: Some(culprit_core::Engine::Mssql),
        host: "fixture.invalid".into(),
        user: Some("fixture".into()),
        password: Some("fixture".into()),
        environment: culprit_core::Environment::Production,
        ..Default::default()
    };
    let provider = crate::MssqlProvider::new(&spec).unwrap();
    let error = provider.interrupt_session(&target(), SessionAction::Terminate).await.unwrap_err();
    assert_eq!(error.code, culprit_core::ErrorCode::PermissionDenied);
    assert_eq!(provider.source_shutdown(), SourceShutdown::Quiescent);
}
