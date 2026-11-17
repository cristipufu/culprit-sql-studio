//! The actual adaptive owner runs on sealed synthetic native events, never on qualified production cost.
use super::{
    runtime::Runtime,
    support::{Frame, Harness},
};
use crate::{
    analytical::test_support::investigation_fixture,
    capture::CollectionCancellation,
    diagnostics::analyze_pending,
    fleet::{journal::FocusedStage, scheduler::Outcome},
    outbox::{DiagnosticInputState, DiagnosticUpdate, Outbox},
};
use culprit_core::{DatabaseIdentity, Engine, telemetry::*};
use std::sync::atomic::Ordering;

fn activity() -> Frame {
    let original = investigation_fixture();
    let mut source = original.baseline_source;
    let mut record = original.baseline_record.record;
    source.target = DatabaseIdentity::server(Engine::Mysql, "app", "fixture.invalid", 3306, None).into();
    source.partition = SourcePartition::InstanceLocal {
        member: Some(SharedResourceIdentity {
            engine: Engine::Mysql,
            kind: SharedResourceKind::Node,
            namespace: NativeIdentity::new("synthetic-native-system"),
            native: NativeIdentity::new("synthetic-native-member"),
        }),
        epoch: source.observation.activity().unwrap().epoch.clone(),
    };
    let SourceObservation::Activity(snapshot) = &mut source.observation else { panic!() };
    snapshot.database_writable = Some(true);
    let TelemetryRecord::Activity(row) = &mut record else { panic!() };
    row.key = ActivityKey {
        session: NativeActivityIdentity { id: "51".into(), generation: "700".into() },
        request: Some(NativeActivityIdentity { id: "19".into(), generation: "1000".into() }),
        transaction: None,
    };
    row.scope = ActivityScopeBasis::RequestDatabase;
    row.database = Some("app".into());
    row.idle = false;
    row.wait = None;
    row.state = "executing".into();
    row.command = Some("SELECT".into());
    row.waiting_on_lock = None;
    row.blocking_status = ActivityBlockingStatus::Known;
    row.elapsed_ms = Some("120000".into());
    row.statement = DefinitionPayload::Unavailable { reason: PayloadUnavailable::NotCollected };
    row.statement_identity = row
        .key
        .request
        .as_ref()
        .and_then(|request| culprit_core::engines::mysql_activity_statement_identity(&row.key.session, request));
    assert!(row.is_valid());
    Frame { source, records: vec![record] }
}

async fn fixture() -> Harness {
    let mut fixture = Harness::with_activity(activity()).await;
    fixture.backend.recipe.id = "synthetic.current-request-work".into();
    fixture.backend.recipe.selector = RecipeSelector::CurrentRequestWork;
    let RecipeCostQualification::Measured(receipt) = &mut fixture.backend.recipe.cost else { panic!() };
    receipt.recipe_id = fixture.backend.recipe.id.clone();
    receipt.selector = RecipeSelector::CurrentRequestWork;
    assert!(fixture.backend.recipe.is_valid());
    fixture
}

pub(super) fn focused_frame(focus: &ActivityRequestFocus) -> Frame {
    let observed_at = (chrono::DateTime::parse_from_rfc3339(&focus.original_observed_at).unwrap()
        + chrono::Duration::seconds(1))
    .to_rfc3339();
    let request = focus.original.key.request.as_ref().unwrap();
    let point = CurrentRequestWorkObservation {
        observed_at: observed_at.clone(),
        actor: focus.original.key.clone(),
        database: "app".into(),
        scope: focus.original.scope,
        statement_identity: focus.statement_identity.clone(),
        statement: DefinitionPayload::Unavailable { reason: PayloadUnavailable::NotCollected },
        native: CurrentRequestWorkNative::Mysql {
            work: Box::new(MysqlCurrentRequestWork {
                thread_id: focus.original.key.session.generation.clone(),
                statement_event_id: request.id.clone(),
                timer_start_ps: request.generation.clone(),
                timer_wait_ps: Some("9007199254740993".into()),
                rows_examined: Some("9007199254740993".into()),
                rows_sent: Some("1".into()),
                created_tmp_tables: Some("3".into()),
                created_tmp_disk_tables: Some("2".into()),
                sort_merge_passes: Some("7".into()),
                stage: MysqlStageObservation::NotObserved,
            }),
        },
    };
    let source = "synthetic-current-request-work".to_owned();
    Frame {
        source: CollectionDescription {
            target: focus.target.clone(),
            partition: focus.partition.clone(),
            source: source.clone(),
            continuity: SourceContinuity::Unverified,
            observation: SourceObservation::CurrentRequestWork {
                profile: CurrentRequestWorkProfile::MysqlStatementEvent,
                snapshot: StateSnapshotDescription {
                    dataset: SnapshotDataset::CurrentRequestWork,
                    observed_from: observed_at.clone(),
                    observed_until: observed_at,
                    records: 1,
                    sources: vec![StateSourceCoverage::collected(source.clone(), 1)],
                },
                result: Box::new(FocusedRequestResult {
                    focus: Some(Box::new(focus.clone())),
                    database_writable: focus.database_writable,
                    binding: FocusedRequestBinding::Matched,
                    membership: FocusedRequestMembership::Observed,
                }),
            },
        },
        records: vec![TelemetryRecord::Operational(OperationalObservation {
            source,
            key: point.key().unwrap(),
            fact: OperationalFact::CurrentRequestWork(Box::new(point)),
        })],
    }
}

fn missing_frame(focus: &ActivityRequestFocus) -> Frame {
    let mut frame = focused_frame(focus);
    let SourceObservation::CurrentRequestWork { snapshot, result, .. } = &mut frame.source.observation else {
        panic!()
    };
    snapshot.records = 0;
    snapshot.sources[0] = StateSourceCoverage::observed(frame.source.source.clone(), 0, false);
    result.binding = FocusedRequestBinding::ParticipantMissing;
    result.membership = FocusedRequestMembership::NotObserved;
    frame.records.clear();
    frame
}

async fn selected_intent(runtime: &Runtime) -> DiagnosticInvestigationIntent {
    let intents = runtime.fixture.journal.focused_intents().await.unwrap();
    let [intent] = intents.as_slice() else { panic!("one exact current-work stage") };
    intent.clone()
}

#[tokio::test]
async fn one_admitted_native_event_is_captured_consumed_and_pinned_on_the_original_long_running_episode() {
    let mut runtime = Runtime::from_fixture(fixture().await).await;
    assert!(runtime.enroll().await);
    let intent = selected_intent(&runtime).await;
    assert_eq!(intent.focus.selector(), RecipeSelector::CurrentRequestWork);
    let original = runtime.fixture.outbox.diagnostic_evidence(&intent.focus.participant().parent).await.unwrap();
    let job = runtime.take_focus().await;
    let result = runtime.execute(&job).await;
    assert_eq!(result.event.status, CollectionStatus::Complete);
    let Outcome::FocusedComplete { result: capture } = &result.outcome else { panic!() };
    let capture = capture.clone();
    let manifest = runtime.fixture.outbox.verify(&capture.capture.observation_id).await.unwrap();
    assert_eq!(manifest.investigation_intent().unwrap(), Some(intent.clone()));
    runtime.finish(result.outcome).await;
    let calls = runtime.native_calls();
    let policies = runtime.policies();
    assert_eq!(
        analyze_pending(&mut runtime.fixture.outbox, &policies, &CollectionCancellation::default(), 10, None)
            .await
            .unwrap(),
        1
    );
    assert_eq!(runtime.native_calls(), calls, "offline interpretation performs no new source work");
    let revisions = runtime.revisions(&capture.capture).await;
    let [revision] = revisions.as_slice() else { panic!() };
    assert_eq!(revision.episode.id, intent.episode_id);
    assert_eq!(revision.episode.analysis, DiagnosticAnalysisStatus::Analyzed);
    assert_eq!(revision.episode.lifecycle, DiagnosticLifecycle::Active);
    assert_eq!(revision.episode.latest, intent.focus.participant().parent);
    let report = revision.episode.report.as_ref().unwrap();
    assert!(matches!(report, DiagnosticReport::CurrentRequestWork { .. }));
    assert_eq!(report.references().count(), 1);
    assert!(revision.inputs.contains(&intent.focus.participant().parent));
    assert!(revision.inputs.contains(report.evidence()));
    assert_eq!(runtime.fixture.outbox.diagnostic_evidence(&intent.focus.participant().parent).await.unwrap(), original);
    assert_eq!(
        runtime.fixture.outbox.diagnostic_input_state(&capture, DIAGNOSTIC_CONTROLLER_VERSION).await.unwrap(),
        DiagnosticInputState::Consumed
    );
    runtime.reconcile().await;
    assert!(!runtime.enroll().await);
    assert_eq!(runtime.native_calls(), calls);
}

#[tokio::test]
async fn missing_current_work_after_symptom_recovery_retains_debt_history_and_one_completed_stage_across_restart() {
    let mut runtime = Runtime::from_fixture(fixture().await).await;
    runtime.complete_empty_baseline().await;
    let checkpoints =
        runtime.fixture.outbox.diagnostic_checkpoints(DIAGNOSTIC_CONTROLLER_VERSION, None, 10).await.unwrap();
    let recovered = checkpoints
        .checkpoints
        .into_iter()
        .flat_map(|checkpoint| checkpoint.episodes)
        .find(|episode| {
            matches!(episode.signal, DiagnosticSignal::Activity { condition: ActivityCondition::LongRunning, .. })
        })
        .unwrap();
    assert_eq!(recovered.lifecycle, DiagnosticLifecycle::Resolved);
    assert!(recovered.analysis.needs_work());
    assert!(runtime.enroll().await);
    let intent = selected_intent(&runtime).await;
    runtime.fixture.backend.current_work = Some(missing_frame(intent.focus.participant()));
    let job = runtime.take_focus().await;
    let result = runtime.execute(&job).await;
    let Outcome::FocusedComplete { result: capture } = &result.outcome else { panic!() };
    let capture = capture.clone();
    runtime.finish(result.outcome).await;
    let policies = runtime.policies();
    analyze_pending(&mut runtime.fixture.outbox, &policies, &CollectionCancellation::default(), 10, None)
        .await
        .unwrap();
    let revisions = runtime.revisions(&capture.capture).await;
    let next = &revisions[0].episode;
    assert_eq!(next.lifecycle, DiagnosticLifecycle::Resolved);
    assert_eq!(next.analysis, DiagnosticAnalysisStatus::Partial);
    assert_eq!(next.issue, Some(DiagnosticAnalysisIssue::NativeContextGone));
    assert_eq!(next.recovery, recovered.recovery);
    assert_eq!(next.first, recovered.first);
    assert!(next.report.is_none() && next.retained_in_checkpoint());
    runtime.reconcile().await;
    assert!(matches!(
        runtime.fixture.journal.focused_stage(&intent.episode_stage_key().unwrap()).await.unwrap(),
        Some(FocusedStage::Completed)
    ));
    let calls = runtime.native_calls();
    assert!(!runtime.enroll().await, "analysis debt cannot turn the same consumed stage into repeated native polling");
    assert_eq!(runtime.native_calls(), calls);
    let expected = runtime
        .fixture
        .outbox
        .diagnostic_checkpoints(DIAGNOSTIC_CONTROLLER_VERSION, None, 10)
        .await
        .unwrap()
        .checkpoints;
    let Harness { _directory, inventory, outbox, journal, bindings, backend } = runtime.fixture;
    let path = _directory.path().join("outbox");
    let journal_path = _directory.path().join("fleet");
    drop(outbox);
    drop(journal);
    let mut outbox = Outbox::open(&path).unwrap();
    outbox.recover().await.unwrap();
    let journal = crate::fleet::FleetJournal::open(&journal_path, &inventory, "one").await.unwrap();
    let mut restarted = Harness { _directory, inventory, outbox, journal, bindings, backend };
    assert_eq!(
        restarted.outbox.diagnostic_checkpoints(DIAGNOSTIC_CONTROLLER_VERSION, None, 10).await.unwrap().checkpoints,
        expected
    );
    assert!(restarted.outbox.diagnostic_evidence(&recovered.first).await.is_ok());
    assert!(matches!(
        restarted.journal.focused_stage(&intent.episode_stage_key().unwrap()).await.unwrap(),
        Some(FocusedStage::Completed)
    ));
    assert!(
        restarted.prepare(&CollectionCancellation::default()).await.unwrap().is_empty(),
        "reopened durable stage cannot dispatch the same native question"
    );
    assert_eq!(restarted.backend.calls.opened.load(Ordering::SeqCst), calls[0]);
    assert_eq!(restarted.backend.calls.captured.load(Ordering::SeqCst), calls[4]);
}

#[tokio::test]
async fn unqualified_or_selector_mismatched_cost_never_enrolls_a_current_work_source() {
    for unqualified in [true, false] {
        let mut fixture = fixture().await;
        if unqualified {
            fixture.backend.recipe.cost = RecipeCostQualification::Unqualified;
        } else {
            let RecipeCostQualification::Measured(receipt) = &mut fixture.backend.recipe.cost else { panic!() };
            receipt.selector = RecipeSelector::FocusedRequestMemory;
            assert!(!fixture.backend.recipe.is_valid());
        }
        if !unqualified {
            assert!(fixture.prepare(&CollectionCancellation::default()).await.is_err());
            assert_eq!(fixture.backend.calls.opened.load(Ordering::SeqCst), 0);
        } else {
            let mut runtime = Runtime::from_fixture(fixture).await;
            assert!(!runtime.enroll().await);
            assert_eq!(runtime.native_calls(), [1; 5]);
            assert_eq!(runtime.fixture.backend.components.load(Ordering::SeqCst), 0);
            assert_eq!(runtime.fixture.backend.verified.load(Ordering::SeqCst), 0);
        }
    }
}

#[tokio::test]
async fn a_caller_cannot_commit_partial_current_work_as_a_completed_native_explanation() {
    let mut runtime = Runtime::from_fixture(fixture().await).await;
    assert!(runtime.enroll().await);
    let intent = selected_intent(&runtime).await;
    let mut frame = focused_frame(intent.focus.participant());
    let SourceObservation::CurrentRequestWork { snapshot, .. } = &mut frame.source.observation else { panic!() };
    snapshot.sources[0] = StateSourceCoverage::observed(frame.source.source.clone(), 1, false);
    runtime.fixture.backend.current_work = Some(frame);
    let job = runtime.take_focus().await;
    let result = runtime.execute(&job).await;
    let Outcome::FocusedComplete { result: capture } = &result.outcome else { panic!() };
    let capture = capture.clone();
    runtime.finish(result.outcome).await;
    let input = runtime
        .fixture
        .outbox
        .diagnostic_input(&capture.capture.observation_id, DIAGNOSTIC_CONTROLLER_VERSION)
        .await
        .unwrap();
    let rows = input
        .rows
        .iter()
        .map(|row| match &row.row.record {
            TelemetryRecord::Operational(observation) => {
                DiagnosticCurrentRequestWorkRow { reference: &row.reference, observation }
            }
            _ => panic!(),
        })
        .collect::<Vec<_>>();
    let at = "2020-01-01T13:00:00Z";
    let mut analysis = analyze_diagnostic_current_request_work(
        &input.source,
        &input.parent,
        &rows,
        input.checkpoint.as_ref().unwrap(),
        at,
        Some(&intent),
    )
    .unwrap();
    assert_eq!(analysis.revisions[0].episode.analysis, DiagnosticAnalysisStatus::Partial);
    analysis.revisions[0].episode.analysis = DiagnosticAnalysisStatus::Analyzed;
    analysis.revisions[0].id = analysis.revisions[0].expected_id().unwrap();
    analysis.checkpoint.episodes = vec![analysis.revisions[0].episode.clone()];
    let update = DiagnosticUpdate {
        step_id: digest(b"forged complete current-work analysis"),
        analyzed_at: at.into(),
        revisions: analysis.revisions,
        checkpoint: analysis.checkpoint,
    };
    let error = runtime.fixture.outbox.commit_diagnostics(input, update).await.unwrap_err();
    assert_eq!(error.detail, Some(culprit_core::ErrorDetail::Outbox(OutboxProblem::Integrity)));
    assert_eq!(
        runtime.fixture.outbox.diagnostic_input_state(&capture, DIAGNOSTIC_CONTROLLER_VERSION).await.unwrap(),
        DiagnosticInputState::Pending
    );
    let policies = runtime.policies();
    assert_eq!(
        analyze_pending(&mut runtime.fixture.outbox, &policies, &CollectionCancellation::default(), 10, None)
            .await
            .unwrap(),
        1
    );
    let revisions = runtime.revisions(&capture.capture).await;
    assert_eq!(revisions[0].episode.analysis, DiagnosticAnalysisStatus::Partial);
    assert!(matches!(revisions[0].episode.report, Some(DiagnosticReport::CurrentRequestWork { .. })));
}
