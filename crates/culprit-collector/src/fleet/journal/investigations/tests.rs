use super::*;
use crate::{
    analytical::test_support::investigation_fixture,
    fleet::scheduler::{ADMISSION_START_WINDOW, FocusEnrollment, FocusedJob, Outcome, Scheduler, Work, WorkKey},
};
use chrono::{DateTime, TimeDelta, Utc};
use std::time::Duration;

struct Fixture {
    directory: tempfile::TempDir,
    inventory: FleetInventory,
}
impl Fixture {
    fn new() -> Self {
        Self {
            directory: tempfile::tempdir().unwrap(),
            inventory: FleetInventory {
                targets: vec![FleetTarget {
                    id: "fixture".into(),
                    connection: "saved-source".into(),
                    shard: "one".into(),
                    subject: CollectionSubject::Database,
                    admission_group: "physical-resource".into(),
                    schedules: vec![FleetSchedule {
                        id: "baseline".into(),
                        every_seconds: 86_400,
                        jitter_seconds: 0,
                        collection: ScheduledCollection::Activity { policy: ActivityPolicy::default() },
                        budget: CollectionBudget::default(),
                    }],
                }],
            },
        }
    }
    fn connection(&self) -> Connection {
        Connection::open(self.directory.path().join("fleet/schedule.sqlite3")).unwrap()
    }
    async fn initialize(&self) -> (FleetJournal, Scheduler) {
        let journal = FleetJournal::open(&self.directory.path().join("fleet"), &self.inventory, "one").await.unwrap();
        let mut scheduler = Scheduler::new(&self.inventory, "one").unwrap();
        let state = scheduler.initial_state(Duration::ZERO, wall(0)).unwrap();
        scheduler.persisted(journal.initialize_scheduler(state).await.unwrap()).unwrap();
        (journal, scheduler)
    }
    async fn reopen(&self, at: i64) -> (FleetJournal, Scheduler) {
        let journal = FleetJournal::open(&self.directory.path().join("fleet"), &self.inventory, "one").await.unwrap();
        let mut scheduler = Scheduler::new(&self.inventory, "one").unwrap();
        scheduler.restore(journal.scheduler().await.unwrap().unwrap(), Duration::ZERO, wall(at)).unwrap();
        (journal, scheduler)
    }
    fn focused_job(&self, intent: &DiagnosticInvestigationIntent) -> FocusedJob {
        FocusedJob::new(intent, &self.inventory.targets[0], 60).unwrap()
    }
}
fn wall(seconds: i64) -> DateTime<Utc> {
    "2026-11-14T12:00:00Z".parse::<DateTime<Utc>>().unwrap() + TimeDelta::seconds(seconds)
}
fn elapsed(seconds: u64) -> Duration {
    Duration::from_secs(seconds)
}
fn result(name: &str) -> DiagnosticCaptureRef {
    DiagnosticCaptureRef {
        capture: CaptureIdentity {
            observation_id: name.into(),
            revision: Some(CaptureRevision {
                outbox_id: "owned-outbox".into(),
                owner_epoch: "1".into(),
                sequence: "2".into(),
            }),
        },
        manifest_sha256: digest(format!("manifest:{name}").as_bytes()),
        source_scope_id: digest(b"focused-page-source-scope"),
    }
}
fn enrollment(scheduler: &mut Scheduler, job: FocusedJob, at: u64) -> ScheduleUpdate {
    let FocusEnrollment::Enqueued(update) = scheduler.enqueue_focus(job, elapsed(at), wall(at as i64)).unwrap() else {
        panic!("expected a new immutable investigation");
    };
    *update
}
async fn enqueue(
    fixture: &Fixture,
    journal: &FleetJournal,
    scheduler: &mut Scheduler,
    intent: &DiagnosticInvestigationIntent,
) -> FocusedJob {
    let job = fixture.focused_job(intent);
    let update = enrollment(scheduler, job.clone(), 0);
    scheduler.persisted(journal.enqueue_focused(intent.clone(), update).await.unwrap()).unwrap();
    job
}
async fn reserve(journal: &FleetJournal, scheduler: &mut Scheduler, at: u64) -> Work {
    let work = scheduler.take(elapsed(at)).unwrap().unwrap();
    let update = scheduler.admission_checkpoint(elapsed(at), wall(at as i64)).unwrap();
    scheduler.persisted(journal.update_scheduler(update).await.unwrap()).unwrap();
    work
}
async fn finish(journal: &FleetJournal, scheduler: &mut Scheduler, at: u64, outcome: Outcome) {
    scheduler.finish(elapsed(at), outcome).unwrap();
    let update = scheduler.completion_checkpoint(elapsed(at), wall(at as i64)).unwrap();
    scheduler.persisted(journal.update_scheduler(update).await.unwrap()).unwrap();
}
async fn baseline(journal: &FleetJournal, scheduler: &mut Scheduler) {
    assert!(matches!(reserve(journal, scheduler, 0).await, Work::Scheduled(_)));
    finish(journal, scheduler, 0, Outcome::Complete).await;
}
fn next_intent(original: &DiagnosticInvestigationIntent, index: usize) -> DiagnosticInvestigationIntent {
    let mut intent = original.clone();
    intent.episode_id = digest(format!("independent-episode:{index}").as_bytes());
    assert!(intent.is_valid());
    intent
}

#[tokio::test]
async fn canonical_intent_enrollment_is_atomic_and_an_exact_lost_acknowledgement_is_idempotent() {
    let fixture = Fixture::new();
    let (journal, mut scheduler) = fixture.initialize().await;
    let original = investigation_fixture().intent;
    let before = journal.scheduler().await.unwrap().unwrap();
    let mut duplicate = Scheduler::new(&fixture.inventory, "one").unwrap();
    duplicate.restore(before.clone(), Duration::ZERO, wall(0)).unwrap();
    let job = enqueue(&fixture, &journal, &mut scheduler, &original).await;
    let retry = enrollment(&mut duplicate, job.clone(), 0);
    duplicate.persisted(journal.enqueue_focused(original.clone(), retry).await.unwrap()).unwrap();
    assert_eq!(journal.focused_intents().await.unwrap().as_slice(), std::slice::from_ref(&original));
    assert!(matches!(
        journal.focused_stage(&original.episode_stage_key().unwrap()).await.unwrap(),
        Some(FocusedStage::Outstanding)
    ));
    let saved = journal.scheduler().await.unwrap().unwrap();
    assert_eq!(saved.focused.len(), 1);
    assert_eq!(saved.clock.revision, before.clock.revision + 1);
    assert_eq!(serde_json::to_value(&saved.jobs).unwrap(), serde_json::to_value(&before.jobs).unwrap());
    assert_eq!(serde_json::to_value(&saved.resources).unwrap(), serde_json::to_value(&before.resources).unwrap());

    let mut newer = original;
    newer.episode_revision = "2".into();
    newer.recipe_version += 1;
    newer.qualification.recipe_version = newer.recipe_version;
    let mut stale = Scheduler::new(&fixture.inventory, "one").unwrap();
    stale.restore(before, Duration::ZERO, wall(0)).unwrap();
    let competing = enrollment(&mut stale, fixture.focused_job(&newer), 0);
    assert!(journal.enqueue_focused(newer, competing).await.is_err());
    assert_eq!(journal.scheduler().await.unwrap().unwrap().clock.revision, saved.clock.revision);
}

#[tokio::test]
async fn enrollment_storage_failure_rolls_back_intent_focus_family_and_clock_together() {
    let fixture = Fixture::new();
    let (journal, mut scheduler) = fixture.initialize().await;
    let intent = investigation_fixture().intent;
    let before = journal.scheduler().await.unwrap().unwrap();
    fixture.connection().execute_batch("CREATE TRIGGER reject_enrollment BEFORE INSERT ON admission_focus_families BEGIN SELECT RAISE(ABORT,'owned storage fault'); END;").unwrap();
    let update = enrollment(&mut scheduler, fixture.focused_job(&intent), 0);
    assert!(journal.enqueue_focused(intent.clone(), update).await.is_err());
    assert!(journal.focused_intents().await.unwrap().is_empty());
    assert!(journal.focused_stage(&intent.episode_stage_key().unwrap()).await.unwrap().is_none());
    let after = journal.scheduler().await.unwrap().unwrap();
    assert_eq!(after.clock.revision, before.clock.revision);
    assert!(after.focused.is_empty() && after.focused_families.is_empty());
    assert_eq!(serde_json::to_value(&after.resources).unwrap(), serde_json::to_value(&before.resources).unwrap());
    assert!(scheduler.take(Duration::ZERO).is_err(), "unacknowledged metadata cannot admit native work");
}

#[tokio::test]
async fn crash_reservations_keep_canonical_parent_and_cooldowns_without_gaining_cancellation_rights() {
    let fixture = Fixture::new();
    let (journal, mut scheduler) = fixture.initialize().await;
    baseline(&journal, &mut scheduler).await;
    let intent = investigation_fixture().intent;
    let job = enqueue(&fixture, &journal, &mut scheduler, &intent).await;
    let pending = journal.scheduler().await.unwrap().unwrap();
    let mut stale = Scheduler::new(&fixture.inventory, "one").unwrap();
    stale.restore(pending.clone(), Duration::ZERO, wall(0)).unwrap();
    let stale_cancel = stale.cancel_focused(&job.key, Duration::ZERO, wall(0)).unwrap();
    assert!(matches!(reserve(&journal, &mut scheduler, 60).await, Work::Focused(actual) if actual == job));
    assert!(journal.cancel_focused(stale_cancel, DiagnosticAnalysisIssue::SourceChanged).await.is_err());
    let saved = journal.scheduler().await.unwrap().unwrap();
    assert_eq!(saved.clock.pending_job, Some(WorkKey::Focused { key: job.key.clone() }));
    assert_eq!(saved.focused[&job.key].status, FocusStatus::Admitted);
    drop(journal);

    let (journal, mut scheduler) = fixture.reopen(3_600).await;
    assert_eq!(journal.focused_intent(&job.key).await.unwrap(), Some(intent.clone()));
    assert!(scheduler.take(Duration::ZERO).is_err());
    let rebase = scheduler.resumed_admission_checkpoint(Duration::ZERO, wall(3_600)).unwrap().unwrap();
    scheduler.persisted(journal.update_scheduler(rebase).await.unwrap()).unwrap();
    assert!(scheduler.cancel_focused(&job.key, Duration::ZERO, wall(3_600)).is_err());
    let saved = journal.scheduler().await.unwrap().unwrap();
    assert!(saved.clock.pending_job.is_none());
    assert_eq!(saved.focused[&job.key].attempts, 1);
    let mut stale = Scheduler::new(&fixture.inventory, "one").unwrap();
    stale.restore(pending, Duration::ZERO, wall(0)).unwrap();
    let stale_cancel = stale.cancel_focused(&job.key, Duration::ZERO, wall(0)).unwrap();
    assert!(
        journal.cancel_focused(stale_cancel, DiagnosticAnalysisIssue::SourceChanged).await.is_err(),
        "rebasing a crash does not prove that native work never started"
    );
    let reserved = ADMISSION_START_WINDOW
        + Duration::from_millis(u64::from(intent.budget.total_timeout_ms))
        + crate::capture::SOURCE_SHUTDOWN_TIMEOUT
        + elapsed(60);
    assert!(scheduler.take(reserved - Duration::from_millis(1)).unwrap().is_none());
    assert!(matches!(scheduler.take(reserved).unwrap(), Some(Work::Focused(actual)) if actual == job));
}

#[tokio::test]
async fn failed_native_admission_storage_cannot_partially_charge_or_mark_a_focus_admitted() {
    let fixture = Fixture::new();
    let (journal, mut scheduler) = fixture.initialize().await;
    baseline(&journal, &mut scheduler).await;
    let intent = investigation_fixture().intent;
    let job = enqueue(&fixture, &journal, &mut scheduler, &intent).await;
    let before = journal.scheduler().await.unwrap().unwrap();
    fixture.connection().execute_batch("CREATE TRIGGER reject_admission BEFORE UPDATE ON admission_resources BEGIN SELECT RAISE(ABORT,'owned storage fault'); END;").unwrap();
    assert!(matches!(scheduler.take(elapsed(60)).unwrap(), Some(Work::Focused(_))));
    let admission = scheduler.admission_checkpoint(elapsed(60), wall(60)).unwrap();
    assert!(journal.update_scheduler(admission).await.is_err());
    let after = journal.scheduler().await.unwrap().unwrap();
    assert_eq!(after.clock.revision, before.clock.revision);
    assert!(after.clock.pending_job.is_none());
    assert_eq!(after.focused[&job.key].status, FocusStatus::Pending);
    assert_eq!(after.focused[&job.key].attempts, 0);
    assert_eq!(serde_json::to_value(&after.resources).unwrap(), serde_json::to_value(&before.resources).unwrap());
    assert_eq!(
        serde_json::to_value(&after.focused_families).unwrap(),
        serde_json::to_value(&before.focused_families).unwrap()
    );
}

#[tokio::test]
async fn an_owner_confirmed_quiescent_retry_can_cancel_without_erasing_resource_cooldown() {
    let fixture = Fixture::new();
    let (journal, mut scheduler) = fixture.initialize().await;
    baseline(&journal, &mut scheduler).await;
    let intent = investigation_fixture().intent;
    let job = enqueue(&fixture, &journal, &mut scheduler, &intent).await;
    reserve(&journal, &mut scheduler, 60).await;
    finish(
        &journal,
        &mut scheduler,
        60,
        Outcome::FocusedRetry {
            reason: crate::fleet::scheduler::FocusRetry::Failed,
            attempt: Some(result("settled-native-attempt").capture),
            source_work: SourceWorkState::Quiescent,
        },
    )
    .await;
    let before = journal.scheduler().await.unwrap().unwrap();
    assert_eq!(before.focused[&job.key].source_work, SourceWorkState::Quiescent);
    assert!(before.focused[&job.key].last_attempt.is_some());
    let update = scheduler.cancel_focused(&job.key, elapsed(60), wall(60)).unwrap();
    scheduler.persisted(journal.cancel_focused(update, DiagnosticAnalysisIssue::SourceChanged).await.unwrap()).unwrap();
    let after = journal.scheduler().await.unwrap().unwrap();
    assert!(after.focused.is_empty());
    assert_eq!(serde_json::to_value(before.resources).unwrap(), serde_json::to_value(after.resources).unwrap());
    assert_eq!(
        serde_json::to_value(before.focused_families).unwrap(),
        serde_json::to_value(after.focused_families).unwrap()
    );
}

#[tokio::test]
async fn completed_receipt_survives_restart_and_rejects_a_new_parent_without_retaining_full_intent_json() {
    let fixture = Fixture::new();
    let (journal, mut scheduler) = fixture.initialize().await;
    baseline(&journal, &mut scheduler).await;
    let intent = investigation_fixture().intent;
    let job = enqueue(&fixture, &journal, &mut scheduler, &intent).await;
    reserve(&journal, &mut scheduler, 60).await;
    let completed = result("focused-native-result");
    finish(&journal, &mut scheduler, 65, Outcome::FocusedComplete { result: completed.clone() }).await;
    let update = scheduler.retire_focused(&job.key, elapsed(65), wall(65)).unwrap();
    scheduler.persisted(journal.retire_focused(update).await.unwrap()).unwrap();
    assert!(journal.focused_intents().await.unwrap().is_empty());
    let Some(receipt) = journal.focused_receipt(&intent.episode_stage_key().unwrap()).await.unwrap() else {
        panic!("a durable completed receipt")
    };
    assert_eq!(receipt.settlement, FocusedSettlement::Completed { result: completed.clone() });
    assert_eq!(journal.focused_receipt_episode_ids().await.unwrap(), [intent.episode_id.clone()].into());
    drop(journal);
    let (journal, mut scheduler) = fixture.reopen(65).await;
    let saved = journal.scheduler().await.unwrap().unwrap();
    assert!(saved.focused.is_empty());
    assert!(saved.focused_families.contains_key(&job.family), "recipe cooldown outlives full payload retirement");
    let mut newer = intent.clone();
    newer.episode_revision = "2".into();
    let candidate = enrollment(&mut scheduler, fixture.focused_job(&newer), 0);
    assert!(journal.enqueue_focused(newer, candidate).await.is_err());
    assert_eq!(journal.scheduler().await.unwrap().unwrap().clock.revision, saved.clock.revision);
    journal.prune_focused_receipts(&[intent.episode_id.clone()].into()).await.unwrap();
    assert!(matches!(
        journal.focused_stage(&intent.episode_stage_key().unwrap()).await.unwrap(),
        Some(FocusedStage::Completed)
    ));
    journal.prune_focused_receipts(&BTreeSet::new()).await.unwrap();
    assert!(journal.focused_receipt_episode_ids().await.unwrap().is_empty());
    assert!(journal.focused_stage(&intent.episode_stage_key().unwrap()).await.unwrap().is_none());
    assert_eq!(
        fixture
            .connection()
            .query_row("SELECT count(*) FROM investigation_intents", [], |row| row.get::<_, u32>(0))
            .unwrap(),
        0
    );
}

#[tokio::test]
async fn only_a_new_validated_intent_can_replace_a_never_started_cancellation() {
    let fixture = Fixture::new();
    let (journal, mut scheduler) = fixture.initialize().await;
    let intent = investigation_fixture().intent;
    let job = enqueue(&fixture, &journal, &mut scheduler, &intent).await;
    let cancelled = scheduler.cancel_focused(&job.key, Duration::ZERO, wall(0)).unwrap();
    scheduler
        .persisted(journal.cancel_focused(cancelled, DiagnosticAnalysisIssue::SourceChanged).await.unwrap())
        .unwrap();
    let Some(receipt) = journal.focused_receipt(&intent.episode_stage_key().unwrap()).await.unwrap() else {
        panic!("typed cancelled receipt")
    };
    assert_eq!(receipt.settlement, FocusedSettlement::Cancelled { reason: DiagnosticAnalysisIssue::SourceChanged });
    drop(journal);
    let (journal, mut scheduler) = fixture.reopen(0).await;
    let retry = enrollment(&mut scheduler, fixture.focused_job(&intent), 0);
    assert!(
        journal.enqueue_focused(intent.clone(), retry).await.is_err(),
        "the cancelled immutable key cannot be revived"
    );
    drop(journal);
    let (journal, mut scheduler) = fixture.reopen(0).await;
    let mut replacement = intent;
    replacement.recipe_version += 1;
    replacement.qualification.recipe_version = replacement.recipe_version;
    let replacement_job = enqueue(&fixture, &journal, &mut scheduler, &replacement).await;
    assert_ne!(replacement_job.key, job.key);
    assert_eq!(replacement_job.family, job.family, "recipe version does not reset its resource family");
    assert!(matches!(
        journal.focused_stage(&replacement.episode_stage_key().unwrap()).await.unwrap(),
        Some(FocusedStage::Outstanding)
    ));
    assert_eq!(
        fixture
            .connection()
            .query_row("SELECT count(*) FROM investigation_receipts", [], |row| row.get::<_, u32>(0))
            .unwrap(),
        0
    );
}

#[tokio::test]
async fn queue_capacity_preserves_every_canonical_intent_and_periodic_job_across_restart() {
    let fixture = Fixture::new();
    let (journal, mut scheduler) = fixture.initialize().await;
    let original = investigation_fixture().intent;
    for index in 0..MAX_FOCUSED_JOBS {
        enqueue(&fixture, &journal, &mut scheduler, &next_intent(&original, index)).await;
    }
    assert!(matches!(
        scheduler
            .enqueue_focus(fixture.focused_job(&next_intent(&original, MAX_FOCUSED_JOBS)), Duration::ZERO, wall(0))
            .unwrap(),
        FocusEnrollment::Capacity
    ));
    assert_eq!(journal.focused_intents().await.unwrap().len(), MAX_FOCUSED_JOBS);
    drop(journal);
    let (journal, mut scheduler) = fixture.reopen(0).await;
    let saved = journal.scheduler().await.unwrap().unwrap();
    assert_eq!(saved.focused.len(), MAX_FOCUSED_JOBS);
    assert_eq!(saved.jobs.len(), 1);
    assert!(matches!(reserve(&journal, &mut scheduler, 0).await, Work::Scheduled(_)));
}

#[tokio::test]
async fn aggregate_payload_quota_is_independent_of_the_job_count_limit() {
    let fixture = Fixture::new();
    let (journal, mut scheduler) = fixture.initialize().await;
    let original = investigation_fixture().intent;
    let connection = fixture.connection();
    for index in 0..MAX_INTENTS_BYTES / MAX_INTENT_BYTES {
        let intent = next_intent(&original, index);
        enqueue(&fixture, &journal, &mut scheduler, &intent).await;
        let mut json = serde_json::to_string(&intent).unwrap();
        json.extend(std::iter::repeat_n(' ', MAX_INTENT_BYTES - json.len()));
        connection
            .execute(
                "UPDATE investigation_intents SET intent_json=?1,intent_sha=?2 WHERE id=?3",
                params![json, digest(json.as_bytes()), intent.key().unwrap()],
            )
            .unwrap();
    }
    let count = journal.focused_intents().await.unwrap().len();
    assert!(count < MAX_FOCUSED_JOBS);
    let before = journal.scheduler().await.unwrap().unwrap();
    let candidate = next_intent(&original, MAX_FOCUSED_JOBS);
    let update = enrollment(&mut scheduler, fixture.focused_job(&candidate), 0);
    assert!(journal.enqueue_focused(candidate, update).await.is_err());
    assert_eq!(journal.focused_intents().await.unwrap().len(), count);
    assert_eq!(journal.scheduler().await.unwrap().unwrap().clock.revision, before.clock.revision);
}

#[tokio::test]
async fn an_oversized_original_statement_cannot_consume_unbounded_journal_metadata() {
    let fixture = Fixture::new();
    let (journal, mut scheduler) = fixture.initialize().await;
    let mut intent = investigation_fixture().intent;
    let text = "x".repeat(MAX_INTENT_BYTES);
    intent.focus.original.statement =
        DefinitionPayload::Available { sha256: digest(text.as_bytes()), text, redaction_version: REDACTION_VERSION };
    assert!(intent.is_valid(), "the metadata owner enforces its own byte ceiling");
    let update = enrollment(&mut scheduler, fixture.focused_job(&intent), 0);
    assert!(journal.enqueue_focused(intent, update).await.is_err());
    assert!(journal.focused_intents().await.unwrap().is_empty());
    assert!(journal.scheduler().await.unwrap().unwrap().focused.is_empty());
}

#[tokio::test]
async fn a_receipt_with_conflicting_storage_identity_cannot_authorize_replay_or_be_silently_pruned() {
    let fixture = Fixture::new();
    let (journal, mut scheduler) = fixture.initialize().await;
    let intent = investigation_fixture().intent;
    let job = enqueue(&fixture, &journal, &mut scheduler, &intent).await;
    let cancelled = scheduler.cancel_focused(&job.key, Duration::ZERO, wall(0)).unwrap();
    scheduler
        .persisted(journal.cancel_focused(cancelled, DiagnosticAnalysisIssue::SourceChanged).await.unwrap())
        .unwrap();
    fixture
        .connection()
        .execute("UPDATE investigation_receipts SET id=?1", [digest(b"a different immutable intent")])
        .unwrap();
    assert!(journal.focused_stage(&intent.episode_stage_key().unwrap()).await.is_err());
    assert!(journal.focused_receipt_episode_ids().await.is_err());
    assert!(journal.prune_focused_receipts(&BTreeSet::new()).await.is_err());
    assert_eq!(
        fixture
            .connection()
            .query_row("SELECT count(*) FROM investigation_receipts", [], |row| row.get::<_, u32>(0))
            .unwrap(),
        1
    );
}

#[tokio::test]
async fn altered_canonical_bytes_and_missing_intent_rows_fail_closed_before_restore() {
    let fixture = Fixture::new();
    let (journal, mut scheduler) = fixture.initialize().await;
    let intent = investigation_fixture().intent;
    let job = enqueue(&fixture, &journal, &mut scheduler, &intent).await;
    let connection = fixture.connection();
    connection
        .execute(
            "UPDATE investigation_intents SET intent_sha=?1 WHERE id=?2",
            params![digest(b"altered payload"), job.key],
        )
        .unwrap();
    assert!(journal.focused_intents().await.is_err());
    assert!(journal.scheduler().await.is_err());
    connection.execute("DELETE FROM investigation_intents", []).unwrap();
    assert!(
        journal.scheduler().await.is_err(),
        "a compact execution row without its exact original intent cannot reopen native work"
    );
}
