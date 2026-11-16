//! Shared execution is separately qualified; its result still belongs to the original database incident.
use super::{
    runtime::Runtime,
    support::{Harness, capture_frame},
};
use crate::{
    capture::CollectionCancellation,
    diagnostics::{analyze_pending, drain_pending},
    fleet::{
        FleetJournal, FleetRunner,
        investigations::Investigations,
        journal::FocusedStage,
        scheduler::{Outcome, Scheduler},
    },
    outbox::{DiagnosticInputState, Outbox},
};
use culprit_core::telemetry::*;
use std::{path::Path, sync::atomic::Ordering, time::Duration};
use tokio::time::Instant;

async fn shared_runtime() -> Runtime {
    Runtime::from_fixture(Harness::shared().await).await
}

async fn only_intent(runtime: &Runtime) -> DiagnosticInvestigationIntent {
    let intents = runtime.fixture.journal.focused_intents().await.unwrap();
    assert_eq!(intents.len(), 1);
    intents.into_iter().next().unwrap()
}

async fn assert_not_enrolled(runtime: &mut Runtime) {
    let calls = runtime.native_calls();
    assert!(!runtime.enroll().await);
    assert!(runtime.fixture.journal.focused_intents().await.unwrap().is_empty());
    assert!(runtime.scheduler.focused_keys().is_empty());
    assert!(runtime.fixture.outbox.investigation_parent_keys().await.unwrap().is_empty());
    assert_eq!(runtime.native_calls(), calls, "a withheld question must not open, observe or close a native source");
}

#[tokio::test]
async fn shared_owner_executes_exact_question_and_updates_only_its_original_database_head() {
    let mut runtime = shared_runtime().await;
    assert_eq!(runtime.native_calls(), [1; 5], "only the independently selected shared baseline has run");
    assert!(runtime.enroll().await);
    let intent = only_intent(&runtime).await;
    let DiagnosticExecutionBinding::Shared { owner } = &intent.execution else { panic!() };
    let (owner_source, owner_row) = runtime.fixture.outbox.diagnostic_evidence(owner).await.unwrap();
    assert!(intent.matches_shared_owner(&owner_source, &owner_row));
    assert_eq!(intent.focus.participant().target, runtime.fixture.backend.activity.source.target);
    assert_ne!(owner_source.target, intent.focus.participant().target);
    assert_ne!(owner_source.partition.native_epoch(), intent.focus.participant().partition.native_epoch());
    assert_eq!(intent.qualification.scope.target(), owner_source.target);
    assert!(matches!(intent.selection(), CollectionSelection::SharedPageWaitFocus { .. }));
    let original = runtime.fixture.outbox.diagnostic_evidence(&intent.focus.participant().parent).await.unwrap();
    let owner_before = (owner_source, owner_row);

    let job = runtime.take_focus().await;
    assert_eq!(job.subject, intent.subject());
    assert_eq!(job.budget, intent.budget, "the selected route cannot replace the tested budget");
    let result = runtime.execute(&job).await;
    assert_eq!(runtime.native_calls(), [2; 5]);
    assert_eq!(result.event.status, CollectionStatus::Complete);
    assert!(intent.matches_result_source(result.source.as_ref().unwrap()));
    let Outcome::FocusedComplete { result: capture } = &result.outcome else { panic!() };
    let capture = capture.clone();
    let manifest = runtime.fixture.outbox.verify(&capture.capture.observation_id).await.unwrap();
    assert_eq!(manifest.investigation_intent().unwrap(), Some(intent.clone()));
    let input = runtime
        .fixture
        .outbox
        .diagnostic_input(&capture.capture.observation_id, DIAGNOSTIC_CONTROLLER_VERSION)
        .await
        .unwrap();
    assert_eq!(input.controller_target().unwrap(), &intent.focus.participant().target);
    assert_eq!(input.source.target, intent.qualification.scope.target());
    assert!(input.rows.is_empty(), "a disappeared participant supplies no borrowed SQL, plan or page participant");
    runtime.finish(result.outcome).await;

    let policies = runtime.policies();
    assert_eq!(
        analyze_pending(&mut runtime.fixture.outbox, &policies, &CollectionCancellation::default(), 10, None)
            .await
            .unwrap(),
        1
    );
    let revisions = runtime.revisions(&capture.capture).await;
    assert_eq!(revisions.len(), 1);
    let episode = &revisions[0].episode;
    assert_eq!(episode.id, intent.episode_id);
    assert_eq!(episode.scope.target, intent.focus.participant().target);
    assert_eq!(episode.scope.partition, intent.focus.participant().partition);
    assert_eq!(episode.latest, intent.focus.participant().parent);
    assert_eq!(episode.issue, Some(DiagnosticAnalysisIssue::NativeContextGone));
    assert!(episode.report.is_none());
    let checkpoints =
        runtime.fixture.outbox.diagnostic_checkpoints(DIAGNOSTIC_CONTROLLER_VERSION, None, 10).await.unwrap();
    assert_eq!(checkpoints.checkpoints.len(), 1, "shared execution must not create a different controller head");
    assert_eq!(checkpoints.checkpoints[0].target, intent.focus.participant().target);
    assert_eq!(runtime.fixture.outbox.diagnostic_evidence(&intent.focus.participant().parent).await.unwrap(), original);
    assert_eq!(runtime.fixture.outbox.diagnostic_evidence(owner).await.unwrap(), owner_before);
    assert_eq!(
        runtime.fixture.outbox.diagnostic_input_state(&capture, DIAGNOSTIC_CONTROLLER_VERSION).await.unwrap(),
        DiagnosticInputState::Consumed
    );
    assert_eq!(runtime.native_calls(), [2; 5], "offline analysis cannot reopen either native resource");
}

#[tokio::test]
async fn database_outside_scope_result_can_progress_to_a_distinct_explicit_shared_resource_stage() {
    let mut runtime = Runtime::from_fixture(Harness::progressive().await).await;
    runtime.started -= runtime.scheduler.wait_allowed(runtime.started.elapsed(), |_| false);
    runtime.baseline().await;
    assert_eq!(runtime.native_calls(), [2; 5], "both configured periodic routes establish their own live binding");
    assert!(runtime.enroll().await);
    let database_intent = only_intent(&runtime).await;
    assert_eq!(database_intent.execution, DiagnosticExecutionBinding::Database);
    runtime.fixture.backend.focus_request = PageWaitRequestState::SameStatement;
    runtime.fixture.backend.focus_resolution = Some(PageWaitResolution::OutsideDatabase);
    let database_job = runtime.take_focus().await;
    let result = runtime.execute(&database_job).await;
    let Outcome::FocusedComplete { result: database_result } = &result.outcome else { panic!() };
    let database_result = database_result.clone();
    runtime.finish(result.outcome).await;
    let policies = runtime.policies();
    drain_pending(&mut runtime.fixture.outbox, &policies, &CollectionCancellation::default(), None).await.unwrap();
    let revisions = runtime.revisions(&database_result.capture).await;
    assert_eq!(revisions.len(), 1);
    let partial = &revisions[0].episode;
    assert_eq!(partial.id, database_intent.episode_id);
    assert_eq!(partial.analysis, DiagnosticAnalysisStatus::Partial);
    assert_eq!(partial.issue, Some(DiagnosticAnalysisIssue::Unsupported));
    assert!(partial.report.is_some(), "the outside-database native point remains evidence");
    runtime.reconcile().await;
    assert!(runtime.fixture.journal.focused_intents().await.unwrap().is_empty());

    assert!(
        runtime.enroll().await,
        "a completed database stage must not suppress the separately qualified shared scope"
    );
    let shared_intent = only_intent(&runtime).await;
    assert!(matches!(shared_intent.execution, DiagnosticExecutionBinding::Shared { .. }));
    assert_eq!(shared_intent.episode_id, database_intent.episode_id);
    assert_eq!(shared_intent.focus, database_intent.focus);
    assert_ne!(shared_intent.episode_stage_key().unwrap(), database_intent.episode_stage_key().unwrap());
    assert_ne!(shared_intent.key().unwrap(), database_intent.key().unwrap());
    runtime.fixture.backend.focus_request = PageWaitRequestState::Missing;
    runtime.fixture.backend.focus_resolution = None;
    let shared_job = runtime.take_focus().await;
    assert!(matches!(shared_job.subject, CollectionSubject::Shared { .. }));
    let result = runtime.execute(&shared_job).await;
    let Outcome::FocusedComplete { result: shared_result } = &result.outcome else { panic!() };
    let shared_result = shared_result.clone();
    runtime.finish(result.outcome).await;
    drain_pending(&mut runtime.fixture.outbox, &policies, &CollectionCancellation::default(), None).await.unwrap();
    let revisions = runtime.revisions(&shared_result.capture).await;
    assert_eq!(revisions.len(), 1);
    assert_eq!(revisions[0].episode.id, partial.id);
    assert_eq!(revisions[0].episode.scope, partial.scope);
    assert_eq!(revisions[0].episode.latest, database_intent.focus.participant().parent);
    assert_eq!(runtime.native_calls(), [4; 5]);
}

#[tokio::test]
async fn unknown_or_empty_shared_owner_never_supplies_fresh_native_authority() {
    for unknown in [true, false] {
        let mut fixture = Harness::shared().await;
        let shared = fixture.backend.shared.as_mut().unwrap();
        let SourceObservation::Snapshot(snapshot) = &mut shared.source.observation else { panic!() };
        if unknown {
            shared.source.partition = SourcePartition::InstanceLocal { member: None, epoch: None };
            snapshot.sources[0] = StateSourceCoverage::observed(shared.source.source.clone(), 1, false);
        } else {
            shared.records.clear();
            snapshot.records = 0;
            snapshot.sources[0] = StateSourceCoverage::collected(shared.source.source.clone(), 0);
        }
        let mut runtime = Runtime::from_fixture(fixture).await;
        assert_eq!(runtime.native_calls(), [1; 5]);
        assert_not_enrolled(&mut runtime).await;
        assert_eq!(runtime.fixture.backend.verified.load(Ordering::SeqCst), 0);
    }
}

#[tokio::test]
async fn different_shared_instance_epoch_source_or_old_observation_cannot_borrow_the_database_incident() {
    enum Mismatch {
        Instance,
        Epoch,
        Source,
        OldObservation,
    }
    for mismatch in [Mismatch::Instance, Mismatch::Epoch, Mismatch::Source, Mismatch::OldObservation] {
        let mut fixture = Harness::shared().await;
        let shared = fixture.backend.shared.as_mut().unwrap();
        match mismatch {
            Mismatch::Instance => {
                let CollectionTarget::Shared(member) = &mut shared.source.target else { panic!() };
                member.native.id = "another-native-instance".into();
                let SourcePartition::InstanceLocal { member: selected, .. } = &mut shared.source.partition else {
                    panic!()
                };
                *selected = Some(member.clone());
                fixture.inventory.targets[0].subject = CollectionSubject::Shared { resource: member.clone() };
            }
            Mismatch::Epoch => {
                let SourcePartition::InstanceLocal { epoch, .. } = &mut shared.source.partition else { panic!() };
                *epoch = Some("another-native-instance-generation".into());
            }
            Mismatch::Source => {
                shared.source.source = "another-native-owner-source".into();
                let SourceObservation::Snapshot(snapshot) = &mut shared.source.observation else { panic!() };
                snapshot.sources[0].source = shared.source.source.clone();
                let TelemetryRecord::Operational(row) = &mut shared.records[0] else { panic!() };
                row.source = shared.source.source.clone();
            }
            Mismatch::OldObservation => {
                let SourceObservation::Snapshot(snapshot) = &mut shared.source.observation else { panic!() };
                snapshot.observed_from = "2019-12-31T23:59:59Z".into();
                snapshot.observed_until = "2020-01-01T12:00:00Z".into();
            }
        }
        let mut runtime = Runtime::from_fixture(fixture).await;
        assert_not_enrolled(&mut runtime).await;
        assert_eq!(runtime.fixture.backend.verified.load(Ordering::SeqCst), 0);
    }
}

#[tokio::test]
async fn shared_cost_refusal_and_changed_saved_alias_stop_before_focused_native_open() {
    for changed_alias in [false, true] {
        let mut runtime = shared_runtime().await;
        if changed_alias {
            runtime.fixture.backend.changed.store(true, Ordering::SeqCst);
        } else {
            runtime.fixture.backend.accept = false;
        }
        assert_not_enrolled(&mut runtime).await;
        assert_eq!(runtime.fixture.backend.verified.load(Ordering::SeqCst), usize::from(!changed_alias));
    }
}

#[tokio::test]
async fn shared_receipt_cannot_exceed_the_selected_baseline_budget() {
    let mut fixture = Harness::shared().await;
    let RecipeCostQualification::Measured(receipt) = &fixture.backend.recipe.cost else { panic!() };
    let stricter = receipt.tested_budget.max_queries - 1;
    fixture.inventory.targets[0].schedules[0].budget.max_queries = stricter;
    // The two-query baseline fits; the three-query focused receipt does not.
    let mut runtime = Runtime::from_fixture(fixture).await;
    assert_not_enrolled(&mut runtime).await;
    assert_eq!(runtime.fixture.backend.verified.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn recovered_database_symptom_still_owes_shared_question_without_reassigning_the_original() {
    let mut runtime = shared_runtime().await;
    let mut recovered = runtime.fixture.backend.activity.clone();
    let SourceObservation::Activity(frame) = &mut recovered.source.observation else { panic!() };
    frame.observed_from = "2020-01-01T12:00:00.100Z".into();
    frame.at = "2020-01-01T12:00:00.200Z".into();
    frame.observed_until = "2020-01-01T12:00:00.300Z".into();
    frame.native_rows = 0;
    frame.observations = 0;
    frame.coverage = StateSourceCoverage::collected(recovered.source.source.clone(), 0);
    recovered.records.clear();
    capture_frame(&mut runtime.fixture.outbox, recovered).await;
    let policies = runtime.policies();
    drain_pending(&mut runtime.fixture.outbox, &policies, &CollectionCancellation::default(), None).await.unwrap();
    assert!(runtime.enroll().await);
    let intent = only_intent(&runtime).await;
    let input = runtime
        .fixture
        .outbox
        .diagnostic_input(&intent.focus.participant().parent.capture.observation_id, DIAGNOSTIC_CONTROLLER_VERSION)
        .await
        .unwrap();
    let before =
        input.checkpoint.unwrap().episodes.into_iter().find(|episode| episode.id == intent.episode_id).unwrap();
    assert_eq!(before.lifecycle, DiagnosticLifecycle::Resolved);
    assert!(before.analysis.needs_work());
    assert!(matches!(before.recovery, Some(DiagnosticRecoveryProof::CompleteScope { .. })));
    let original = runtime.fixture.outbox.diagnostic_evidence(&intent.focus.participant().parent).await.unwrap();
    runtime.fixture.backend.focus_request = PageWaitRequestState::Changed;
    let job = runtime.take_focus().await;
    let result = runtime.execute(&job).await;
    let Outcome::FocusedComplete { result: capture } = &result.outcome else { panic!() };
    let capture = capture.clone();
    runtime.finish(result.outcome).await;
    drain_pending(&mut runtime.fixture.outbox, &policies, &CollectionCancellation::default(), None).await.unwrap();
    let revisions = runtime.revisions(&capture.capture).await;
    assert_eq!(revisions.len(), 1);
    let after = &revisions[0].episode;
    assert_eq!(after.id, before.id);
    assert_eq!(after.scope, before.scope);
    assert_eq!(after.latest, intent.focus.participant().parent);
    assert_eq!(after.lifecycle, DiagnosticLifecycle::Resolved);
    assert!(after.report.is_none(), "a reused request cannot borrow the original SQL or page participant");
    assert_eq!(runtime.fixture.outbox.diagnostic_evidence(&intent.focus.participant().parent).await.unwrap(), original);
    assert_eq!(runtime.native_calls(), [2; 5]);
}

#[tokio::test]
async fn restored_shared_queue_waits_for_fresh_positive_owner_without_consuming_a_native_reservation() {
    let mut runtime = shared_runtime().await;
    assert!(runtime.enroll().await);
    let intent = only_intent(&runtime).await;
    let key = intent.key().unwrap();
    let saved = runtime.fixture.journal.scheduler().await.unwrap().unwrap();
    let mut restored = Scheduler::new(&runtime.fixture.inventory, "one").unwrap();
    restored.restore(saved, Duration::ZERO, chrono::Utc::now()).unwrap();
    runtime.scheduler = restored;
    runtime.investigations = Investigations::new(&runtime.fixture.inventory, "one");
    runtime.started = Instant::now();
    let ready = Duration::from_secs(61);
    let state = runtime.scheduler.focused_state(&key).unwrap();
    assert!(!runtime.investigations.route_available(&state.job));
    assert!(
        runtime.scheduler.take_allowed(ready, |job| runtime.investigations.route_available(job)).unwrap().is_none()
    );
    assert!(runtime.scheduler.wait_allowed(ready, |job| runtime.investigations.route_available(job)) > Duration::ZERO);
    assert_eq!(runtime.fixture.journal.scheduler().await.unwrap().unwrap().focused[&key].attempts, 0);
    assert_eq!(runtime.native_calls(), [1; 5]);

    runtime.started -= runtime.scheduler.wait_allowed(runtime.started.elapsed(), |_| false);
    runtime.baseline().await;
    assert_eq!(runtime.native_calls(), [2; 5], "the same periodic lane establishes the new activation binding");
    assert_eq!(only_intent(&runtime).await, intent, "fresh owner observation cannot rewrite queued provenance");
    let job = runtime.take_focus().await;
    assert_eq!(job.key, key);
    assert_eq!(job.budget, intent.budget);
    let result = runtime.execute(&job).await;
    assert!(matches!(result.outcome, Outcome::FocusedComplete { .. }));
    runtime.finish(result.outcome).await;
    assert_eq!(runtime.native_calls(), [3; 5]);
}

#[tokio::test]
async fn a_shared_only_runner_retires_consumed_completed_work_after_its_first_fresh_baseline() {
    let mut runtime = shared_runtime().await;
    assert!(runtime.enroll().await);
    let intent = only_intent(&runtime).await;
    runtime.fixture.backend.focus_request = PageWaitRequestState::SameStatement;
    runtime.fixture.backend.focus_resolution = Some(PageWaitResolution::Unavailable);
    let job = runtime.take_focus().await;
    let result = runtime.execute(&job).await;
    let Outcome::FocusedComplete { result: capture } = &result.outcome else { panic!() };
    let capture = capture.clone();
    runtime.finish(result.outcome).await;
    let policies = runtime.policies();
    drain_pending(&mut runtime.fixture.outbox, &policies, &CollectionCancellation::default(), None).await.unwrap();
    assert_eq!(
        runtime.fixture.outbox.diagnostic_input_state(&capture, DIAGNOSTIC_CONTROLLER_VERSION).await.unwrap(),
        DiagnosticInputState::Consumed
    );
    let saved = runtime.fixture.journal.scheduler().await.unwrap().unwrap();
    assert!(saved.clock.pending_job.is_none());
    assert_eq!(saved.focused[&job.key].source_work, SourceWorkState::Quiescent);
    assert!(matches!(saved.focused[&job.key].status, crate::fleet::scheduler::FocusStatus::Completed { .. }));
    assert_eq!(runtime.fixture.outbox.investigation_parent_keys().await.unwrap(), vec![job.key.clone()]);
    let calls = runtime.native_calls();
    assert_eq!(calls, [2; 5]);

    // Simulate a crash after consumption and before the owning run_ready turn can reconcile its completed work.
    let Harness { _directory: directory, inventory, outbox, journal, backend, .. } = runtime.fixture;
    drop(outbox);
    drop(journal);
    elapse_quiet_scheduler_downtime(&directory.path().join("fleet/schedule.sqlite3"));
    let mut outbox = Outbox::open(&directory.path().join("outbox")).unwrap();
    let recovered = outbox.recover().await.unwrap();
    assert_eq!((recovered.interrupted, recovered.quarantined, recovered.source_debts), (0, 0, 0));
    let journal = FleetJournal::open(&directory.path().join("fleet"), &inventory, "one").await.unwrap();
    let cancellation = CollectionCancellation::default();
    let mut runner = FleetRunner::new(&backend, &inventory, "one", &journal, &mut outbox).unwrap();
    runner.initialize(&cancellation).await.unwrap();
    let restored = runner.scheduler.focused_state(&job.key).unwrap();
    assert!(!runner.investigations.route_available(&restored.job), "cold queue metadata supplies no native binding");
    assert_eq!(journal.focused_intents().await.unwrap(), vec![intent.clone()]);
    assert_eq!(runner.scheduler.wait_allowed(runner.started.elapsed(), |_| false), Duration::ZERO);

    let summary = runner.run(&cancellation, Some(1), |_| Ok(())).await.unwrap();
    assert_eq!((summary.attempts, summary.complete, summary.incomplete), (1, 1, 0));
    assert!(runner.scheduler.focused_keys().is_empty(), "the actual shared baseline turn must run maintenance");
    drop(runner);
    assert!(journal.focused_intents().await.unwrap().is_empty());
    assert!(outbox.investigation_parent_keys().await.unwrap().is_empty());
    assert!(
        matches!(
            journal.focused_stage(&intent.episode_stage_key().unwrap()).await.unwrap(),
            Some(FocusedStage::Completed)
        ),
        "the still-active partial incident retains its exact shared-stage replay receipt"
    );
    assert_eq!(journal.focused_receipt_episode_ids().await.unwrap(), [intent.episode_id].into());
    let native = &backend.calls;
    assert_eq!(
        [&native.captured, &native.opened, &native.observed, &native.verified, &native.closed]
            .map(|count| count.load(Ordering::SeqCst)),
        calls.map(|count| count + 1),
        "only the regular fresh shared baseline may open a source after restart"
    );
}

fn elapse_quiet_scheduler_downtime(path: &Path) {
    // Age only scheduler anchors, exactly as elapsed known-quiet wall time would. Native facts, references,
    // source clocks, retained counters, remaining delays and source-work obligations remain byte-exact.
    let mut connection = rusqlite::Connection::open(path).unwrap();
    let transaction = connection.transaction().unwrap();
    let age = chrono::Duration::days(2);
    for (table, pointer) in [
        ("admission_clock", "/wall"),
        ("admission_jobs", "/delay/observed_at"),
        ("admission_resources", "/delay/observed_at"),
        ("admission_families", "/observed_at"),
        ("admission_focused", "/delay/observed_at"),
        ("admission_focus_families", "/delay/observed_at"),
    ] {
        let rows = transaction
            .prepare(&format!("SELECT CAST(id AS TEXT),state_json FROM {table}"))
            .unwrap()
            .query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        for (key, json) in rows {
            let mut value: serde_json::Value = serde_json::from_str(&json).unwrap();
            let field = value.pointer_mut(pointer).unwrap();
            let at = chrono::DateTime::parse_from_rfc3339(field.as_str().unwrap()).unwrap();
            *field = serde_json::json!((at - age).to_rfc3339());
            transaction
                .execute(
                    &format!("UPDATE {table} SET state_json=?1 WHERE id=?2"),
                    rusqlite::params![serde_json::to_string(&value).unwrap(), key],
                )
                .unwrap();
        }
    }
    transaction.commit().unwrap();
}
