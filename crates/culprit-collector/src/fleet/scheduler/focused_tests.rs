use super::tests::{inventory, seconds};
use super::*;
use chrono::{DateTime, TimeDelta, Utc};

fn wall(seconds: i64) -> DateTime<Utc> {
    "2026-11-14T12:00:00Z".parse::<DateTime<Utc>>().unwrap() + TimeDelta::seconds(seconds)
}
fn single_target(period: u32) -> FleetInventory {
    let mut inventory = inventory();
    inventory.targets.truncate(1);
    inventory.targets[0].schedules[0].every_seconds = period;
    inventory
}
fn initialize(inventory: &FleetInventory) -> (Scheduler, ScheduleState) {
    let mut scheduler = Scheduler::new(inventory, "one").unwrap();
    let state = scheduler.initial_state(Duration::ZERO, wall(0)).unwrap();
    scheduler.persisted(state.clock.clone()).unwrap();
    (scheduler, state)
}
// In-memory transport exercises the actual checkpoint transitions; journal tests cover their atomic SQLite application.
fn persist(scheduler: &mut Scheduler, state: &mut ScheduleState, update: ScheduleUpdate) {
    if let Some((key, value)) = update.job {
        state.jobs.insert(key, value);
    }
    state.resources.insert(update.resource.0, update.resource.1);
    if let Some((key, value)) = update.family {
        state.families.insert(key, value);
    }
    match update.focused {
        Some(FocusMutation::Insert { key, state: value } | FocusMutation::Update { key, state: value }) => {
            state.focused.insert(key, value);
        }
        Some(FocusMutation::Remove { key }) => {
            state.focused.remove(&key);
        }
        None => {}
    }
    match update.focused_family {
        Some(FocusFamilyMutation::Upsert { key, state: value }) => {
            state.focused_families.insert(key, value);
        }
        Some(FocusFamilyMutation::Remove { key }) => {
            state.focused_families.remove(&key);
        }
        None => {}
    }
    scheduler.persisted(update.clock.clone()).unwrap();
    state.clock = update.clock;
}
fn reserve(scheduler: &mut Scheduler, state: &mut ScheduleState, at: u64) -> Work {
    let work = scheduler.take(seconds(at)).unwrap().unwrap();
    let update = scheduler.admission_checkpoint(seconds(at), wall(at as i64)).unwrap();
    persist(scheduler, state, update);
    work
}
fn finish(scheduler: &mut Scheduler, state: &mut ScheduleState, at: u64, outcome: Outcome) {
    scheduler.finish(seconds(at), outcome).unwrap();
    let update = scheduler.completion_checkpoint(seconds(at), wall(at as i64)).unwrap();
    persist(scheduler, state, update);
}
fn baseline(scheduler: &mut Scheduler, state: &mut ScheduleState, at: u64) {
    assert!(matches!(reserve(scheduler, state, at), Work::Scheduled(_)));
    finish(scheduler, state, at, Outcome::Complete);
}
fn job(inventory: &FleetInventory, seed: &str, recipe: &str) -> FocusedJob {
    let target = &inventory.targets[0];
    let value = FocusedJob {
        key: digest(format!("intent:{seed}").as_bytes()),
        episode_id: digest(format!("episode:{seed}").as_bytes()),
        episode_stage_key: digest(format!("stage:{seed}").as_bytes()),
        recipe_id: recipe.into(),
        recipe_version: 1,
        target: target.id.clone(),
        connection: target.connection.clone(),
        subject: target.subject.clone(),
        resource: target.admission_group.clone(),
        family: focus_family_key(&target.id, recipe).unwrap(),
        budget: CollectionBudget {
            total_timeout_ms: 5_000,
            statement_timeout_ms: 1_000,
            ..CollectionBudget::default()
        },
        cooldown_seconds: 60,
    };
    assert!(value.is_valid());
    value
}
fn enqueue(scheduler: &mut Scheduler, state: &mut ScheduleState, job: FocusedJob, at: u64) {
    let FocusEnrollment::Enqueued(update) = scheduler.enqueue_focus(job, seconds(at), wall(at as i64)).unwrap() else {
        panic!("expected a new focused job")
    };
    persist(scheduler, state, *update);
}
fn result(seed: &str) -> DiagnosticCaptureRef {
    let value = DiagnosticCaptureRef {
        capture: CaptureIdentity {
            observation_id: seed.into(),
            revision: Some(CaptureRevision {
                outbox_id: "fixture-outbox".into(),
                owner_epoch: "1".into(),
                sequence: "2".into(),
            }),
        },
        manifest_sha256: digest(format!("manifest:{seed}").as_bytes()),
        source_scope_id: digest(b"fixture-scope"),
    };
    assert!(value.is_valid());
    value
}

#[test]
fn sixty_second_cadence_has_no_focus_slot_but_one_hundred_eighty_seconds_has_one() {
    for period in [60, 180] {
        let inventory = single_target(period);
        let (mut scheduler, mut state) = initialize(&inventory);
        baseline(&mut scheduler, &mut state, 0);
        let focus = job(&inventory, "one", "page-target");
        enqueue(&mut scheduler, &mut state, focus.clone(), 0);
        assert_eq!(scheduler.wait(Duration::ZERO), seconds(60));
        match reserve(&mut scheduler, &mut state, 60) {
            Work::Scheduled(_) => assert_eq!(period, 60),
            Work::Focused(actual) => {
                assert_eq!(period, 180);
                assert_eq!(actual, focus);
            }
        }
    }
}

#[test]
fn unavailable_focus_route_waits_for_a_periodic_turn_without_reserving_native_capacity_or_spinning() {
    let inventory = single_target(180);
    let (mut scheduler, mut state) = initialize(&inventory);
    baseline(&mut scheduler, &mut state, 0);
    let focus = job(&inventory, "no-current-native-route", "page-target");
    enqueue(&mut scheduler, &mut state, focus.clone(), 0);
    let sequence = scheduler.sequence;
    let resource = scheduler.resources["shared"].clone();
    assert!(scheduler.take_allowed(seconds(60), |_| false).unwrap().is_none());
    assert_eq!(scheduler.wait_allowed(seconds(60), |_| false), seconds(120));
    assert_eq!(scheduler.sequence, sequence);
    assert_eq!(state.focused[&focus.key].attempts, 0);
    let after = &scheduler.resources["shared"];
    assert_eq!(
        (after.due, after.attempts, after.pressure_failures, after.max_reservation),
        (resource.due, resource.attempts, resource.pressure_failures, resource.max_reservation)
    );
    assert_eq!(scheduler.wait_allowed(seconds(60), |_| true), Duration::ZERO);
    assert!(
        scheduler.take_allowed(seconds(109), |_| true).unwrap().is_none(),
        "a fresh route cannot skip the remaining fit check"
    );
    let Work::Scheduled(_) = scheduler.take_allowed(seconds(180), |_| false).unwrap().unwrap() else {
        panic!("the periodic source remains runnable without a focused route")
    };
    let update = scheduler.admission_checkpoint(seconds(180), wall(180)).unwrap();
    persist(&mut scheduler, &mut state, update);
    finish(&mut scheduler, &mut state, 180, Outcome::Complete);
    assert!(
        matches!(scheduler.take_allowed(seconds(240), |candidate| candidate.key == focus.key).unwrap(), Some(Work::Focused(actual)) if actual == focus)
    );
}

#[test]
fn fit_includes_start_native_timeout_shutdown_and_same_resource_gap_at_exact_boundary() {
    for period in [131, 132] {
        let inventory = single_target(period);
        let (mut scheduler, mut state) = initialize(&inventory);
        baseline(&mut scheduler, &mut state, 0);
        enqueue(&mut scheduler, &mut state, job(&inventory, "boundary", "page-target"), 0);
        if period == 132 {
            assert!(matches!(reserve(&mut scheduler, &mut state, 60), Work::Focused(_)));
        } else {
            assert!(scheduler.take(seconds(60)).unwrap().is_none());
            assert_eq!(scheduler.wait(seconds(60)), seconds(71));
        }
    }
}

#[test]
fn every_periodic_collection_and_history_lane_has_precedence_over_a_focus() {
    let mut inventory = single_target(60);
    inventory.targets[0].schedules[0].collection =
        ScheduledCollection::QueryHistory { lookback_seconds: 3_600, reconcile_seconds: 600 };
    let (mut scheduler, mut state) = initialize(&inventory);
    enqueue(&mut scheduler, &mut state, job(&inventory, "history", "page-target"), 0);
    baseline(&mut scheduler, &mut state, 0);
    let Work::Scheduled(backfill) = reserve(&mut scheduler, &mut state, 60) else {
        panic!("backfill must remain protected")
    };
    assert!(backfill.lane == HistoryLane::Backfill);
}

#[test]
fn independent_resource_needs_global_serial_fit_but_not_another_resources_post_gap() {
    for other_period in [71, 80] {
        let mut inventory = single_target(180);
        let mut other = inventory.targets[0].clone();
        other.id = "independent".into();
        other.connection = "independent".into();
        other.admission_group = "other".into();
        other.schedules[0].every_seconds = other_period;
        inventory.targets.push(other);
        let (mut scheduler, mut state) = initialize(&inventory);
        baseline(&mut scheduler, &mut state, 0);
        baseline(&mut scheduler, &mut state, 0);
        enqueue(&mut scheduler, &mut state, job(&inventory, "serial", "page-target"), 0);
        if other_period == 80 {
            assert!(matches!(reserve(&mut scheduler, &mut state, 60), Work::Focused(_)));
        } else {
            assert!(scheduler.take(seconds(60)).unwrap().is_none());
        }
    }
}

#[test]
fn focused_jobs_share_static_resource_fairness_and_do_not_accelerate_each_other() {
    let inventory = single_target(3_600);
    let (mut scheduler, mut state) = initialize(&inventory);
    baseline(&mut scheduler, &mut state, 0);
    let first = job(&inventory, "first", "page-target");
    let second = job(&inventory, "second", "another-recipe");
    enqueue(&mut scheduler, &mut state, first.clone(), 0);
    enqueue(&mut scheduler, &mut state, second.clone(), 0);
    assert!(matches!(reserve(&mut scheduler, &mut state, 60), Work::Focused(actual) if actual == first));
    finish(
        &mut scheduler,
        &mut state,
        60,
        Outcome::FocusedRetry {
            reason: FocusRetry::More,
            attempt: Some(result("first-attempt").capture),
            source_work: SourceWorkState::Quiescent,
        },
    );
    assert!(scheduler.take(seconds(119)).unwrap().is_none());
    assert!(matches!(reserve(&mut scheduler, &mut state, 120), Work::Focused(actual) if actual == second));
    finish(&mut scheduler, &mut state, 120, Outcome::FocusedComplete { result: result("second-result") });
    assert!(matches!(reserve(&mut scheduler, &mut state, 180), Work::Focused(actual) if actual == first));
}

#[test]
fn focused_pressure_backoff_holds_the_whole_resource_and_survives_cancellation() {
    let inventory = single_target(3_600);
    let (mut scheduler, mut state) = initialize(&inventory);
    baseline(&mut scheduler, &mut state, 0);
    let first = job(&inventory, "pressure", "page-target");
    let second = job(&inventory, "waiting", "second-recipe");
    let queued = job(&inventory, "never-started", "third-recipe");
    enqueue(&mut scheduler, &mut state, first.clone(), 0);
    enqueue(&mut scheduler, &mut state, second.clone(), 0);
    enqueue(&mut scheduler, &mut state, queued.clone(), 0);
    for at in [60, 120] {
        assert!(matches!(reserve(&mut scheduler, &mut state, at), Work::Focused(_)));
        finish(
            &mut scheduler,
            &mut state,
            at,
            Outcome::FocusedRetry {
                reason: FocusRetry::Pressure,
                attempt: None,
                source_work: SourceWorkState::Unconfirmed,
            },
        );
    }
    assert!(scheduler.cancel_focused(&second.key, seconds(120), wall(120)).is_err());
    let update = scheduler.cancel_focused(&queued.key, seconds(120), wall(120)).unwrap();
    persist(&mut scheduler, &mut state, update);
    assert_eq!(scheduler.resources["shared"].pressure_failures, 2);
    assert!(scheduler.take(seconds(239)).unwrap().is_none());
    assert!(matches!(reserve(&mut scheduler, &mut state, 240), Work::Focused(_)));
}

#[test]
fn interrupted_focus_never_earns_downtime_or_erases_its_durable_native_reservation() {
    let inventory = single_target(86_400);
    let (mut scheduler, mut state) = initialize(&inventory);
    baseline(&mut scheduler, &mut state, 0);
    let focus = job(&inventory, "restart", "page-target");
    enqueue(&mut scheduler, &mut state, focus.clone(), 0);
    assert!(matches!(reserve(&mut scheduler, &mut state, 60), Work::Focused(_)));
    assert_eq!(state.clock.pending_job, Some(WorkKey::Focused { key: focus.key.clone() }));
    assert_eq!(state.focused[&focus.key].source_work, SourceWorkState::Active);
    for resumed_wall in [59, 3_600] {
        let mut resumed = Scheduler::new(&inventory, "one").unwrap();
        let mut saved = state.clone();
        resumed.restore(saved.clone(), Duration::ZERO, wall(resumed_wall)).unwrap();
        assert_eq!(resumed.focused_state(&focus.key).unwrap().source_work, SourceWorkState::Unconfirmed);
        assert!(resumed.take(Duration::ZERO).is_err(), "the interrupted marker must be rebased first");
        assert!(resumed.cancel_focused(&focus.key, Duration::ZERO, wall(resumed_wall)).is_err());
        let update = resumed.resumed_admission_checkpoint(Duration::ZERO, wall(resumed_wall)).unwrap().unwrap();
        persist(&mut resumed, &mut saved, update);
        assert_eq!(saved.focused[&focus.key].source_work, SourceWorkState::Unconfirmed);
        assert!(
            resumed.cancel_focused(&focus.key, Duration::ZERO, wall(resumed_wall)).is_err(),
            "rebasing never proves the admitted native work did not start"
        );
        assert!(resumed.take(seconds(71)).unwrap().is_none());
        assert!(matches!(reserve(&mut resumed, &mut saved, 72), Work::Focused(_)));
    }
}

#[test]
fn fractional_restart_cooldown_is_rounded_up_without_shortening_admission() {
    let inventory = single_target(3_600);
    let (mut scheduler, mut state) = initialize(&inventory);
    baseline(&mut scheduler, &mut state, 0);
    enqueue(&mut scheduler, &mut state, job(&inventory, "fraction", "page-target"), 0);
    reserve(&mut scheduler, &mut state, 60);
    scheduler.finish(seconds(60) + Duration::from_nanos(1), Outcome::More).unwrap();
    let update = scheduler.completion_checkpoint(seconds(60) + Duration::from_nanos(2), wall(60)).unwrap();
    persist(&mut scheduler, &mut state, update);
    let mut resumed = Scheduler::new(&inventory, "one").unwrap();
    resumed.restore(state, Duration::ZERO, wall(60)).unwrap();
    assert!(resumed.take(seconds(60) - Duration::from_nanos(1)).unwrap().is_none());
    assert!(matches!(resumed.take(seconds(60)).unwrap(), Some(Work::Focused(_))));
}

#[test]
fn completion_requires_exact_result_and_an_unconfirmed_attempt_cannot_be_canceled() {
    let inventory = single_target(3_600);
    let (mut scheduler, mut state) = initialize(&inventory);
    baseline(&mut scheduler, &mut state, 0);
    let focus = job(&inventory, "known-attempt", "page-target");
    enqueue(&mut scheduler, &mut state, focus.clone(), 0);
    reserve(&mut scheduler, &mut state, 60);
    assert!(scheduler.finish(seconds(60), Outcome::Complete).is_err());
    assert!(scheduler.finish(seconds(60), Outcome::Skipped { more: false }).is_err());
    finish(
        &mut scheduler,
        &mut state,
        60,
        Outcome::FocusedRetry {
            reason: FocusRetry::Failed,
            attempt: Some(result("known").capture),
            source_work: SourceWorkState::Unconfirmed,
        },
    );
    assert!(scheduler.cancel_focused(&focus.key, seconds(60), wall(60)).is_err());
    assert!(scheduler.retire_focused(&focus.key, seconds(60), wall(60)).is_err());
}

#[test]
fn settled_attempts_can_be_canceled_without_inventing_a_completed_result_or_erasing_cooldowns() {
    for source_work in [SourceWorkState::NotStarted, SourceWorkState::Quiescent] {
        for known_capture in [false, true] {
            let inventory = single_target(3_600);
            let (mut scheduler, mut state) = initialize(&inventory);
            baseline(&mut scheduler, &mut state, 0);
            let mut focus = job(&inventory, "settled-retry", "page-target");
            focus.cooldown_seconds = 300;
            enqueue(&mut scheduler, &mut state, focus.clone(), 0);
            reserve(&mut scheduler, &mut state, 60);
            assert_eq!(scheduler.focused_state(&focus.key).unwrap().source_work, SourceWorkState::Active);
            finish(
                &mut scheduler,
                &mut state,
                60,
                Outcome::FocusedRetry {
                    reason: FocusRetry::Failed,
                    attempt: known_capture.then(|| result("quiet-native-attempt").capture),
                    source_work,
                },
            );
            assert_eq!(state.focused[&focus.key].attempts, 1);
            assert_eq!(state.focused[&focus.key].status, FocusStatus::Pending);
            assert_eq!(state.focused[&focus.key].source_work, source_work);
            let resource = serde_json::to_value(&state.resources).unwrap();
            let family = serde_json::to_value(&state.focused_families).unwrap();
            let update = scheduler.cancel_focused(&focus.key, seconds(60), wall(60)).unwrap();
            assert!(matches!(update.focused, Some(FocusMutation::Remove { .. })));
            persist(&mut scheduler, &mut state, update);
            assert_eq!(serde_json::to_value(&state.resources).unwrap(), resource);
            assert_eq!(serde_json::to_value(&state.focused_families).unwrap(), family);
            assert!(state.focused.is_empty());
            assert!(scheduler.retire_focused_family(&focus.family, seconds(359), wall(359)).unwrap().is_none());
        }
    }
}

#[test]
fn a_metadata_rejection_cannot_clear_unknown_native_work_but_an_explicit_owner_close_can() {
    let inventory = single_target(86_400);
    let (mut scheduler, mut state) = initialize(&inventory);
    baseline(&mut scheduler, &mut state, 0);
    let focus = job(&inventory, "sticky-unknown-owner", "page-target");
    enqueue(&mut scheduler, &mut state, focus.clone(), 0);
    reserve(&mut scheduler, &mut state, 60);
    finish(&mut scheduler, &mut state, 60, Outcome::Failed);
    assert_eq!(state.focused[&focus.key].source_work, SourceWorkState::Unconfirmed);
    assert!(scheduler.cancel_focused(&focus.key, seconds(60), wall(60)).is_err());
    reserve(&mut scheduler, &mut state, 120);
    finish(
        &mut scheduler,
        &mut state,
        120,
        Outcome::FocusedRetry { reason: FocusRetry::More, attempt: None, source_work: SourceWorkState::NotStarted },
    );
    assert_eq!(state.focused[&focus.key].source_work, SourceWorkState::Unconfirmed);
    assert!(scheduler.cancel_focused(&focus.key, seconds(120), wall(120)).is_err());
    let mut resumed = Scheduler::new(&inventory, "one").unwrap();
    resumed.restore(state.clone(), seconds(120), wall(120)).unwrap();
    assert_eq!(resumed.focused_state(&focus.key).unwrap().source_work, SourceWorkState::Unconfirmed);
    assert!(resumed.cancel_focused(&focus.key, seconds(120), wall(120)).is_err());
    reserve(&mut scheduler, &mut state, 180);
    finish(
        &mut scheduler,
        &mut state,
        180,
        Outcome::FocusedRetry { reason: FocusRetry::Failed, attempt: None, source_work: SourceWorkState::Quiescent },
    );
    assert_eq!(state.focused[&focus.key].source_work, SourceWorkState::Quiescent);
    assert!(
        state.focused[&focus.key].last_attempt.is_none(),
        "closure comes from the owner, never the absence of a capture ID"
    );
    let update = scheduler.cancel_focused(&focus.key, seconds(180), wall(180)).unwrap();
    persist(&mut scheduler, &mut state, update);
    assert_eq!(scheduler.resources["shared"].attempts, 4);
}

#[test]
fn generic_retry_outcomes_without_owner_proof_remain_uncertain_and_active_is_not_a_settlement() {
    for outcome in [Outcome::Failed, Outcome::Pressure, Outcome::More] {
        let inventory = single_target(3_600);
        let (mut scheduler, mut state) = initialize(&inventory);
        baseline(&mut scheduler, &mut state, 0);
        let focus = job(&inventory, "no-owner-proof", "page-target");
        enqueue(&mut scheduler, &mut state, focus.clone(), 0);
        reserve(&mut scheduler, &mut state, 60);
        assert!(
            scheduler
                .finish(
                    seconds(60),
                    Outcome::FocusedRetry {
                        reason: FocusRetry::Failed,
                        attempt: None,
                        source_work: SourceWorkState::Active,
                    }
                )
                .is_err()
        );
        finish(&mut scheduler, &mut state, 60, outcome);
        assert_eq!(state.focused[&focus.key].source_work, SourceWorkState::Unconfirmed);
        assert_eq!(state.focused[&focus.key].status, FocusStatus::Pending);
        assert!(scheduler.cancel_focused(&focus.key, seconds(60), wall(60)).is_err());
    }
}

#[test]
fn completed_gc_keeps_resource_history_and_cross_version_family_cooldown() {
    let inventory = single_target(3_600);
    let (mut scheduler, mut state) = initialize(&inventory);
    baseline(&mut scheduler, &mut state, 0);
    let mut first = job(&inventory, "completed", "page-target");
    first.cooldown_seconds = 300;
    enqueue(&mut scheduler, &mut state, first.clone(), 0);
    reserve(&mut scheduler, &mut state, 60);
    let sealed = result("sealed");
    finish(&mut scheduler, &mut state, 60, Outcome::FocusedComplete { result: sealed.clone() });
    assert!(matches!(&state.focused[&first.key].status, FocusStatus::Completed { result } if result == &sealed));
    assert_eq!(state.focused[&first.key].source_work, SourceWorkState::Quiescent);
    assert!(scheduler.cancel_focused(&first.key, seconds(60), wall(60)).is_err());
    let update = scheduler.retire_focused(&first.key, seconds(60), wall(60)).unwrap();
    persist(&mut scheduler, &mut state, update);
    assert!(scheduler.retire_focused_family(&first.family, seconds(359), wall(359)).unwrap().is_none());
    let mut next = job(&inventory, "new-version", "page-target");
    next.recipe_version = 2;
    assert_eq!(first.family, next.family);
    enqueue(&mut scheduler, &mut state, next, 60);
    assert!(scheduler.take(seconds(359)).unwrap().is_none());
    assert!(matches!(reserve(&mut scheduler, &mut state, 360), Work::Focused(_)));
    assert_eq!(scheduler.resources["shared"].attempts, 2, "unfinished reservation is not yet an in-memory completion");
}

#[test]
fn removing_all_focus_jobs_does_not_make_retained_resource_history_invalid_after_restart() {
    let inventory = single_target(3_600);
    let (mut scheduler, mut state) = initialize(&inventory);
    baseline(&mut scheduler, &mut state, 0);
    let focus = job(&inventory, "retired", "page-target");
    enqueue(&mut scheduler, &mut state, focus.clone(), 0);
    reserve(&mut scheduler, &mut state, 60);
    finish(&mut scheduler, &mut state, 60, Outcome::FocusedComplete { result: result("done") });
    let update = scheduler.retire_focused(&focus.key, seconds(60), wall(60)).unwrap();
    persist(&mut scheduler, &mut state, update);
    let mut resumed = Scheduler::new(&inventory, "one").unwrap();
    resumed.restore(state, Duration::ZERO, wall(60)).unwrap();
    assert_eq!(resumed.resources["shared"].attempts, 2);
    assert_eq!(
        resumed.resources["shared"].max_reservation,
        native_reservation(&inventory.targets[0].schedules[0].budget)
    );
    let update = resumed.retire_focused_family(&focus.family, seconds(60), wall(120)).unwrap().unwrap();
    assert!(matches!(update.focused_family, Some(FocusFamilyMutation::Remove { .. })));
    assert_eq!(resumed.resources["shared"].attempts, 2);
}

#[test]
fn enrollment_is_durable_route_bound_and_deduplicates_an_episode_across_baselines() {
    let inventory = single_target(180);
    let (mut scheduler, mut state) = initialize(&inventory);
    let focus = job(&inventory, "dedup", "page-target");
    let mut wrong = focus.clone();
    wrong.connection = "different-route".into();
    assert!(scheduler.enqueue_focus(wrong, Duration::ZERO, wall(0)).is_err());
    let FocusEnrollment::Enqueued(update) = scheduler.enqueue_focus(focus.clone(), Duration::ZERO, wall(0)).unwrap()
    else {
        panic!("new focus")
    };
    assert!(scheduler.take(Duration::ZERO).is_err(), "durability must settle before admission");
    persist(&mut scheduler, &mut state, *update);
    let mut newer = focus.clone();
    newer.key = digest(b"newer-parent-baseline");
    newer.recipe_version = 2;
    assert!(
        matches!(scheduler.enqueue_focus(newer, Duration::ZERO, wall(0)).unwrap(), FocusEnrollment::Existing { key } if key == focus.key)
    );
    assert_eq!(scheduler.focused.len(), 1);
    assert_eq!(scheduler.jobs.len(), 1);
    let update = scheduler.cancel_focused(&focus.key, Duration::ZERO, wall(0)).unwrap();
    assert!(scheduler.take(Duration::ZERO).is_err(), "removal must settle durably before another turn");
    persist(&mut scheduler, &mut state, update);
    assert!(matches!(reserve(&mut scheduler, &mut state, 0), Work::Scheduled(_)));
}

#[test]
fn compact_focus_binds_the_exact_validated_intent_without_copying_parent_or_cost_payloads() {
    let fixture = crate::analytical::test_support::investigation_fixture();
    let inventory = single_target(180);
    let job = FocusedJob::new(&fixture.intent, &inventory.targets[0], 60).unwrap();
    assert!(job.matches_intent(&fixture.intent));
    assert_eq!(job.key, fixture.intent.key().unwrap());
    assert_eq!(job.budget, fixture.intent.budget);
    assert!(serde_json::to_vec(&job).unwrap().len() < 4_096);
    let mut wrong = job.clone();
    wrong.budget.total_timeout_ms += 1;
    assert!(!wrong.matches_intent(&fixture.intent));
    let mut wrong = job;
    wrong.episode_stage_key = digest(b"another-stage");
    assert!(!wrong.matches_intent(&fixture.intent));
}

#[test]
fn focused_job_capacity_defers_without_dropping_or_changing_static_membership() {
    let inventory = single_target(180);
    let (mut scheduler, mut state) = initialize(&inventory);
    for index in 0..MAX_FOCUSED_JOBS {
        enqueue(&mut scheduler, &mut state, job(&inventory, &index.to_string(), "page-target"), 0);
    }
    let sequence = scheduler.sequence;
    assert!(matches!(
        scheduler.enqueue_focus(job(&inventory, "overflow", "page-target"), Duration::ZERO, wall(0)).unwrap(),
        FocusEnrollment::Capacity
    ));
    assert_eq!(scheduler.sequence, sequence);
    assert_eq!(scheduler.focused.len(), MAX_FOCUSED_JOBS);
    assert_eq!(scheduler.focused_families.len(), 1);
    assert!(matches!(reserve(&mut scheduler, &mut state, 0), Work::Scheduled(_)));
}

#[test]
fn independent_family_capacity_is_bounded_and_expired_families_can_be_retired() {
    let inventory = single_target(180);
    let (mut scheduler, mut state) = initialize(&inventory);
    for index in 0..MAX_FOCUSED_FAMILIES {
        let focus = job(&inventory, &index.to_string(), &format!("recipe-{index}"));
        enqueue(&mut scheduler, &mut state, focus.clone(), 0);
        let update = scheduler.cancel_focused(&focus.key, Duration::ZERO, wall(0)).unwrap();
        persist(&mut scheduler, &mut state, update);
    }
    assert!(matches!(
        scheduler.enqueue_focus(job(&inventory, "overflow", "another-recipe"), Duration::ZERO, wall(0)).unwrap(),
        FocusEnrollment::Capacity
    ));
    let key = scheduler.focused_families.keys().next().unwrap().clone();
    let update = scheduler.retire_focused_family(&key, Duration::ZERO, wall(0)).unwrap().unwrap();
    persist(&mut scheduler, &mut state, update);
    enqueue(&mut scheduler, &mut state, job(&inventory, "new", "another-recipe"), 0);
    assert_eq!(scheduler.focused_families.len(), MAX_FOCUSED_FAMILIES);
}

#[test]
fn restore_rejects_unknown_route_duplicate_episode_and_unmatched_admitted_marker() {
    let inventory = single_target(180);
    let (mut scheduler, mut state) = initialize(&inventory);
    let first = job(&inventory, "one", "page-target");
    let second = job(&inventory, "two", "page-target");
    enqueue(&mut scheduler, &mut state, first.clone(), 0);
    enqueue(&mut scheduler, &mut state, second.clone(), 0);
    for defect in 0..8 {
        let mut saved = state.clone();
        let row = saved.focused.get_mut(&second.key).unwrap();
        match defect {
            0 => row.job.connection = "unknown".into(),
            1 => row.job.episode_id = first.episode_id.clone(),
            2 => {
                row.status = FocusStatus::Admitted;
                row.attempts = 1;
                row.failures = 1;
                row.source_work = SourceWorkState::Active;
            }
            3 => row.last_attempt = Some(result("impossible-attempt-with-zero-admissions").capture),
            4 => row.source_work = SourceWorkState::Quiescent,
            5 => row.source_work = SourceWorkState::Active,
            6 => {
                let capture = result("unconfirmed-completion");
                row.attempts = 1;
                row.last_attempt = Some(capture.capture.clone());
                row.status = FocusStatus::Completed { result: capture };
                row.source_work = SourceWorkState::Unconfirmed;
            }
            _ => {
                saved.jobs.clear();
            }
        }
        let mut resumed = Scheduler::new(&inventory, "one").unwrap();
        assert!(resumed.restore(saved, Duration::ZERO, wall(0)).is_err());
    }
}
