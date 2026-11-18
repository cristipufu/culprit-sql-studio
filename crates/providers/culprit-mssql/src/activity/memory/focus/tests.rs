use super::*;
use crate::{
    activity::memory::tests::{columns, context, reader_with_identity, row},
    activity::tests::details,
    resources::test_support::replace,
    value::Val,
};

pub(crate) fn native_row() -> Rec {
    replace(
        &row(),
        &[
            ("wait_type", Val::Text("RESOURCE_SEMAPHORE".into())),
            ("blocking_session_id", Val::Int(0)),
            ("grant_granted_without_offset", Val::Null),
            ("grant_granted_memory_kb", Val::Null),
            ("grant_wait_time_ms", Val::Int(40000)),
        ],
    )
}

pub(crate) fn original_focus() -> ActivityRequestFocus {
    let row = native_row();
    let authority = crate::activity::database_context::test_azure_authority(&row).unwrap();
    let context = authority.capture_context().unwrap();
    let original = crate::activity::rows::record(&row).unwrap().unwrap().facts;
    let source = CollectionDescription {
        target: context.database.clone().into(),
        partition: authority.source_partition().unwrap(),
        source: culprit_core::source_id::SESSIONS.into(),
        continuity: SourceContinuity::Unverified,
        observation: SourceObservation::Activity(ActivitySnapshotDescription {
            database_writable: context.writable,
            epoch: context.epoch.clone(),
            at: context.at.clone(),
            observed_from: context.at.clone(),
            observed_until: context.at.clone(),
            population: ActivityPopulation::ActiveRequestsOpenTransactionsAndBlockers,
            scope: MeasurementScope::Database,
            native_rows: 1,
            observations: 1,
            coverage: StateSourceCoverage::observed(culprit_core::source_id::SESSIONS.into(), 1, true),
        }),
    };
    let capture = CaptureIdentity {
        observation_id: "original-execution-grant-wait".into(),
        revision: Some(CaptureRevision {
            outbox_id: "sealed-native-fixture".into(),
            owner_epoch: "1".into(),
            sequence: "1".into(),
        }),
    };
    let exported = ExportedRecord::new(&source, &capture, TelemetryRecord::from(original.clone())).unwrap();
    let parent =
        DiagnosticEvidenceRef::from_export(&source, &digest(b"original execution-grant native manifest"), &exported, 0)
            .unwrap();
    ActivityRequestFocus {
        parent,
        target: source.target,
        partition: source.partition,
        database_writable: context.writable,
        source: source.source,
        epoch: context.epoch.unwrap(),
        original_observed_at: context.at,
        statement_identity: original.statement_identity.clone().unwrap(),
        original,
    }
}

pub(crate) fn replies(rows: Vec<Rec>, enrich: bool) -> Vec<crate::read::ReadResult<Vec<Rec>>> {
    let mut values = vec![Ok(vec![crate::grants::tests::platform()]), Ok(vec![context()]), Ok(columns()), Ok(rows)];
    if enrich {
        values.push(Ok(vec![details(42, "SELECT * FROM dbo.orders WHERE id=42")]));
    }
    values.push(Ok(vec![context()]));
    values
}

#[test]
fn only_the_native_execution_grant_wait_class_authorizes_the_memory_question() {
    for (name, expected) in [
        ("RESOURCE_SEMAPHORE", Some(ActivityWaitClass::ExecutionGrant)),
        ("RESOURCE_SEMAPHORE_QUERY_COMPILE", Some(ActivityWaitClass::CompilationAdmission)),
        ("CMEMTHREAD", None),
        ("MEMORY_ALLOCATION_EXT", None),
        ("RESOURCE_SEMAPHORE_FUTURE", None),
    ] {
        let row = replace(&native_row(), &[("wait_type", Val::Text(name.into()))]);
        assert_eq!(crate::activity::rows::record(&row).unwrap().unwrap().facts.wait.unwrap().class, expected, "{name}");
    }
}

#[tokio::test]
async fn the_original_generation_and_statement_are_filtered_before_sql_enrichment_without_an_extra_read() {
    let original = original_focus();
    let other = replace(&native_row(), &[("session_id", Val::Int(43)), ("grant_session_id", Val::Int(43))]);
    let mut reader = reader_with_identity(replies(vec![native_row(), other], true), IdentityDetail::AzureDatabase);
    let focused = reader.capture_focused_memory(&original).await.unwrap();
    assert_eq!(focused.result.focus, original);
    assert_eq!(focused.result.membership, RequestMemoryMembership::Observed);
    assert_eq!(focused.capture.observations.len(), 2);
    assert_eq!(focused.capture.observations[0].requested_kibibytes.as_deref(), Some("9007199254740993"));
    assert!(focused.result.accepts_observation(&focused.capture.observations[0]));
    assert_eq!(focused.result.subject.as_ref(), Some(&focused.capture.observations[0].identity));
    assert_eq!(focused.result.peers, vec![focused.capture.observations[1].identity.clone()]);
    assert!(!focused.capture.observations[1].request.as_ref().unwrap().statement.is_available());
    assert_eq!(focused.capture.observations[1].statement_relationship, GrantStatementRelationship::Unresolved);
    assert!(focused.result.matches_observations(&focused.capture.observations));
    assert_eq!(reader.query_count(), 5);
    let [Parameter::Text(selectors), ..] = reader.io.calls[4].parameters.as_slice() else { panic!() };
    assert!(selectors.contains("42"));
    assert!(!selectors.contains("43"));
    assert!(reader.verify_memory_context(&focused.capture.owner).await.unwrap().has_qualified_local_owner());
    assert_eq!(reader.query_count(), 6, "same query count as the original snapshot reader");
    assert_eq!(reader.close().await, SourceShutdown::Quiescent);
}

#[tokio::test]
async fn a_recycled_request_changed_statement_or_unverified_handle_never_gets_replacement_sql() {
    for changes in [
        vec![("request_generation", Val::Text("2020-01-01T07:58:02".into()))],
        vec![("statement_start", Val::Int(2))],
        vec![("grant_sql_handle", Val::Null)],
    ] {
        let mut reader =
            reader_with_identity(replies(vec![replace(&native_row(), &changes)], false), IdentityDetail::AzureDatabase);
        let focused = reader.capture_focused_memory(&original_focus()).await.unwrap();
        assert_eq!(focused.result.membership, RequestMemoryMembership::Unverified);
        assert!(focused.capture.observations.is_empty());
        assert_eq!(reader.query_count(), 4, "no SQL detail read for an unqualified replacement");
        reader.verify_memory_context(&focused.capture.owner).await.unwrap();
        assert_eq!(reader.close().await, SourceShutdown::Quiescent);
    }
}

#[tokio::test]
async fn an_absent_grant_is_not_a_missing_request_or_permission_to_enrich_another_participant() {
    let other = replace(&native_row(), &[("session_id", Val::Int(43)), ("grant_session_id", Val::Int(43))]);
    let mut reader = reader_with_identity(replies(vec![other], false), IdentityDetail::AzureDatabase);
    let focused = reader.capture_focused_memory(&original_focus()).await.unwrap();
    assert_eq!(focused.result.membership, RequestMemoryMembership::NotObserved);
    assert!(focused.capture.observations.is_empty());
    assert_eq!(reader.query_count(), 4);
    assert_eq!(reader.close().await, SourceShutdown::Quiescent);
}

#[tokio::test]
async fn a_changed_native_owner_or_unknown_parent_role_stops_before_grant_columns_or_observations() {
    for unknown_role in [false, true] {
        let mut focus = original_focus();
        let observed = if unknown_role {
            focus.database_writable = None;
            context()
        } else {
            replace(&context(), &[("physical_incarnation", Val::Text("different-native-database-generation".into()))])
        };
        let mut reader = reader_with_identity(
            vec![Ok(vec![crate::grants::tests::platform()]), Ok(vec![observed])],
            IdentityDetail::AzureDatabase,
        );
        let error = reader.capture_focused_memory(&focus).await.err().unwrap();
        assert_eq!(
            CollectionStop::from_error(&error),
            if unknown_role { CollectionStop::SourceUnavailable } else { CollectionStop::SourceChanged }
        );
        assert_eq!(reader.query_count(), 2);
        assert_eq!(reader.close().await, SourceShutdown::Quiescent);
    }
}

#[tokio::test]
async fn only_verified_same_resource_peers_survive_and_masked_resources_never_gain_a_roster() {
    for masked in [false, true] {
        let original = if masked { replace(&native_row(), &[("grant_pool_id", Val::Null)]) } else { native_row() };
        let peer = replace(&native_row(), &[("session_id", Val::Int(43)), ("grant_session_id", Val::Int(43))]);
        let other_resource = replace(
            &peer,
            &[("session_id", Val::Int(44)), ("grant_session_id", Val::Int(44)), ("grant_semaphore_id", Val::Int(1))],
        );
        let changed = replace(
            &peer,
            &[("session_id", Val::Int(45)), ("grant_session_id", Val::Int(45)), ("grant_sql_handle", Val::Null)],
        );
        let foreign = replace(
            &peer,
            &[("session_id", Val::Int(46)), ("grant_session_id", Val::Int(46)), ("scope_basis", Val::Int(5))],
        );
        let mut reader = reader_with_identity(
            replies(vec![original, peer, other_resource, changed, foreign], true),
            IdentityDetail::AzureDatabase,
        );
        let focused = reader.capture_focused_memory(&original_focus()).await.unwrap();
        assert_eq!(focused.capture.observations.len(), if masked { 1 } else { 2 });
        assert_eq!(focused.result.peers.len(), usize::from(!masked));
        assert_eq!(focused.result.resource.is_none(), masked);
        assert!(focused.result.matches_observations(&focused.capture.observations));
        assert_eq!(reader.query_count(), 5, "peer metadata uses the existing native frame");
        assert_eq!(reader.close().await, SourceShutdown::Quiescent);
    }
}

#[tokio::test]
async fn one_row_cost_approval_cannot_read_or_publish_a_larger_native_set_by_filtering_it() {
    let peer = replace(&native_row(), &[("session_id", Val::Int(43)), ("grant_session_id", Val::Int(43))]);
    let io = crate::resources::test_support::FakeRead::new(replies(vec![native_row(), peer], false));
    let options = crate::activity::ReadOptions {
        budget: CollectionBudget { max_snapshot_rows: 1, ..Default::default() },
        identity: IdentityDetail::AzureDatabase,
    };
    let mut reader = crate::activity::memory::tests::reader_with_options(io, options);
    let error = reader.capture_focused_memory(&original_focus()).await.err().unwrap();
    assert_eq!(CollectionStop::from_error(&error), CollectionStop::RowLimit);
    assert_eq!(reader.query_count(), 4, "the limit applies before selection and SQL enrichment");
    assert_eq!(reader.io.calls[3].limit, 2, "one native allowance plus its overflow sentinel");
    assert_eq!(reader.close().await, SourceShutdown::Quiescent);
}
