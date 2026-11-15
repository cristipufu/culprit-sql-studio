//! Persisted synthetic stages exercise journal ownership, never source permission or production cost qualification.
use super::*;
use culprit_core::{Engine, MeasurementScope};

fn paired_intents() -> (DiagnosticInvestigationIntent, DiagnosticInvestigationIntent) {
    let fixture = investigation_fixture();
    let member = SharedResourceIdentity {
        engine: Engine::Mssql,
        kind: SharedResourceKind::Instance,
        namespace: NativeIdentity::new("owned-native-namespace"),
        native: NativeIdentity::new("owned-native-member"),
    };
    let mut source = fixture.baseline_source;
    let epoch = source.observation.activity().unwrap().epoch.clone();
    source.partition = SourcePartition::InstanceLocal { member: Some(member.clone()), epoch };
    let mut original_record = fixture.baseline_record.record;
    let TelemetryRecord::Activity(original) = &mut original_record else { panic!() };
    original.wait.as_mut().unwrap().identified_resources[0].native.id = "2:1:4144".into();
    let row = ExportedRecord::new(&source, &fixture.baseline_record.capture, original_record).unwrap();
    let reference = DiagnosticEvidenceRef::from_export(&source, &digest(b"owned-db-baseline"), &row, 0).unwrap();
    let TelemetryRecord::Activity(original) = &row.record else { panic!() };
    let rows = [DiagnosticActivityRow { reference: &reference, observation: original }];
    let update = observe_diagnostic_activity(
        &source,
        &reference.capture_ref(),
        &rows,
        None,
        &ActivityPolicy::default(),
        "2020-01-01T12:00:02Z",
    )
    .unwrap();
    let episode = &update.checkpoint.episodes[0];
    let focus = update.focuses[0].1.clone();
    let component = fixture.intent.qualification.component;
    let mut recipe = fixture.recipe;
    let RecipeCostQualification::Measured(receipt) = &mut recipe.cost else { panic!() };
    receipt.scope = RecipeCostScope::Activity(episode.scope.clone());
    let database =
        DiagnosticInvestigationIntent::new(episode, &recipe, focus.clone(), &component, &CollectionBudget::default())
            .unwrap();

    let mut owner_source = fixture.source;
    owner_source.target = CollectionTarget::Shared(member.clone());
    owner_source.partition = SourcePartition::InstanceLocal {
        member: Some(member.clone()),
        epoch: Some("owned-instance-generation".into()),
    };
    let mut owner_record = fixture.record.record;
    let TelemetryRecord::Operational(observation) = &mut owner_record else { panic!() };
    let OperationalFact::CounterSet(counters) = &mut observation.fact else { panic!() };
    counters.scope = MeasurementScope::Server;
    for sample in &mut counters.counters {
        sample.scope = MeasurementScope::Server;
    }
    observation.key = counters.key().unwrap();
    let owner_row = ExportedRecord::new(&owner_source, &fixture.record.capture, owner_record).unwrap();
    let owner =
        DiagnosticEvidenceRef::from_export(&owner_source, &digest(b"owned-shared-baseline"), &owner_row, 0).unwrap();
    recipe.subjects = vec![RecipeSubject::Shared(member.kind)];
    let RecipeCostQualification::Measured(receipt) = &mut recipe.cost else { panic!() };
    receipt.scope = RecipeCostScope::SharedResource {
        target: member,
        partition: owner_source.partition.clone(),
        source: owner_source.source.clone(),
    };
    receipt.evidence = owner.clone();
    let shared = DiagnosticInvestigationIntent::new_shared(
        episode,
        &recipe,
        focus,
        &component,
        &CollectionBudget::default(),
        DiagnosticSharedOwner { source: &owner_source, row: &owner_row, reference: &owner },
    )
    .unwrap();
    assert_eq!(database.episode_id, shared.episode_id);
    assert_ne!(database.episode_stage_key().unwrap(), shared.episode_stage_key().unwrap());
    (database, shared)
}

fn fixture_with_shared(shared: &DiagnosticInvestigationIntent) -> Fixture {
    let mut fixture = Fixture::new();
    let mut target = fixture.inventory.targets[0].clone();
    target.id = "shared-fixture".into();
    target.subject = shared.subject();
    target.schedules[0].collection = ScheduledCollection::Snapshot { dataset: SnapshotDataset::Operational };
    fixture.inventory.targets.push(target);
    fixture
}

async fn enroll_at(
    fixture: &Fixture,
    journal: &FleetJournal,
    scheduler: &mut Scheduler,
    intent: &DiagnosticInvestigationIntent,
    at: u64,
) -> FocusedJob {
    let target = fixture.inventory.targets.iter().find(|target| target.subject == intent.subject()).unwrap();
    let job = FocusedJob::new(intent, target, 60).unwrap();
    let update = enrollment(scheduler, job.clone(), at);
    scheduler.persisted(journal.enqueue_focused(intent.clone(), update).await.unwrap()).unwrap();
    job
}

async fn complete_at(journal: &FleetJournal, scheduler: &mut Scheduler, job: &FocusedJob, at: u64) {
    assert!(matches!(reserve(journal, scheduler, at).await, Work::Focused(actual) if actual == *job));
    finish(journal, scheduler, at, Outcome::FocusedComplete { result: result(&job.key) }).await;
}

async fn retire_at(journal: &FleetJournal, scheduler: &mut Scheduler, job: &FocusedJob, at: u64) {
    let update = scheduler.retire_focused(&job.key, elapsed(at), wall(at as i64)).unwrap();
    scheduler.persisted(journal.retire_focused(update).await.unwrap()).unwrap();
}

fn synthetic_receipt(episode: &str, stage: usize) -> FocusedReceipt {
    FocusedReceipt {
        key: digest(format!("owned-intent:{episode}:{stage}").as_bytes()),
        episode_id: episode.into(),
        episode_stage_key: digest(format!("owned-stage:{episode}:{stage}").as_bytes()),
        settlement: FocusedSettlement::Completed { result: result("owned-stage-result") },
    }
}

fn insert_receipt(connection: &Connection, receipt: &FocusedReceipt) {
    assert!(receipt.is_valid());
    connection
        .execute(
            "INSERT INTO investigation_receipts VALUES(?1,?2,?3,?4)",
            params![
                receipt.key,
                receipt.episode_id,
                receipt.episode_stage_key,
                super::super::super::scheduler::encode(receipt).unwrap()
            ],
        )
        .unwrap();
}

#[tokio::test]
async fn completed_database_and_shared_stages_survive_a_failed_retirement_restart_and_exact_replay() {
    let (database, shared) = paired_intents();
    let fixture = fixture_with_shared(&shared);
    let (journal, mut scheduler) = fixture.initialize().await;
    baseline(&journal, &mut scheduler).await;
    assert!(matches!(reserve(&journal, &mut scheduler, 60).await, Work::Scheduled(_)));
    finish(&journal, &mut scheduler, 60, Outcome::Complete).await;
    let database_job = enroll_at(&fixture, &journal, &mut scheduler, &database, 60).await;
    complete_at(&journal, &mut scheduler, &database_job, 120).await;
    retire_at(&journal, &mut scheduler, &database_job, 120).await;
    let database_stage = database.episode_stage_key().unwrap();
    let database_receipt = journal.focused_receipt(&database_stage).await.unwrap().unwrap();

    let shared_job = enroll_at(&fixture, &journal, &mut scheduler, &shared, 120).await;
    complete_at(&journal, &mut scheduler, &shared_job, 180).await;
    let before = journal.scheduler().await.unwrap().unwrap();
    fixture.connection().execute_batch("CREATE TRIGGER reject_retirement BEFORE DELETE ON admission_focused BEGIN SELECT RAISE(ABORT,'owned retirement fault'); END;").unwrap();
    let update = scheduler.retire_focused(&shared_job.key, elapsed(180), wall(180)).unwrap();
    assert!(journal.retire_focused(update).await.is_err());
    assert_eq!(journal.focused_receipt(&database_stage).await.unwrap(), Some(database_receipt.clone()));
    assert!(journal.focused_receipt(&shared.episode_stage_key().unwrap()).await.unwrap().is_none());
    assert_eq!(journal.scheduler().await.unwrap().unwrap().clock.revision, before.clock.revision);
    assert_eq!(journal.focused_intent(&shared_job.key).await.unwrap(), Some(shared.clone()));
    drop(journal);

    fixture.connection().execute_batch("DROP TRIGGER reject_retirement").unwrap();
    let (journal, mut scheduler) = fixture.reopen(180).await;
    let update = scheduler.retire_focused(&shared_job.key, Duration::ZERO, wall(180)).unwrap();
    scheduler.persisted(journal.retire_focused(update).await.unwrap()).unwrap();
    let shared_stage = shared.episode_stage_key().unwrap();
    let shared_receipt = journal.focused_receipt(&shared_stage).await.unwrap().unwrap();
    assert_ne!(shared_receipt.key, database_receipt.key);
    assert_eq!(shared_receipt.episode_id, database_receipt.episode_id);
    assert_eq!(journal.focused_receipt(&database_stage).await.unwrap(), Some(database_receipt.clone()));
    assert_eq!(journal.focused_receipt_episode_ids().await.unwrap(), [database.episode_id.clone()].into());
    drop(journal);

    for original in [&database, &shared] {
        let (journal, mut scheduler) = fixture.reopen(180).await;
        let before = journal.scheduler().await.unwrap().unwrap();
        let mut newer = (*original).clone();
        newer.episode_revision = "2".into();
        let target = fixture.inventory.targets.iter().find(|target| target.subject == newer.subject()).unwrap();
        let update = enrollment(&mut scheduler, FocusedJob::new(&newer, target, 60).unwrap(), 180);
        assert!(journal.enqueue_focused(newer, update).await.is_err(), "each completed physical stage stays completed");
        assert_eq!(journal.scheduler().await.unwrap().unwrap().clock.revision, before.clock.revision);
        assert_eq!(journal.focused_receipt(&database_stage).await.unwrap(), Some(database_receipt.clone()));
        assert_eq!(journal.focused_receipt(&shared_stage).await.unwrap(), Some(shared_receipt.clone()));
    }
    let (journal, _) = fixture.reopen(180).await;
    journal.prune_focused_receipts(&[database.episode_id.clone()].into()).await.unwrap();
    assert_eq!(journal.focused_receipt_episode_ids().await.unwrap().len(), 1);
    journal.prune_focused_receipts(&BTreeSet::new()).await.unwrap();
    assert!(journal.focused_receipt(&database_stage).await.unwrap().is_none());
    assert!(journal.focused_receipt(&shared_stage).await.unwrap().is_none());
}

#[tokio::test]
async fn outstanding_shared_stage_still_excludes_a_competing_database_intent_for_the_same_episode() {
    let (database, shared) = paired_intents();
    let fixture = fixture_with_shared(&shared);
    let (journal, mut scheduler) = fixture.initialize().await;
    let before = journal.scheduler().await.unwrap().unwrap();
    enroll_at(&fixture, &journal, &mut scheduler, &shared, 0).await;
    let saved = journal.scheduler().await.unwrap().unwrap();
    let mut stale = Scheduler::new(&fixture.inventory, "one").unwrap();
    stale.restore(before, Duration::ZERO, wall(0)).unwrap();
    let competing = enrollment(&mut stale, fixture.focused_job(&database), 0);
    assert!(journal.enqueue_focused(database, competing).await.is_err());
    assert_eq!(journal.scheduler().await.unwrap().unwrap().clock.revision, saved.clock.revision);
    assert_eq!(journal.focused_intents().await.unwrap(), vec![shared]);
}

#[tokio::test]
async fn receipt_capacity_bounds_each_episode_distinct_episodes_and_total_stages() {
    let fixture = Fixture::new();
    let (journal, _) = fixture.initialize().await;
    let mut connection = fixture.connection();
    let transaction = connection.transaction().unwrap();
    let first = digest(b"first bounded episode");
    insert_receipt(&transaction, &synthetic_receipt(&first, 0));
    insert_receipt(&transaction, &synthetic_receipt(&first, 1));
    assert!(require_receipt_capacity(&transaction, &first).is_err());
    for index in 1..MAX_DIAGNOSTIC_EPISODES {
        insert_receipt(&transaction, &synthetic_receipt(&digest(format!("episode:{index}").as_bytes()), 0));
    }
    assert!(require_receipt_capacity(&transaction, &digest(b"one episode beyond the cap")).is_err());
    let remaining = digest(b"episode:1");
    assert!(require_receipt_capacity(&transaction, &remaining).is_ok(), "a known episode may use its second stage");
    for index in 1..MAX_DIAGNOSTIC_EPISODES {
        insert_receipt(&transaction, &synthetic_receipt(&digest(format!("episode:{index}").as_bytes()), 1));
    }
    assert_eq!(receipts(&transaction).unwrap().len(), MAX_RECEIPTS);
    assert!(require_receipt_capacity(&transaction, &digest(b"over the total cap")).is_err());
    transaction.commit().unwrap();
    assert_eq!(journal.focused_receipt_episode_ids().await.unwrap().len(), MAX_DIAGNOSTIC_EPISODES);
    journal.prune_focused_receipts(&[first.clone()].into()).await.unwrap();
    assert_eq!(journal.focused_receipt_episode_ids().await.unwrap(), [first].into());
    assert_eq!(receipts(&connection).unwrap().len(), MAX_FOCUSED_STAGES_PER_EPISODE);
}

#[tokio::test]
async fn cancelled_stage_replacement_reuses_its_slot_without_erasing_the_other_completed_target() {
    let fixture = Fixture::new();
    let (journal, mut scheduler) = fixture.initialize().await;
    let intent = investigation_fixture().intent;
    let job = enqueue(&fixture, &journal, &mut scheduler, &intent).await;
    let cancelled = scheduler.cancel_focused(&job.key, Duration::ZERO, wall(0)).unwrap();
    scheduler
        .persisted(journal.cancel_focused(cancelled, DiagnosticAnalysisIssue::SourceChanged).await.unwrap())
        .unwrap();
    let other = synthetic_receipt(&intent.episode_id, 1);
    insert_receipt(&fixture.connection(), &other);
    assert_eq!(receipts(&fixture.connection()).unwrap().len(), MAX_FOCUSED_STAGES_PER_EPISODE);
    let mut replacement = intent.clone();
    replacement.recipe_version += 1;
    replacement.qualification.recipe_version = replacement.recipe_version;
    enqueue(&fixture, &journal, &mut scheduler, &replacement).await;
    assert_eq!(journal.focused_receipt(&other.episode_stage_key).await.unwrap(), Some(other));
    assert!(matches!(
        journal.focused_stage(&intent.episode_stage_key().unwrap()).await.unwrap(),
        Some(FocusedStage::Outstanding)
    ));
    assert_eq!(journal.focused_receipt_episode_ids().await.unwrap(), [intent.episode_id].into());
}

#[tokio::test]
async fn an_episode_stage_capacity_refusal_cannot_partially_enroll_or_advance_admission() {
    let fixture = Fixture::new();
    let (journal, mut scheduler) = fixture.initialize().await;
    let intent = investigation_fixture().intent;
    let connection = fixture.connection();
    for stage in 0..MAX_FOCUSED_STAGES_PER_EPISODE {
        insert_receipt(&connection, &synthetic_receipt(&intent.episode_id, stage));
    }
    let before = journal.scheduler().await.unwrap().unwrap();
    let receipts_before = receipts(&connection).unwrap();
    let update = enrollment(&mut scheduler, fixture.focused_job(&intent), 0);
    let error = journal.enqueue_focused(intent, update).await.unwrap_err();
    assert_eq!(error.detail, Some(culprit_core::ErrorDetail::Delivery(DeliveryProblem::Capacity)));
    let after = journal.scheduler().await.unwrap().unwrap();
    assert_eq!(after.clock.revision, before.clock.revision);
    assert!(after.focused.is_empty() && after.focused_families.is_empty());
    assert_eq!(serde_json::to_value(after.resources).unwrap(), serde_json::to_value(before.resources).unwrap());
    assert!(journal.focused_intents().await.unwrap().is_empty());
    assert_eq!(receipts(&connection).unwrap(), receipts_before);
}

#[tokio::test]
async fn an_old_journal_version_is_rejected_without_rewriting_its_receipts_or_schema() {
    let fixture = Fixture::new();
    let (journal, _) = fixture.initialize().await;
    let connection = fixture.connection();
    assert_eq!(
        connection.pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0)).unwrap(),
        JOURNAL_VERSION
    );
    let receipt = synthetic_receipt(&digest(b"old journal episode"), 0);
    insert_receipt(&connection, &receipt);
    connection.pragma_update(None, "user_version", JOURNAL_VERSION - 1).unwrap();
    drop(journal);
    assert!(FleetJournal::open(&fixture.directory.path().join("fleet"), &fixture.inventory, "one").await.is_err());
    assert_eq!(
        connection.pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0)).unwrap(),
        JOURNAL_VERSION - 1
    );
    assert_eq!(receipts(&connection).unwrap(), vec![receipt]);
}

#[tokio::test]
async fn an_overflowing_episode_stage_inventory_fails_closed_without_partial_pruning() {
    let fixture = Fixture::new();
    let (journal, _) = fixture.initialize().await;
    let connection = fixture.connection();
    let episode = digest(b"corrupt overfull stage inventory");
    for stage in 0..=MAX_FOCUSED_STAGES_PER_EPISODE {
        insert_receipt(&connection, &synthetic_receipt(&episode, stage));
    }
    assert!(journal.focused_receipt_episode_ids().await.is_err());
    assert!(journal.prune_focused_receipts(&BTreeSet::new()).await.is_err());
    assert_eq!(
        connection.query_row("SELECT count(*) FROM investigation_receipts", [], |row| row.get::<_, u32>(0)).unwrap(),
        u32::try_from(MAX_FOCUSED_STAGES_PER_EPISODE + 1).unwrap()
    );
}
