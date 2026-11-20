//! Shared caller labels remain context across concurrent backends, request reuse and ended sessions.
use super::{owned_fixture, retained_activity};
use culprit_core::{SessionCensusReport, telemetry::*};
mod actors;
mod fixture;
use fixture::Fixture;

const SOURCE_QUERY_CAP: u32 = 3 * 6 + 3 * 25;

#[tokio::test]
#[ignore = "requires the existing nonce-owned loopback PostgreSQL fixture; select caller_context:: explicitly"]
async fn shared_login_and_application_labels_cannot_replace_original_native_request_or_ended_backend() {
    Fixture::run(|fixture| Box::pin(prove(fixture))).await;
}

async fn prove(fixture: &mut Fixture) {
    fixture.prepare().await;
    let recipe = crate::telemetry_recipes().into_iter().find(|recipe| recipe.id == "postgres.activity").unwrap();
    assert_eq!(recipe.cost, RecipeCostQualification::Unqualified);
    let mut original = retained_activity::original(&fixture.specification, fixture.pid(0), &fixture.cancellation).await;
    let initial = original.capture().census();
    assert_shared_callers(&initial, &[fixture.identity(0).clone(), fixture.identity(1).clone()]);
    let original_report = serde_json::to_value(&initial).unwrap();
    let key = original.focus.original.key.clone();
    let peer = original
        .capture()
        .records
        .iter()
        .find_map(|record| match &record.record {
            TelemetryRecord::Activity(row) if row.key.session == *fixture.identity(1) => Some(row),
            _ => None,
        })
        .unwrap();
    assert_eq!(peer.client, original.focus.original.client);
    assert!(
        !original.focus.matches_parent(original.capture().source(), peer, &original.focus.parent),
        "a shared label cannot authenticate another native participant"
    );
    let mut reused_generation = original.focus.original.clone();
    // A deterministic negative receipt mutation, not a claim that this fixture forced operating-system PID reuse.
    reused_generation.key.session.generation = "2000-01-01T00:00:00.000001Z".into();
    assert_eq!(reused_generation.key.session.id, original.focus.original.key.session.id);
    assert!(!original.focus.matches_parent(original.capture().source(), &reused_generation, &original.focus.parent));
    let matched = original.blocking(&fixture.specification, &fixture.cancellation).await;
    assert_binding(&matched, FocusedRequestBinding::Matched);
    let report = blocking_report(&matched);
    assert!(!report.relationships.is_empty());
    for record in &matched.records {
        let TelemetryRecord::Activity(row) = &record.record else { unreachable!() };
        if row.idle {
            assert!(row.key.request.is_none());
            assert!(row.statement_identity.is_none());
            assert!(matches!(row.statement, DefinitionPayload::Unavailable { .. }));
        }
    }
    fixture.release_gate().await.unwrap();
    fixture.settle_callers().await.unwrap();
    let idle = original.census(&fixture.specification, &fixture.cancellation).await;
    let idle_report = idle.census();
    for identity in [fixture.identity(0), fixture.identity(1)] {
        let entry = idle_report.entries.iter().find(|entry| &entry.activity.session == identity).unwrap();
        assert!(entry.idle);
        assert!(entry.activity.request.is_none());
        let row = idle
            .records
            .iter()
            .find_map(|record| match &record.record {
                TelemetryRecord::Activity(row) if &row.key.session == identity => Some(row),
                _ => None,
            })
            .unwrap();
        assert!(row.statement_identity.is_none());
        assert!(matches!(row.statement, DefinitionPayload::Unavailable { .. }));
    }
    fixture.reuse_first().await.unwrap();
    let mut reused = retained_activity::original(&fixture.specification, fixture.pid(0), &fixture.cancellation).await;
    assert_eq!(reused.focus.original.key.session, key.session);
    assert_ne!(reused.focus.original.key.request, key.request);
    assert_ne!(reused.focus.statement_identity, original.focus.statement_identity);
    assert_eq!(reused.focus.original.client, original.focus.original.client);
    let changed = original.blocking(&fixture.specification, &fixture.cancellation).await;
    assert_binding(&changed, FocusedRequestBinding::ParticipantChanged);
    assert!(changed.records.is_empty());
    fixture.release_gate().await.unwrap();
    fixture.settle_first().await.unwrap();
    fixture.close_callers().await.unwrap();
    let missing = original.blocking(&fixture.specification, &fixture.cancellation).await;
    assert_binding(&missing, FocusedRequestBinding::ParticipantMissing);
    assert!(missing.records.is_empty());
    original.verify_retained().await;
    reused.verify_retained().await;
    assert_eq!(
        serde_json::to_value(original.capture().census()).unwrap(),
        original_report,
        "original labels, receipts and native generations survive real actor exit"
    );
    let captures = [original.capture(), &matched, &idle, reused.capture(), &changed, &missing];
    let queries = captures.iter().map(|capture| capture.manifest.source_queries).sum::<u32>();
    assert!(queries <= SOURCE_QUERY_CAP);
    assert!(fixture.fixture_queries() <= 13);
    eprintln!(
        "owned PG caller acceptance {}",
        serde_json::json!({
            "same_login_and_application":true,"independent_original_backends":2,"same_backend_new_request":true,"old_request_binding":FocusedRequestBinding::ParticipantChanged,
            "ended_binding":FocusedRequestBinding::ParticipantMissing,"original_after_actor_exit_verified":true,
            "source_captures":captures.len(),"source_queries":queries,"source_query_cap":SOURCE_QUERY_CAP,"fixture_command_cap":13,
            "native_pid_reuse_forced":false,"business_owner_inferred":false,"cost_qualified":false
        })
    );
}
fn assert_shared_callers(report: &SessionCensusReport, identities: &[NativeActivityIdentity]) {
    let entries = identities
        .iter()
        .map(|identity| report.entries.iter().find(|entry| &entry.activity.session == identity).unwrap())
        .collect::<Vec<_>>();
    assert_ne!(entries[0].activity.session, entries[1].activity.session);
    assert!(entries.iter().all(|entry| entry.activity.request.is_some() && !entry.idle));
    let field = |entry: &culprit_core::SessionCensusEntry, key: &str| {
        entry.fields.iter().find(|field| field.key == key).unwrap().value.clone()
    };
    assert_eq!(field(entries[0], "application").as_deref(), Some(actors::SHARED_APPLICATION));
    assert_eq!(field(entries[0], "application"), field(entries[1], "application"));
    assert!(field(entries[0], "login").is_some());
    assert_eq!(field(entries[0], "login"), field(entries[1], "login"));
    assert_ne!(entries[0].reference, entries[1].reference);
    assert!(report.limitations.contains(&culprit_core::t!("view.session_census.limit.client")));
}
fn assert_binding(capture: &owned_fixture::Captured, expected: FocusedRequestBinding) {
    let SourceObservation::FocusedBlocking { result, .. } = &capture.source().observation else { unreachable!() };
    assert_eq!(result.binding, expected);
    let report = blocking_report(capture);
    assert_eq!(report.binding, expected);
    if expected != FocusedRequestBinding::Matched {
        assert_eq!(result.membership, FocusedRequestMembership::NotObserved);
        assert!(report.relationships.is_empty());
        assert_eq!(report.request_end, BlockingRequestState::NotRead);
    }
}
fn blocking_report(capture: &owned_fixture::Captured) -> culprit_core::BlockingReport {
    let owned = capture
        .records
        .iter()
        .enumerate()
        .map(|(index, row)| match &row.record {
            TelemetryRecord::Activity(row) => (row.as_ref(), capture.reference(index)),
            _ => unreachable!(),
        })
        .collect::<Vec<_>>();
    let rows = owned
        .iter()
        .map(|(observation, reference)| ActivityFrameRow { observation, reference: Some(reference) })
        .collect::<Vec<_>>();
    culprit_checks::focused_blocking::explain(capture.source(), &rows, true).unwrap()
}
