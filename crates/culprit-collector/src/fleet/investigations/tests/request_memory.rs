//! Real sealed-outbox/runtime ownership with synthetic native facts; these receipts do not qualify production cost.
use super::{
    runtime::Runtime,
    support::{Frame, Harness},
};
use crate::{
    analytical::test_support::request_memory_evidence,
    capture::CollectionCancellation,
    diagnostics::analyze_pending,
    fleet::{journal::FocusedStage, scheduler::Outcome},
    outbox::{DiagnosticInputState, Outbox},
};
use culprit_core::telemetry::*;
use std::sync::atomic::Ordering;

pub(super) fn focused_frame(focus: &ActivityRequestFocus) -> Frame {
    let (_, mut records) =
        request_memory_evidence(DefinitionPayload::Unavailable { reason: PayloadUnavailable::NotCollected }, true);
    let TelemetryRecord::Operational(row) = &mut records[0] else { panic!() };
    let OperationalFact::ExecutionGrant(grant) = &mut row.fact else { panic!() };
    let at = (chrono::DateTime::parse_from_rfc3339(&focus.original_observed_at).unwrap()
        + chrono::Duration::seconds(1))
    .to_rfc3339();
    grant.identity = NativeIdentity::new("synthetic-native-grant")
        .qualified("session-generation", &focus.original.key.session.generation);
    grant.observed_at = at.clone();
    grant.state = ExecutionGrantState::Waiting;
    grant.granted_without_offset = None;
    grant.granted_kibibytes = None;
    grant.wait_ms = Some("40000".into());
    grant.native_null_columns.retain(|name| name != "wait_time_ms");
    grant.native_null_columns.extend(["grant_time".into(), "granted_memory_kb".into()]);
    grant.request = Some(focus.original.clone());
    grant.statement_relationship = if focus.original.statement.is_available() {
        GrantStatementRelationship::ObservedCurrentStatement
    } else {
        GrantStatementRelationship::Unresolved
    };
    row.key = grant.key().unwrap();
    Frame {
        source: CollectionDescription {
            target: focus.target.clone(),
            partition: focus.partition.clone(),
            source: row.source.clone(),
            continuity: SourceContinuity::Unverified,
            observation: SourceObservation::FocusedRequestMemory {
                snapshot: StateSnapshotDescription {
                    dataset: SnapshotDataset::CurrentRequestMemory,
                    observed_from: at.clone(),
                    observed_until: at.clone(),
                    records: 1,
                    sources: vec![StateSourceCoverage::observed(row.source.clone(), 1, true)],
                },
                result: Box::new(ActivityRequestMemoryFocusResult {
                    focus: focus.clone(),
                    observed_from: at.clone(),
                    observed_until: at,
                    source: FocusSourceState::Matched,
                    membership: RequestMemoryMembership::Observed,
                    subject: Some(grant.identity.clone()),
                    resource: grant.resource.clone(),
                    peers: vec![],
                }),
            },
        },
        records,
    }
}

async fn fixture() -> Harness {
    let mut fixture = Harness::new(1).await;
    let (memory_source, _) =
        request_memory_evidence(DefinitionPayload::Unavailable { reason: PayloadUnavailable::NotCollected }, true);
    let activity = &mut fixture.backend.activity;
    activity.source.partition = memory_source.partition;
    let SourceObservation::Activity(frame) = &mut activity.source.observation else { panic!() };
    frame.epoch = activity.source.partition.native_epoch().map(str::to_owned);
    frame.database_writable = Some(true);
    let TelemetryRecord::Activity(row) = &mut activity.records[0] else { panic!() };
    row.elapsed_ms = Some("1".into());
    row.waiting_on_lock = None;
    row.blocking_status = ActivityBlockingStatus::Unknown;
    let wait = row.wait.as_mut().unwrap();
    wait.class = Some(ActivityWaitClass::ExecutionGrant);
    wait.native_type = Some("synthetic-native-grant-wait".into());
    wait.identified_resources.clear();
    wait.owners.clear();
    fixture.backend.recipe.selector = RecipeSelector::FocusedRequestMemory;
    fixture.backend.recipe.id = "synthetic.focused-request-memory".into();
    let RecipeCostQualification::Measured(receipt) = &mut fixture.backend.recipe.cost else { panic!() };
    receipt.selector = RecipeSelector::FocusedRequestMemory;
    receipt.recipe_id = fixture.backend.recipe.id.clone();
    receipt.scope = DiagnosticScope::from_source(&activity.source).unwrap().into();
    fixture
}

#[tokio::test]
async fn an_admitted_memory_question_is_captured_consumed_and_replayed_without_replacing_its_original_episode() {
    let mut runtime = Runtime::from_fixture(fixture().await).await;
    assert!(runtime.enroll().await);
    let intents = runtime.fixture.journal.focused_intents().await.unwrap();
    let [intent] = intents.as_slice() else { panic!() };
    let intent = intent.clone();
    assert_eq!(intent.focus.selector(), RecipeSelector::FocusedRequestMemory);
    let key = intent.key().unwrap();
    let parent = intent.focus.parent().clone();
    let original = runtime.fixture.outbox.diagnostic_evidence(&parent).await.unwrap();
    let job = runtime.take_focus().await;
    let result = runtime.execute(&job).await;
    assert_eq!(result.event.status, CollectionStatus::Complete);
    assert_eq!(runtime.native_calls(), [2; 5]);
    let Outcome::FocusedComplete { result: capture } = &result.outcome else { panic!() };
    let capture = capture.clone();
    let manifest = runtime.fixture.outbox.verify(&capture.capture.observation_id).await.unwrap();
    assert_eq!(manifest.investigation_intent().unwrap().unwrap().key().unwrap(), key);
    let input = runtime
        .fixture
        .outbox
        .diagnostic_input(&capture.capture.observation_id, DIAGNOSTIC_CONTROLLER_VERSION)
        .await
        .unwrap();
    assert_eq!(input.rows.len(), 1);
    let SourceObservation::FocusedRequestMemory { result, .. } = &input.source.observation else { panic!() };
    assert_eq!(result.focus, *intent.focus.activity_participant().unwrap());
    let TelemetryRecord::Operational(OperationalObservation { fact: OperationalFact::ExecutionGrant(grant), .. }) =
        &input.rows[0].row.record
    else {
        panic!()
    };
    assert!(result.accepts_observation(grant));
    assert_eq!(grant.requested_kibibytes.as_deref(), Some("9007199254740993"));
    runtime.finish(Outcome::FocusedComplete { result: capture.clone() }).await;
    runtime.reconcile().await;
    assert!(
        runtime.fixture.journal.focused_intent(&key).await.unwrap().is_some(),
        "unconsumed evidence retains its canonical intent and pin"
    );
    let policies = runtime.policies();
    assert_eq!(
        analyze_pending(&mut runtime.fixture.outbox, &policies, &CollectionCancellation::default(), 10, None)
            .await
            .unwrap(),
        1
    );
    let revisions = runtime.revisions(&capture.capture).await;
    let [revision] = revisions.as_slice() else { panic!() };
    assert_eq!(revision.episode.id, intent.episode_id);
    assert_eq!(revision.episode.lifecycle, DiagnosticLifecycle::Active);
    assert_eq!(revision.episode.analysis, DiagnosticAnalysisStatus::Analyzed);
    assert!(revision.episode.recovery.is_none());
    assert_eq!(revision.episode.latest, parent);
    assert!(matches!(revision.episode.report, Some(DiagnosticReport::RequestMemory { .. })));
    assert_eq!(
        runtime.fixture.outbox.diagnostic_input_state(&capture, DIAGNOSTIC_CONTROLLER_VERSION).await.unwrap(),
        DiagnosticInputState::Consumed
    );
    runtime.reconcile().await;
    assert!(runtime.fixture.journal.focused_intent(&key).await.unwrap().is_none());
    assert!(runtime.fixture.outbox.investigation_parent_keys().await.unwrap().is_empty());
    assert!(matches!(
        runtime.fixture.journal.focused_stage(&intent.episode_stage_key().unwrap()).await.unwrap(),
        Some(FocusedStage::Completed)
    ));
    assert!(!runtime.enroll().await, "the same active original symptom cannot spin another completed memory question");
    assert_eq!(runtime.native_calls(), [2; 5]);
    assert_eq!(runtime.fixture.outbox.diagnostic_evidence(&parent).await.unwrap(), original);
    let Harness { _directory: directory, outbox, journal, .. } = runtime.fixture;
    drop(outbox);
    drop(journal);
    let mut reopened = Outbox::open(&directory.path().join("outbox")).unwrap();
    let recovered = reopened.recover().await.unwrap();
    assert_eq!(recovered.source_debts, 0);
    let restored =
        reopened.diagnostic_input(&capture.capture.observation_id, DIAGNOSTIC_CONTROLLER_VERSION).await.unwrap();
    assert_eq!(restored.investigation_intent().unwrap().unwrap().key().unwrap(), key);
    assert_eq!(reopened.diagnostic_evidence(&parent).await.unwrap(), original);
}

#[tokio::test]
async fn unqualified_or_rejected_memory_recipes_never_open_a_focused_source() {
    for unqualified in [true, false] {
        let mut fixture = fixture().await;
        if unqualified {
            fixture.backend.recipe.cost = RecipeCostQualification::Unqualified;
        } else {
            fixture.backend.accept = false;
        }
        let mut runtime = Runtime::from_fixture(fixture).await;
        let calls = runtime.native_calls();
        assert!(!runtime.enroll().await);
        assert_eq!(runtime.native_calls(), calls);
        assert_eq!(runtime.fixture.backend.verified.load(Ordering::SeqCst), usize::from(!unqualified));
        assert!(runtime.fixture.journal.focused_intents().await.unwrap().is_empty());
        assert!(runtime.fixture.outbox.investigation_parent_keys().await.unwrap().is_empty());
    }
}
