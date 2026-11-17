use super::*;

struct PopulationCase {
    source: CollectionDescription,
    rows: Vec<(DiagnosticEvidenceRef, ActivityObservation)>,
    episodes: Vec<DiagnosticEpisode>,
    recipe: RecipeDescriptor,
    component: ComponentVersion,
}

impl PopulationCase {
    fn new() -> Self {
        Self::with_original_token(true)
    }

    fn with_original_token(known: bool) -> Self {
        let mut first = actor();
        if !known {
            first.statement_identity = None;
        }
        first.wait = None;
        first.elapsed_ms = Some("120000".into());
        let mut second = first.clone();
        second.key.session.id = "52".into();
        let (mut source, parent, _) = frame(1, vec![first.clone(), second.clone()], true);
        let mut owner = crate::telemetry::native_read_context::tests::read_context().partition;
        let SourcePartition::InstanceLocal { epoch, .. } = &mut owner else { panic!() };
        *epoch = Some("native-epoch".into());
        source.partition = owner;
        let SourceObservation::Activity(native) = &mut source.observation else { panic!() };
        native.database_writable = Some(true);
        let input = DiagnosticCaptureRef { source_scope_id: source.captured_range().unwrap().scope_id, ..parent };
        let rows = [first, second]
            .into_iter()
            .enumerate()
            .map(|(ordinal, row)| {
                let exported =
                    ExportedRecord::new(&source, &input.capture, TelemetryRecord::from(row.clone())).unwrap();
                let reference =
                    DiagnosticEvidenceRef::from_export(&source, &input.manifest_sha256, &exported, ordinal as u32)
                        .unwrap();
                (reference, row)
            })
            .collect::<Vec<_>>();
        let observed = observe(&(source.clone(), input, rows.clone()), None);
        let fixture = scenario();
        let mut recipe = fixture.recipe;
        recipe.id = "fixture.page-wait-task-population".into();
        recipe.selector = RecipeSelector::PageWaitTasks;
        let RecipeCostQualification::Measured(receipt) = &mut recipe.cost else { panic!() };
        receipt.recipe_id = recipe.id.clone();
        receipt.selector = recipe.selector;
        receipt.scope = observed.checkpoint.episodes[0].scope.clone().into();
        assert!(recipe.is_valid(), "synthetic structural cost data never qualifies a production source");
        Self { source, rows, episodes: observed.checkpoint.episodes, recipe, component: fixture.component }
    }

    fn question(&self, index: usize) -> DiagnosticFocus {
        let (reference, _) = &self.rows[index];
        let episode = self.episodes.iter().find(|episode| episode.first == *reference).unwrap();
        let DiagnosticFocusPlan::Ready(focus) =
            episode.plan_page_wait_tasks_focus(&population_evidence(&self.source, &self.rows)).unwrap()
        else {
            panic!("one native incident and its exact frame")
        };
        *focus
    }

    fn intent(&self, index: usize) -> DiagnosticInvestigationIntent {
        let question = self.question(index);
        let episode = self.episodes.iter().find(|episode| episode.first == question.participant().parent).unwrap();
        DiagnosticInvestigationIntent::new(
            episode,
            &self.recipe,
            question,
            &self.component,
            &CollectionBudget::default(),
        )
        .unwrap()
    }

    fn later(
        &self,
        token: bool,
    ) -> (CollectionDescription, DiagnosticCaptureRef, Vec<(DiagnosticEvidenceRef, ActivityObservation)>) {
        let mut source = self.source.clone();
        let SourceObservation::Activity(native) = &mut source.observation else { panic!() };
        native.at = "2020-01-01T00:02:00Z".into();
        native.observed_from = native.at.clone();
        native.observed_until = native.at.clone();
        let mut parent = self.rows[0].0.capture_ref();
        parent.capture.observation_id = "later-native-activity".into();
        parent.capture.revision.as_mut().unwrap().sequence = "101".into();
        parent.manifest_sha256 = digest(b"later-native-activity-manifest");
        parent.source_scope_id = source.captured_range().unwrap().scope_id;
        let rows = self
            .rows
            .iter()
            .enumerate()
            .map(|(ordinal, (_, original))| {
                let mut row = original.clone();
                if ordinal == 0 {
                    row.statement_identity = token.then(|| NativeIdentity::new("native-statement"));
                }
                let exported =
                    ExportedRecord::new(&source, &parent.capture, TelemetryRecord::from(row.clone())).unwrap();
                let reference =
                    DiagnosticEvidenceRef::from_export(&source, &parent.manifest_sha256, &exported, ordinal as u32)
                        .unwrap();
                (reference, row)
            })
            .collect();
        (source, parent, rows)
    }

    fn result(&self, intent: &DiagnosticInvestigationIntent) -> CollectionDescription {
        let source = "native-page-task-population";
        CollectionDescription {
            target: self.source.target.clone(),
            partition: self.source.partition.clone(),
            source: source.into(),
            continuity: SourceContinuity::Unverified,
            observation: SourceObservation::PageWaitTargets {
                snapshot: StateSnapshotDescription {
                    dataset: SnapshotDataset::PageWaitTargets,
                    observed_from: "2020-01-01T00:01:01Z".into(),
                    observed_until: "2020-01-01T00:01:02Z".into(),
                    records: 0,
                    sources: vec![StateSourceCoverage::observed(source.into(), 0, false)],
                },
                database_writable: Some(true),
                original_activity: Some(intent.focus.participant().parent.capture_ref()),
            },
        }
    }
}

#[test]
fn paired_question_retains_the_full_native_frame_context_without_fabricating_a_coordinator_wait() {
    let case = PopulationCase::new();
    let question = case.question(0);
    let DiagnosticFocus::PageWaitTasks { focus, original_context, original_requests } = &question else { panic!() };
    assert!(focus.original.wait.is_none());
    assert_eq!(**original_context, NativeReadContext::from_activity(&case.source).unwrap());
    assert!(question.matches_parent(&case.source, &case.rows[0].1, &case.rows[0].0));
    assert_eq!(question.references().collect::<Vec<_>>(), [&case.rows[0].0]);
    let selection = question.database_selection();
    assert!(
        matches!(&selection, CollectionSelection::PageWaitTasks { activity, original_context: context, original_requests: requests }
        if *activity == focus.parent.capture_ref() && context == original_context && requests == original_requests)
    );
    assert!(selection.focus().is_none(), "population selection does not claim request-exclusive membership");
    assert!(selection.requires_authenticated_parent());
}

#[test]
fn paired_lineage_restores_the_admitted_question_with_no_native_header_or_no_native_rows() {
    let case = PopulationCase::new();
    let intent = case.intent(0);
    let lineage = DiagnosticInvestigationLineage::from_intent(&intent).unwrap();
    let mut manifest = owner_manifest(&scenario());
    manifest.request =
        CollectionRequest { subject: intent.subject(), selection: intent.selection(), budget: intent.budget.clone() };
    manifest.investigation = Some(lineage.clone());
    assert_eq!(manifest.investigation_intent().unwrap(), Some(intent.clone()));
    let source = case.result(&intent);
    assert!(source.observation.diagnostic_focus().is_none(), "header cannot supply a lost trigger");
    assert_eq!(lineage.intent_for_source(&source).unwrap(), intent);
    manifest.description = Some(source);
    assert_eq!(manifest.investigation_intent().unwrap(), Some(intent.clone()));
    let mut encoded = serde_json::to_value(&lineage).unwrap();
    encoded.as_object_mut().unwrap().remove("focus");
    assert!(serde_json::from_value::<DiagnosticInvestigationLineage>(encoded).is_err());
}

#[test]
fn siblings_from_one_original_frame_share_one_population_stage_but_never_one_trigger_receipt() {
    let case = PopulationCase::new();
    let first = case.intent(0);
    let second = case.intent(1);
    assert_ne!(first.episode_id, second.episode_id);
    assert_ne!(first.key().unwrap(), second.key().unwrap());
    assert_eq!(first.episode_stage_key().unwrap(), second.episode_stage_key().unwrap());
    assert_eq!(first.focus.participant().parent.capture_ref(), second.focus.participant().parent.capture_ref());
    assert_ne!(first.focus.participant().parent, second.focus.participant().parent);
}

#[test]
fn changed_result_owner_role_parent_or_private_original_cannot_repair_a_paired_receipt() {
    let case = PopulationCase::new();
    let intent = case.intent(0);
    let lineage = DiagnosticInvestigationLineage::from_intent(&intent).unwrap();
    for field in ["owner", "epoch", "role", "parent", "clock"] {
        let mut source = case.result(&intent);
        match field {
            "owner" => {
                let SourcePartition::InstanceLocal { member: Some(member), .. } = &mut source.partition else {
                    panic!()
                };
                member.native.id = "other".into();
            }
            "epoch" => {
                let SourcePartition::InstanceLocal { epoch, .. } = &mut source.partition else { panic!() };
                *epoch = Some("new-process".into());
            }
            _ => {
                let SourceObservation::PageWaitTargets { snapshot, database_writable, original_activity } =
                    &mut source.observation
                else {
                    panic!()
                };
                match field {
                    "role" => *database_writable = None,
                    "parent" => original_activity.as_mut().unwrap().manifest_sha256 = digest(b"different original"),
                    _ => snapshot.observed_from = "2020-01-01T00:00:00Z".into(),
                }
            }
        }
        assert!(lineage.intent_for_source(&source).is_err(), "{field}");
    }
    let mut private = lineage.clone();
    private.focus.participant_mut().original.statement =
        DefinitionPayload::sql("SELECT secret FROM private_input", crate::Engine::Mssql);
    assert!(private.reconstruct(&private.focus).is_err());
    assert!(!private.matches_request(&CollectionRequest {
        subject: intent.subject(),
        selection: intent.selection(),
        budget: intent.budget.clone()
    }));
}

#[test]
fn only_the_first_receipt_can_authorize_the_population_even_when_a_later_point_has_the_same_native_key() {
    for first_token in [false, true] {
        let case = PopulationCase::with_original_token(first_token);
        let previous = DiagnosticCheckpoint {
            controller_version: DIAGNOSTIC_CONTROLLER_VERSION,
            target: case.source.target.clone(),
            input: case.rows[0].0.capture_ref(),
            episodes: case.episodes.clone(),
        };
        let later = case.later(!first_token);
        let updated = observe(&later, Some(&previous));
        let episode = updated.checkpoint.episodes.iter().find(|episode| episode.first == case.rows[0].0).unwrap();
        assert_ne!(episode.first, episode.latest);
        assert!(episode.plan_page_wait_tasks_focus(&population_evidence(&later.0, &later.2)).is_err());
        let original_plan = episode.plan_page_wait_tasks_focus(&population_evidence(&case.source, &case.rows)).unwrap();
        match original_plan {
            DiagnosticFocusPlan::Ready(question) if first_token => {
                assert!(episode.matches_focus(&question));
                let mut substituted = *question;
                substituted.participant_mut().parent = later.2[0].0.clone();
                substituted.participant_mut().original_observed_at = later.0.observation.activity().unwrap().at.clone();
                assert!(!episode.matches_focus(&substituted));
            }
            DiagnosticFocusPlan::Unavailable(DiagnosticAnalysisIssue::MissingNativeIdentity) if !first_token => (),
            _ => panic!("later token visibility cannot repair or erase the original receipt"),
        }
    }
}

#[test]
fn the_early_population_stage_key_is_the_same_canonical_intent_key_and_separates_original_captures() {
    let case = PopulationCase::new();
    let intent = case.intent(0);
    let original = intent.focus.participant().parent.capture_ref();
    let target = intent.qualification.scope.target();
    assert_eq!(page_wait_tasks_stage_key(&original, &target).unwrap(), intent.episode_stage_key().unwrap());
    let later = case.later(true);
    assert_ne!(page_wait_tasks_stage_key(&later.1, &target).unwrap(), intent.episode_stage_key().unwrap());
}

fn population_evidence<'a>(
    source: &'a CollectionDescription,
    rows: &'a [(DiagnosticEvidenceRef, ActivityObservation)],
) -> ActivityFrameEvidence<'a> {
    let rows = rows
        .iter()
        .map(|(reference, observation)| ActivityFrameRow { observation, reference: Some(reference) })
        .collect::<Vec<_>>();
    ActivityFrameEvidence::new(source, &rows, true).unwrap()
}

#[test]
fn the_population_roster_contains_every_eligible_first_frame_request_without_statement_bodies() {
    let case = PopulationCase::new();
    let frame = population_evidence(&case.source, &case.rows);
    let expected = frame.page_wait_task_requests().unwrap();
    let DiagnosticFocus::PageWaitTasks { original_requests, .. } = case.question(0) else { panic!() };
    assert_eq!(original_requests, expected);
    assert_eq!(original_requests.len(), 2);
    assert!(ActivityRequestSelector::is_canonical(&original_requests));
    let text = serde_json::to_string(&original_requests).unwrap();
    assert!(!text.contains("client"));
    assert!(!text.contains("elapsed_ms"));
    assert!(!text.contains("redaction_version"));
    let mut reversed = original_requests.clone();
    reversed.reverse();
    assert!(!ActivityRequestSelector::is_canonical(&reversed));
    let mut duplicate = original_requests;
    duplicate.push(duplicate[0].clone());
    assert!(!ActivityRequestSelector::is_canonical(&duplicate));
    assert!(!ActivityRequestSelector::is_canonical(&[]));
}

#[test]
fn only_native_request_database_rows_with_original_tokens_enter_the_roster() {
    let mut case = PopulationCase::new();
    case.rows[1].1.statement_identity = None;
    let row = &mut case.rows[1];
    let exported = ExportedRecord::new(&case.source, &row.0.capture, TelemetryRecord::from(row.1.clone())).unwrap();
    row.0 =
        DiagnosticEvidenceRef::from_export(&case.source, &row.0.manifest_sha256, &exported, row.0.row_ordinal).unwrap();
    let requests = population_evidence(&case.source, &case.rows).page_wait_task_requests().unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].key, case.rows[0].1.key);
    let rows =
        case.rows.iter().map(|(_, observation)| ActivityFrameRow { observation, reference: None }).collect::<Vec<_>>();
    let unreceipted = ActivityFrameEvidence::new(&case.source, &rows, true).unwrap();
    assert!(unreceipted.page_wait_task_requests().is_err());
}
