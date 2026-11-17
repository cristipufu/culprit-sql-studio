use super::super::tests::support::*;
use super::*;
use crate::fleet::scheduler::{FocusEnrollment, Scheduler};
use culprit_core::ErrorDetail;
use std::sync::atomic::Ordering;
mod optional_evidence;
mod task_holders;
mod writers;

#[tokio::test]
async fn sealed_original_and_cost_evidence_prepare_one_verified_candidate_without_native_work() {
    let mut harness = Harness::new(1).await;
    let candidates = harness.prepare(&CollectionCancellation::default()).await.unwrap();
    assert_eq!(candidates.len(), 1);
    let candidate = &candidates[0];
    assert!(candidate.intent.is_valid());
    assert!(candidate.job.matches_intent(&candidate.intent));
    assert_eq!(candidate.job.cooldown_seconds, 60);
    assert_eq!(candidate.route_revision, harness.backend.revision);
    assert_eq!(harness.backend.verified.load(Ordering::SeqCst), 1);
    assert_eq!(harness.backend.calls.captured.load(Ordering::SeqCst), 0);
    assert_eq!(harness.backend.calls.opened.load(Ordering::SeqCst), 0);
    assert!(harness.journal.focused_intents().await.unwrap().is_empty(), "preparation never enrolls work");
}

#[tokio::test]
async fn a_native_lock_wait_without_duration_selects_its_question_but_unqualified_cost_cannot_open_or_enroll_it() {
    let fixture = crate::analytical::test_support::investigation_fixture();
    let mut record = fixture.baseline_record.record;
    let TelemetryRecord::Activity(row) = &mut record else { panic!("original native Activity point") };
    row.waiting_on_lock = Some(true);
    row.blocking_status = ActivityBlockingStatus::Unknown;
    row.elapsed_ms = Some("40000".into());
    let wait = row.wait.as_mut().unwrap();
    wait.class = None;
    wait.identified_resources.clear();
    wait.native_type = Some("native-lock-wait".into());
    wait.elapsed_ms = None;
    wait.owners.clear();
    let mut harness = Harness::with_activity(Frame { source: fixture.baseline_source, records: vec![record] }).await;
    harness.backend.recipe.id = "fixture.focused-blocking".into();
    harness.backend.recipe.selector = RecipeSelector::FocusedBlocking;
    harness.backend.recipe.cost = RecipeCostQualification::Unqualified;
    let page = harness.outbox.diagnostic_checkpoints(DIAGNOSTIC_CONTROLLER_VERSION, None, 10).await.unwrap();
    let episodes = &page.checkpoints[0].episodes;
    let [episode] = episodes.as_slice() else { panic!("one native lock question, without invented timed symptoms") };
    assert!(matches!(episode.signal, DiagnosticSignal::LockWait { .. }));
    assert_eq!(possible_questions(episode), [RecipeSelector::FocusedBlocking]);
    assert!(harness.prepare(&CollectionCancellation::default()).await.unwrap().is_empty());
    assert_eq!(harness.backend.components.load(Ordering::SeqCst), 0);
    assert_eq!(harness.backend.verified.load(Ordering::SeqCst), 0);
    assert_eq!(harness.backend.calls.opened.load(Ordering::SeqCst), 0);
    assert_eq!(harness.backend.calls.captured.load(Ordering::SeqCst), 0);
    assert!(harness.journal.focused_intents().await.unwrap().is_empty());
}

#[tokio::test]
async fn unqualified_unknown_activation_changed_routes_and_stricter_caller_caps_never_verify_cost() {
    for withheld in ["unqualified", "activation", "clock", "route", "budget"] {
        let mut harness = Harness::new(1).await;
        match withheld {
            "unqualified" => harness.backend.recipe.cost = RecipeCostQualification::Unqualified,
            "activation" => harness.bindings.invalidate("target"),
            "clock" => {
                let mut source = harness.bindings.get("target").unwrap().source.clone();
                let SourceObservation::Activity(frame) = &mut source.observation else { panic!() };
                frame.at = "2019-01-01T00:00:00Z".into();
                frame.observed_from = frame.at.clone();
                frame.observed_until = frame.at.clone();
                harness.bindings.observe("target", &source, &harness.backend.revision).unwrap();
            }
            "route" => harness.backend.changed.store(true, Ordering::SeqCst),
            _ => harness.inventory.targets[0].schedules[0].budget.max_queries = 1,
        }
        assert!(harness.prepare(&CollectionCancellation::default()).await.unwrap().is_empty(), "{withheld}");
        assert_eq!(harness.backend.verified.load(Ordering::SeqCst), 0, "{withheld}");
        assert_eq!(harness.backend.calls.opened.load(Ordering::SeqCst), 0, "{withheld}");
        if withheld == "unqualified" {
            assert_eq!(harness.backend.components.load(Ordering::SeqCst), 0);
        }
    }
}

#[tokio::test]
async fn existing_outstanding_stage_is_skipped_before_cost_artifact_verification() {
    let mut harness = Harness::new(1).await;
    let candidate = harness.prepare(&CollectionCancellation::default()).await.unwrap().pop().unwrap();
    let mut scheduler = Scheduler::new(&harness.inventory, "one").unwrap();
    let now = chrono::Utc::now();
    let state = scheduler.initial_state(Duration::ZERO, now).unwrap();
    scheduler.persisted(harness.journal.initialize_scheduler(state).await.unwrap()).unwrap();
    let FocusEnrollment::Enqueued(update) = scheduler.enqueue_focus(candidate.job, Duration::ZERO, now).unwrap() else {
        panic!()
    };
    harness.journal.enqueue_focused(candidate.intent, *update).await.unwrap();
    assert!(harness.prepare(&CollectionCancellation::default()).await.unwrap().is_empty());
    assert_eq!(harness.backend.verified.load(Ordering::SeqCst), 1, "only the initial preparation verified cost");
}

#[tokio::test]
async fn verifier_refusal_is_withheld_but_integrity_errors_propagate() {
    let mut harness = Harness::new(1).await;
    harness.backend.accept = false;
    assert!(harness.prepare(&CollectionCancellation::default()).await.unwrap().is_empty());
    harness.backend.fail = true;
    let error = harness.prepare(&CollectionCancellation::default()).await.err().unwrap();
    assert_eq!(error.detail, Some(ErrorDetail::Delivery(DeliveryProblem::Integrity)));
}

#[tokio::test]
async fn cancellation_discards_prepared_candidates_without_enrollment_or_native_work() {
    let mut harness = Harness::new(1).await;
    let cancellation = CollectionCancellation::default();
    harness.backend.cancel = Some(cancellation.clone());
    assert!(harness.prepare(&cancellation).await.unwrap().is_empty());
    assert!(harness.journal.focused_intents().await.unwrap().is_empty());
    assert_eq!(harness.backend.verified.load(Ordering::SeqCst), 1);
    assert!(harness.prepare(&cancellation).await.unwrap().is_empty());
    assert_eq!(harness.backend.verified.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn bounded_scans_resume_after_a_withheld_prefix_instead_of_starving_later_episodes() {
    let mut harness = Harness::new(MAX_QUESTIONS_PER_PASS + 2).await;
    harness.backend.recipe.cost = RecipeCostQualification::Unqualified;
    assert!(harness.prepare(&CollectionCancellation::default()).await.unwrap().is_empty());
    assert_eq!(harness.bindings.planning_position().1, MAX_QUESTIONS_PER_PASS);
    assert!(harness.prepare(&CollectionCancellation::default()).await.unwrap().is_empty());
    assert_eq!(harness.bindings.planning_position(), (None, 0));
    assert_eq!(harness.backend.verified.load(Ordering::SeqCst), 0);
}
