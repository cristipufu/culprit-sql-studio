//! Synthetic sealed-row consistency tests do not establish native cost qualification or storage authenticity.
use super::*;

struct SharedScenario {
    scenario: Scenario,
    checkpoint: DiagnosticCheckpoint,
    owner_source: CollectionDescription,
    owner_row: ExportedRecord,
    owner: DiagnosticEvidenceRef,
    member: SharedResourceIdentity,
}

impl SharedScenario {
    fn intent(&self) -> DiagnosticInvestigationIntent {
        DiagnosticInvestigationIntent::new_shared(
            &self.scenario.episode,
            &self.scenario.recipe,
            self.scenario.focus.clone().into(),
            &self.scenario.component,
            &CollectionBudget::default(),
            DiagnosticSharedOwner { source: &self.owner_source, row: &self.owner_row, reference: &self.owner },
        )
        .unwrap()
    }
}

type Frame = (CollectionDescription, DiagnosticCaptureRef, Vec<(DiagnosticEvidenceRef, ActivityObservation)>);

fn baseline(sequence: u64, present: bool, member: &SharedResourceIdentity) -> Frame {
    let mut observation = actor();
    observation.wait.as_mut().unwrap().identified_resources[0].native.id = "2:1:99".into();
    let (mut source, mut input, mut rows) = frame(sequence, if present { vec![observation] } else { vec![] }, true);
    source.partition =
        SourcePartition::InstanceLocal { member: Some(member.clone()), epoch: Some("native-epoch".into()) };
    let SourceObservation::Activity(native) = &mut source.observation else { panic!() };
    native.database_writable = Some(true);
    input.source_scope_id = source.captured_range().unwrap().scope_id;
    for (reference, observation) in &mut rows {
        let row = ExportedRecord::new(&source, &input.capture, TelemetryRecord::from(observation.clone())).unwrap();
        *reference = DiagnosticEvidenceRef::from_export(&source, &input.manifest_sha256, &row, 0).unwrap();
    }
    (source, input, rows)
}

fn shared_scenario() -> SharedScenario {
    let mut scenario = scenario();
    let member = SharedResourceIdentity {
        engine: Engine::Mssql,
        kind: SharedResourceKind::Instance,
        namespace: NativeIdentity::new("native-machine"),
        native: NativeIdentity::new("native-instance"),
    };
    let observed = observe(&baseline(1, true, &member), None);
    scenario.episode = observed.checkpoint.episodes[0].clone();
    scenario.focus = observed.focuses[0].1.page_wait().unwrap().clone();
    let partition = SourcePartition::InstanceLocal {
        member: Some(member.clone()),
        epoch: Some("independent-instance-epoch".into()),
    };
    let owner_source = CollectionDescription {
        target: CollectionTarget::Shared(member.clone()),
        partition: partition.clone(),
        source: "shared-native-operational".into(),
        continuity: SourceContinuity::Unverified,
        observation: SourceObservation::Snapshot(StateSnapshotDescription {
            dataset: SnapshotDataset::Operational,
            observed_from: "2020-01-01T00:01:30Z".into(),
            observed_until: "2020-01-01T00:01:31Z".into(),
            records: 1,
            sources: vec![StateSourceCoverage::collected("shared-native-operational".into(), 1)],
        }),
    };
    let owner_capture = CaptureIdentity {
        observation_id: "shared-owner-read".into(),
        revision: Some(CaptureRevision {
            outbox_id: "owner-outbox".into(),
            owner_epoch: "1".into(),
            sequence: "2".into(),
        }),
    };
    let counters = NativeCounterSet {
        identity: NativeIdentity::new("native-shared-observer"),
        scope: MeasurementScope::Server,
        counters: vec![NativeCounterSample::gauge(
            "native_reads",
            12,
            CounterUnit::Count,
            MeasurementScope::Server,
            "native-observer",
            "native_reads",
        )],
        properties: Default::default(),
    };
    let owner_row = ExportedRecord::new(
        &owner_source,
        &owner_capture,
        TelemetryRecord::Operational(OperationalObservation {
            source: owner_source.source.clone(),
            key: counters.key().unwrap(),
            fact: OperationalFact::CounterSet(Box::new(counters)),
        }),
    )
    .unwrap();
    let owner =
        DiagnosticEvidenceRef::from_export(&owner_source, &digest(b"sealed-shared-owner"), &owner_row, 0).unwrap();
    // The independently retained synthetic experiment has a different source and exact receipt.
    scenario.cost_source.target = owner_source.target.clone();
    scenario.cost_source.partition = partition.clone();
    let mut cost_record = owner_row.record.clone();
    let TelemetryRecord::Operational(cost) = &mut cost_record else { panic!() };
    cost.source = scenario.cost_source.source.clone();
    scenario.cost_row = ExportedRecord::new(&scenario.cost_source, &scenario.cost_row.capture, cost_record).unwrap();
    let cost_ref = DiagnosticEvidenceRef::from_export(
        &scenario.cost_source,
        &digest(b"sealed-shared-cost"),
        &scenario.cost_row,
        0,
    )
    .unwrap();
    let receipt = scenario.receipt();
    receipt.scope =
        RecipeCostScope::SharedResource { target: member.clone(), partition, source: owner_source.source.clone() };
    receipt.evidence = cost_ref;
    scenario.recipe.subjects = vec![RecipeSubject::Shared(SharedResourceKind::Instance)];
    SharedScenario { scenario, checkpoint: observed.checkpoint, owner_source, owner_row, owner, member }
}

fn result(
    shared: &SharedScenario,
    intent: &DiagnosticInvestigationIntent,
) -> (CollectionDescription, DiagnosticCaptureRef) {
    let (mut source, mut capture, _) = focus_frame(
        intent.focus.page_wait().unwrap().clone(),
        PageWaitRequestState::Missing,
        PageWaitRelation::Unqualified,
        vec![],
    );
    source.target = shared.owner_source.target.clone();
    source.partition = shared.owner_source.partition.clone();
    capture.source_scope_id = source.captured_range().unwrap().scope_id;
    (source, capture)
}

#[test]
fn shared_cost_scope_retains_native_owner_without_inventing_activity_population_or_equating_epochs() {
    let shared = shared_scenario();
    let intent = shared.intent();
    assert!(intent.is_valid() && intent.matches_episode(&shared.scenario.episode));
    assert!(intent.matches_shared_owner(&shared.owner_source, &shared.owner_row));
    assert!(intent.qualification.scope.as_activity().is_none());
    assert_ne!(intent.qualification.scope.native_epoch(), Some(intent.focus.participant().epoch.as_str()));
    assert_eq!(intent.focus.participant().target, shared.scenario.episode.scope.target);
    assert_eq!(intent.subject(), CollectionSubject::Shared { resource: shared.member });
    assert_eq!(intent.qualification.scope.target(), shared.owner_source.target);
    assert!(
        DiagnosticInvestigationIntent::new(
            &shared.scenario.episode,
            &shared.scenario.recipe,
            shared.scenario.focus.clone().into(),
            &shared.scenario.component,
            &CollectionBudget::default()
        )
        .is_err()
    );
}

#[test]
fn exact_positive_owner_row_is_required_and_tampered_receipts_descriptors_or_empty_sources_fail() {
    let shared = shared_scenario();
    let original = shared.intent();
    for mutation in ["target", "epoch", "source", "dataset", "empty", "unavailable", "payload", "ordinal", "hash"] {
        let mut source = shared.owner_source.clone();
        let mut row = shared.owner_row.clone();
        let mut reference = shared.owner.clone();
        match mutation {
            "target" => source.target = shared.scenario.focus.participant().target.clone(),
            "epoch" => {
                source.partition = SourcePartition::InstanceLocal {
                    member: Some(shared.member.clone()),
                    epoch: Some("restarted-instance".into()),
                }
            }
            "source" => source.source = "other-shared-source".into(),
            "payload" => row.record_id = Some(digest(b"foreign row")),
            "ordinal" => reference.row_ordinal += 1,
            "hash" => reference.manifest_sha256 = digest(b"foreign manifest"),
            _ => {
                let SourceObservation::Snapshot(snapshot) = &mut source.observation else { panic!() };
                match mutation {
                    "dataset" => snapshot.dataset = SnapshotDataset::MemoryFailures,
                    "empty" => {
                        snapshot.records = 0;
                        snapshot.sources[0] = StateSourceCoverage::collected(snapshot.sources[0].source.clone(), 0);
                    }
                    _ => {
                        snapshot.records = 0;
                        snapshot.sources[0] = StateSourceCoverage::unavailable(
                            snapshot.sources[0].source.clone(),
                            crate::ErrorCode::PermissionDenied,
                            None,
                        );
                    }
                }
            }
        }
        if !matches!(mutation, "ordinal" | "hash") {
            assert!(!original.matches_shared_owner(&source, &row), "{mutation}");
        }
        let changed = DiagnosticInvestigationIntent::new_shared(
            &shared.scenario.episode,
            &shared.scenario.recipe,
            shared.scenario.focus.clone().into(),
            &shared.scenario.component,
            &CollectionBudget::default(),
            DiagnosticSharedOwner { source: &source, row: &row, reference: &reference },
        );
        if mutation == "hash" {
            let changed = changed.unwrap();
            assert_ne!(
                changed.key().unwrap(),
                original.key().unwrap(),
                "storage must independently authenticate the new receipt"
            );
        } else {
            assert!(changed.is_err(), "{mutation}");
        }
    }
}

#[test]
fn a_partial_positive_owner_is_usable_but_unknown_owner_or_another_member_cannot_authorize_a_shared_question() {
    let mut shared = shared_scenario();
    let SourceObservation::Snapshot(snapshot) = &mut shared.owner_source.observation else { panic!() };
    snapshot.sources[0] = StateSourceCoverage::observed(snapshot.sources[0].source.clone(), 1, false);
    // Counts/coverage are authenticated in the descriptor; this test issues the corresponding fixture reference.
    shared.owner = DiagnosticEvidenceRef::from_export(
        &shared.owner_source,
        &digest(b"sealed-partial-owner"),
        &shared.owner_row,
        0,
    )
    .unwrap();
    assert!(shared.intent().matches_shared_owner(&shared.owner_source, &shared.owner_row));
    for unknown in [false, true] {
        let mut recipe = shared.scenario.recipe.clone();
        let RecipeCostQualification::Measured(receipt) = &mut recipe.cost else { panic!() };
        let RecipeCostScope::SharedResource { target, partition, .. } = &mut receipt.scope else { panic!() };
        if unknown {
            *partition = SourcePartition::InstanceLocal { member: None, epoch: None };
        } else {
            target.native.id = "another-instance".into();
            *partition =
                SourcePartition::InstanceLocal { member: Some(target.clone()), epoch: Some("other-epoch".into()) };
        }
        assert!(
            DiagnosticInvestigationIntent::new_shared(
                &shared.scenario.episode,
                &recipe,
                shared.scenario.focus.clone().into(),
                &shared.scenario.component,
                &CollectionBudget::default(),
                DiagnosticSharedOwner {
                    source: &shared.owner_source,
                    row: &shared.owner_row,
                    reference: &shared.owner
                }
            )
            .is_err()
        );
    }
}

#[test]
fn shared_lineage_preserves_exact_selection_subject_budget_and_owner_without_an_id_only_request() {
    let shared = shared_scenario();
    let intent = shared.intent();
    let lineage = DiagnosticInvestigationLineage::from_intent(&intent).unwrap();
    let request =
        CollectionRequest { subject: intent.subject(), selection: intent.selection(), budget: intent.budget.clone() };
    request.validate().unwrap();
    assert!(lineage.matches_request(&request));
    assert_eq!(lineage.reconstruct(&intent.focus).unwrap().key().unwrap(), intent.key().unwrap());
    let restored: DiagnosticInvestigationLineage =
        serde_json::from_slice(&serde_json::to_vec(&lineage).unwrap()).unwrap();
    assert_eq!(restored.reconstruct(&intent.focus).unwrap().execution, intent.execution);
    for mutation in ["database-selection", "subject", "epoch", "budget"] {
        let mut changed = request.clone();
        match mutation {
            "database-selection" => {
                changed.selection =
                    CollectionSelection::FocusedPageWait { focus: Box::new(intent.focus.page_wait().unwrap().clone()) }
            }
            "subject" => changed.subject = CollectionSubject::Database,
            "epoch" => {
                let CollectionSelection::SharedPageWaitFocus { partition, .. } = &mut changed.selection else {
                    panic!()
                };
                *partition = SourcePartition::InstanceLocal {
                    member: Some(shared.member.clone()),
                    epoch: Some("other-epoch".into()),
                };
            }
            _ => changed.budget.max_queries += 1,
        }
        assert!(!lineage.matches_request(&changed), "{mutation}");
    }
    let mut other = intent.clone();
    let DiagnosticExecutionBinding::Shared { owner } = &mut other.execution else { panic!() };
    owner.row_ordinal += 1;
    assert_ne!(other.key().unwrap(), intent.key().unwrap());
    assert_eq!(other.episode_stage_key().unwrap(), intent.episode_stage_key().unwrap());
    let mut database_recipe = shared.scenario.recipe.clone();
    database_recipe.subjects = vec![RecipeSubject::Database];
    let RecipeCostQualification::Measured(receipt) = &mut database_recipe.cost else { panic!() };
    receipt.scope = shared.scenario.episode.scope.clone().into();
    let database = DiagnosticInvestigationIntent::new(
        &shared.scenario.episode,
        &database_recipe,
        intent.focus.clone(),
        &shared.scenario.component,
        &CollectionBudget::default(),
    )
    .unwrap();
    assert_ne!(
        database.episode_stage_key().unwrap(),
        intent.episode_stage_key().unwrap(),
        "a completed database question cannot block the independently admitted shared target stage"
    );
}

#[test]
fn shared_result_cannot_change_the_database_incident_without_exact_admitted_intent_and_execution_generation() {
    let shared = shared_scenario();
    let intent = shared.intent();
    let (source, capture) = result(&shared, &intent);
    assert!(intent.matches_result_source(&source));
    assert!(analyze_diagnostic_focus(&source, &capture, &[], &shared.checkpoint, "2020-01-01T01:00:00Z").is_err());
    let update = analyze_diagnostic_focus_with_intent(
        &source,
        &capture,
        &[],
        &shared.checkpoint,
        "2020-01-01T01:00:00Z",
        &intent,
    )
    .unwrap();
    assert_eq!(update.checkpoint.target, shared.scenario.focus.participant().target);
    assert!(update.checkpoint.episodes.is_empty());
    assert_eq!(update.revisions.len(), 1);
    let revision = &update.revisions[0];
    assert_eq!(revision.episode.lifecycle, DiagnosticLifecycle::Resolved);
    assert_eq!(revision.episode.analysis, DiagnosticAnalysisStatus::Unresolved);
    assert!(revision.inputs.contains(&intent.focus.participant().parent) && revision.inputs.contains(&shared.owner));
    assert_eq!(revision.episode.first, shared.scenario.episode.first);
    assert_eq!(revision.episode.latest, shared.scenario.episode.latest);
    let proof = revision.episode.recovery.as_ref().unwrap();
    assert!(!proof.matches_source(&revision.episode, &source, &capture));
    assert!(proof.matches_source_with_intent(&revision.episode, &source, &capture, &intent));
    for mutation in ["epoch", "focus", "episode"] {
        let mut changed_source = source.clone();
        let mut changed_intent = intent.clone();
        match mutation {
            "epoch" => {
                changed_source.partition = SourcePartition::InstanceLocal {
                    member: Some(shared.member.clone()),
                    epoch: Some("restarted-instance".into()),
                }
            }
            "focus" => {
                let SourceObservation::FocusedPageWait { result, .. } = &mut changed_source.observation else {
                    panic!()
                };
                result.focus.participant_mut().original.elapsed_ms = Some("2".into());
            }
            _ => changed_intent.episode_id = digest(b"unrelated-episode"),
        }
        if mutation == "episode" {
            let unchanged = analyze_diagnostic_focus_with_intent(
                &changed_source,
                &capture,
                &[],
                &shared.checkpoint,
                "2020-01-01T01:00:00Z",
                &changed_intent,
            )
            .unwrap();
            assert!(unchanged.revisions.is_empty());
            assert_eq!(unchanged.checkpoint.episodes, shared.checkpoint.episodes);
        } else {
            assert!(
                analyze_diagnostic_focus_with_intent(
                    &changed_source,
                    &capture,
                    &[],
                    &shared.checkpoint,
                    "2020-01-01T01:00:00Z",
                    &changed_intent
                )
                .is_err(),
                "{mutation}"
            );
        }
    }
}

#[test]
fn resolved_but_owed_shared_analysis_preserves_first_recovery_and_terminal_replay_changes_nothing() {
    let shared = shared_scenario();
    let intent = shared.intent();
    let recovery = observe(&baseline(2, false, &shared.member), Some(&shared.checkpoint));
    assert!(intent.matches_admission(
        &recovery.checkpoint.episodes[0],
        &shared.scenario.recipe,
        &shared.scenario.component,
        &CollectionBudget::default()
    ));
    let original_proof = recovery.checkpoint.episodes[0].recovery.clone();
    let (source, capture) = result(&shared, &intent);
    let update = analyze_diagnostic_focus_with_intent(
        &source,
        &capture,
        &[],
        &recovery.checkpoint,
        "2020-01-01T01:00:00Z",
        &intent,
    )
    .unwrap();
    assert_eq!(update.revisions[0].episode.recovery, original_proof);
    assert_eq!(update.revisions[0].episode.first, shared.scenario.episode.first);
    let mut terminal = shared.checkpoint.clone();
    terminal.episodes[0].analysis = DiagnosticAnalysisStatus::Unresolved;
    terminal.episodes[0].issue = Some(DiagnosticAnalysisIssue::EvidenceUnavailable);
    let replay =
        analyze_diagnostic_focus_with_intent(&source, &capture, &[], &terminal, "2020-01-01T01:00:00Z", &intent)
            .unwrap();
    assert!(replay.revisions.is_empty());
    assert_eq!(replay.checkpoint.episodes, terminal.episodes);
}

#[test]
fn delayed_middle_parent_keeps_exact_evidence_without_clearing_a_newer_positive_native_observation() {
    let mut shared = shared_scenario();
    let middle = baseline(2, true, &shared.member);
    let second = observe(&middle, Some(&shared.checkpoint));
    let third = observe(&baseline(3, true, &shared.member), Some(&second.checkpoint));
    shared.scenario.episode = third.checkpoint.episodes[0].clone();
    let PageWaitFocusPlan::Ready(focus) =
        shared.scenario.episode.plan_page_wait_focus(&middle.0, &middle.2[0].1, &middle.2[0].0).unwrap()
    else {
        panic!()
    };
    shared.scenario.focus = *focus;
    let intent = shared.intent();
    let (source, capture) = result(&shared, &intent);
    let update = analyze_diagnostic_focus_with_intent(
        &source,
        &capture,
        &[],
        &third.checkpoint,
        "2020-01-01T01:00:00Z",
        &intent,
    )
    .unwrap();
    let episode = &update.revisions[0].episode;
    assert_eq!(episode.lifecycle, DiagnosticLifecycle::Active);
    assert!(episode.recovery.is_none());
    assert_eq!(episode.latest, third.checkpoint.episodes[0].latest);
    assert!(update.revisions[0].inputs.contains(&middle.2[0].0));
    assert!(update.revisions[0].inputs.contains(&shared.owner));
}

#[test]
fn admitted_shared_recovery_proof_rejects_delayed_and_malformed_native_upper_bounds() {
    let shared = shared_scenario();
    let intent = shared.intent();
    let newer = observe(&baseline(3, true, &shared.member), Some(&shared.checkpoint));
    let episode = &newer.checkpoint.episodes[0];
    let (mut source, capture) = result(&shared, &intent);
    let proof = DiagnosticRecoveryProof::FocusedParticipantGone {
        capture: capture.clone(),
        parent: Box::new(intent.focus.participant().parent.clone()),
    };
    assert!(intent.matches_result_source(&source), "a valid admitted result is still too early to establish recovery");
    assert!(!proof.matches_source_with_intent(episode, &source, &capture, &intent));
    for upper_bound in [episode.last_seen_at.as_str(), "2020-01-01T00:04:02Z"] {
        let SourceObservation::FocusedPageWait { result, snapshot } = &mut source.observation else { panic!() };
        result.observed_until = upper_bound.into();
        snapshot.observed_until = result.observed_until.clone();
        assert!(proof.matches_source_with_intent(episode, &source, &capture, &intent));
    }
    let mut malformed = episode.clone();
    malformed.last_seen_at = "unrecorded-native-clock".into();
    assert!(!proof.matches_source_with_intent(&malformed, &source, &capture, &intent));
    let SourceObservation::FocusedPageWait { result, .. } = &mut source.observation else { panic!() };
    result.observed_until = "unrecorded-native-clock".into();
    assert!(!proof.matches_source_with_intent(episode, &source, &capture, &intent));
}

#[test]
fn admitted_and_failed_shared_events_pin_both_authorities_and_replay_without_duplicate_history() {
    let shared = shared_scenario();
    let intent = shared.intent();
    let event = DiagnosticInvestigationEvent {
        intent: Box::new(intent.clone()),
        attempt: None,
        source_work: SourceWorkState::NotStarted,
        outcome: DiagnosticInvestigationOutcome::Admitted,
    };
    let before = &shared.scenario.episode;
    let mut next = before.clone();
    next.analysis = DiagnosticAnalysisStatus::Collecting;
    let revision =
        DiagnosticRevision::with_investigation(before, next, event.clone(), "2020-01-01T01:00:00Z").unwrap().unwrap();
    assert!(revision.inputs.contains(&intent.focus.participant().parent) && revision.inputs.contains(&shared.owner));
    assert!(
        DiagnosticRevision::with_investigation(
            &revision.episode,
            revision.episode.clone(),
            event,
            "2020-01-01T01:01:00Z"
        )
        .unwrap()
        .is_none()
    );
    let mut tampered = revision.clone();
    tampered.inputs.retain(|reference| reference != &shared.owner);
    tampered.id = tampered.expected_id().unwrap();
    assert!(!tampered.is_valid());
}
