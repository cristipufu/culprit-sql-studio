use super::*;

fn local_partition() -> SourcePartition {
    SourcePartition::InstanceLocal {
        member: Some(SharedResourceIdentity {
            engine: Engine::Postgres,
            kind: SharedResourceKind::Instance,
            namespace: NativeIdentity::new("controlled-native-postgres-lineage"),
            native: NativeIdentity::new("controlled-native-postgres-member"),
        }),
        epoch: Some("native-process".into()),
    }
}
fn original(qualified: bool) -> Frame {
    let mut waiter = activity_observation(DefinitionPayload::sql("SELECT id FROM orders", Engine::Postgres));
    waiter.key = ActivityKey {
        session: NativeActivityIdentity { id: "42".into(), generation: "2020-01-01T11:00:00Z".into() },
        request: Some(NativeActivityIdentity { id: "statement".into(), generation: "2020-01-01T11:59:20Z".into() }),
        transaction: None,
    };
    waiter.scope = ActivityScopeBasis::SessionDatabase;
    waiter.command = None;
    waiter.state = "active".into();
    waiter.statement_identity = Some(NativeIdentity::new("controlled-native-pg-statement"));
    waiter.elapsed_ms = Some("40000".into());
    waiter.session_counters = ActivityCounters::default();
    waiter.request_counters = Some(ActivityCounters::default());
    waiter.blocking_status = ActivityBlockingStatus::Unknown;
    waiter.waiting_on_lock = Some(true);
    waiter.wait = Some(ActivityWait {
        class: None,
        identified_resources: vec![],
        native_type: Some("Lock: relation".into()),
        elapsed_ms: None,
        owners: vec![],
        resources: None,
    });
    let mut frame = baseline(0, vec![waiter]);
    frame.source.target =
        culprit_core::DatabaseIdentity::server(Engine::Postgres, "db", "fixture.invalid", 5432, None).into();
    frame.source.partition = if qualified { local_partition() } else { SourcePartition::Target };
    frame.source.source = "pg_stat_activity".into();
    let SourceObservation::Activity(native) = &mut frame.source.observation else { unreachable!() };
    native.epoch = Some("native-process".into());
    native.database_writable = Some(true);
    native.coverage.source = frame.source.source.clone();
    native.population = ActivityPopulation::ClientBackends;
    frame
}

fn lock_episode(checkpoint: &DiagnosticCheckpoint) -> &DiagnosticEpisode {
    let mut episodes =
        checkpoint.episodes.iter().filter(|episode| matches!(episode.signal, DiagnosticSignal::LockWait { .. }));
    let episode = episodes.next().expect("the native heavyweight-lock sensor creates one exact request episode");
    assert!(episodes.next().is_none());
    episode
}
async fn focus(harness: &Harness, captured: &CaptureIdentity) -> ActivityRequestFocus {
    let input = harness.outbox.diagnostic_input(&captured.observation_id, DIAGNOSTIC_CONTROLLER_VERSION).await.unwrap();
    let [row] = input.rows.as_slice() else { panic!("one exact original request") };
    let TelemetryRecord::Activity(original) = &row.row.record else { panic!() };
    let native = input.source.observation.activity().unwrap();
    let focus = ActivityRequestFocus {
        parent: row.reference.clone(),
        target: input.source.target.clone(),
        partition: input.source.partition.clone(),
        database_writable: native.database_writable,
        source: input.source.source.clone(),
        epoch: native.epoch.clone().unwrap(),
        original_observed_at: native.at.clone(),
        original: original.as_ref().clone(),
        statement_identity: original.statement_identity.clone().unwrap(),
    };
    assert!(focus.matches_parent(&input.source, original, &row.reference));
    focus
}
fn observed(focus: ActivityRequestFocus, end: BlockingRequestState) -> Frame {
    let mut waiter = focus.original.clone();
    waiter.blocking_status = ActivityBlockingStatus::Known;
    waiter.elapsed_ms = Some("50000".into());
    let mut owner = waiter.clone();
    owner.key.session.id = "43".into();
    owner.key.request = None;
    owner.key.transaction = Some("2020-01-01T11:58:00Z".into());
    owner.idle = true;
    owner.state = "idle in transaction".into();
    owner.open_transactions = Some(1);
    owner.transaction_age_ms = Some("120000".into());
    owner.statement_identity = None;
    owner.request_counters = None;
    owner.statement = DefinitionPayload::Unavailable { reason: PayloadUnavailable::NotCollected };
    owner.wait = None;
    owner.waiting_on_lock = None;
    waiter.wait.as_mut().unwrap().owners = vec![ActivityWaitOwner::Session {
        id: owner.key.session.id.clone(),
        generation: Some(owner.key.session.generation.clone()),
    }];
    let result = ActivityBlockingFocusResult {
        binding: if focus.partition == SourcePartition::Target {
            FocusedRequestBinding::UnverifiedOriginalScope
        } else {
            FocusedRequestBinding::Matched
        },
        focus,
        membership: FocusedRequestMembership::Observed,
        request_end: end,
        closure: BlockingClosureDescription {
            status: BlockingClosureStatus::Observed,
            max_lookups: MAX_BLOCKING_LOOKUPS,
            max_depth: MAX_BLOCKING_DEPTH,
            max_participants: MAX_BLOCKING_PARTICIPANTS,
        },
    };
    Frame {
        source: CollectionDescription {
            target: result.focus.target.clone(),
            partition: local_partition(),
            source: "postgres-blocking".into(),
            continuity: SourceContinuity::Unverified,
            observation: SourceObservation::FocusedBlocking {
                snapshot: ActivitySnapshotDescription {
                    epoch: Some("native-process".into()),
                    database_writable: Some(true),
                    observed_from: clock(10),
                    observed_until: clock(11),
                    at: clock(10),
                    scope: culprit_core::MeasurementScope::Database,
                    population: ActivityPopulation::ActiveRequestsOpenTransactionsAndBlockers,
                    native_rows: 2,
                    observations: 2,
                    coverage: StateSourceCoverage::observed("postgres-blocking".into(), 2, false),
                },
                result: Box::new(result),
            },
        },
        records: vec![waiter.into(), owner.into()],
    }
}
fn policies() -> DiagnosticPolicies {
    DiagnosticPolicies::for_activity("fixture", CollectionSubject::Database, ActivityPolicy::default())
        .unwrap()
        .with_recipes(
            "fixture",
            vec![RecipeDescriptor {
                id: "postgres.blocking".into(),
                version: 1,
                purpose: RecipePurpose::Investigation,
                selector: RecipeSelector::FocusedBlocking,
                subjects: vec![RecipeSubject::Database],
                budget: CollectionBudget::default(),
                cost: RecipeCostQualification::Unqualified,
            }],
        )
        .unwrap()
}

#[tokio::test]
async fn a_real_sealed_qualified_parent_defers_cost_then_a_single_explicit_closure_preserves_all_history_and_pins() {
    let mut harness = Harness::new().await;
    harness.policies = policies();
    let baseline = harness.capture(original(true)).await;
    assert_eq!(harness.work(1).await, 1);
    let before = lock_episode(&harness.checkpoint().await).clone();
    assert_eq!(before.issue, Some(DiagnosticAnalysisIssue::CostUnqualified));
    let focus = focus(&harness, &baseline).await;
    assert_eq!(focus.original.waiting_on_lock, Some(true));
    assert_eq!(
        focus.original.wait.as_ref().unwrap().elapsed_ms,
        None,
        "the metadata baseline does not measure wait duration"
    );
    assert!(
        focus.original.conditions(&ActivityPolicy::default()).is_empty(),
        "a request age is not a measured lock-wait age"
    );
    let original_evidence = harness.outbox.diagnostic_evidence(&focus.parent).await.unwrap();
    let selected = harness.capture(observed(focus.clone(), BlockingRequestState::SameStatement)).await;
    assert_eq!(harness.work(1).await, 1);
    let after = lock_episode(&harness.checkpoint().await).clone();
    assert_eq!(after.id, before.id);
    assert_eq!(after.first, focus.parent);
    assert_eq!(after.analysis, DiagnosticAnalysisStatus::Analyzed);
    assert_eq!(after.lifecycle, DiagnosticLifecycle::Active);
    assert!(after.recovery.is_none());
    let report = after.report.as_ref().unwrap();
    assert_eq!(report.references().count(), 2);
    assert_eq!(report.evidence().capture, selected);
    let revisions = harness.revisions(&selected).await;
    assert_eq!(revisions.len(), 1);
    assert!(report.references().all(|reference| revisions[0].inputs.contains(reference)));
    assert!(revisions[0].inputs.contains(&focus.parent));
    assert_eq!(harness.outbox.diagnostic_evidence(&focus.parent).await.unwrap(), original_evidence);
    let calls = harness.calls.counts();
    assert_eq!(harness.work(10).await, 0);
    drop(harness.outbox);
    let mut outbox = Outbox::open(&harness.path).unwrap();
    outbox.recover().await.unwrap();
    assert_eq!(
        analyze_pending(&mut outbox, &harness.policies, &CollectionCancellation::default(), 10, None).await.unwrap(),
        0
    );
    let restored = outbox.diagnostic_checkpoints(DIAGNOSTIC_CONTROLLER_VERSION, None, 10).await.unwrap();
    assert_eq!(lock_episode(&restored.checkpoints[0]), &after);
    assert_eq!(outbox.diagnostic_evidence(&focus.parent).await.unwrap(), original_evidence);
    assert_eq!(harness.calls.counts(), calls, "restart cannot open a new native source to repair the original");
}

#[tokio::test]
async fn current_member_authority_and_a_final_disappearance_never_upgrade_or_resolve_the_original_incident() {
    for qualified in [false, true] {
        let mut harness = Harness::new().await;
        let baseline = harness.capture(original(qualified)).await;
        assert_eq!(harness.work(1).await, 1);
        let before = lock_episode(&harness.checkpoint().await).clone();
        let focus = focus(&harness, &baseline).await;
        let selected = harness.capture(observed(focus.clone(), BlockingRequestState::Missing)).await;
        assert_eq!(harness.work(1).await, 1);
        let after = lock_episode(&harness.checkpoint().await).clone();
        assert_eq!(after.id, before.id);
        assert_eq!(after.scope, before.scope);
        assert_eq!(after.first, focus.parent);
        assert_eq!(after.lifecycle, DiagnosticLifecycle::Active);
        assert_eq!(after.analysis, DiagnosticAnalysisStatus::Partial);
        assert!(after.recovery.is_none());
        assert_eq!(after.report.is_some(), qualified);
        let input =
            harness.outbox.diagnostic_input(&selected.observation_id, DIAGNOSTIC_CONTROLLER_VERSION).await.unwrap();
        assert_eq!(input.rows.len(), 2, "independently useful current raw facts survive either original-scope outcome");
        assert_eq!(harness.calls.counts(), [2, 2, 2, 2]);
    }
}

#[tokio::test]
async fn an_explicit_current_frame_without_a_prior_controller_checkpoint_does_not_create_a_retroactive_episode() {
    let mut harness = Harness::new().await;
    let original = harness.capture(original(true)).await;
    let focus = focus(&harness, &original).await;
    let captured = harness.capture(observed(focus, BlockingRequestState::SameStatement)).await;
    let input = harness.outbox.diagnostic_input(&captured.observation_id, DIAGNOSTIC_CONTROLLER_VERSION).await.unwrap();
    assert!(input.checkpoint.is_none());
    let before = harness.calls.counts();
    let update = analyze(&input, &ActivityPolicy::default(), &clock(20)).unwrap();
    assert!(update.revisions.is_empty() && update.checkpoint.episodes.is_empty());
    assert_eq!(input.rows.len(), 2, "the original native closure is retained independently of controller history");
    assert_eq!(harness.calls.counts(), before);
}
