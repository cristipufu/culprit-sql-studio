use super::*;

fn observe(scenario: &Scenario, previous: Option<&DiagnosticCheckpoint>) -> Result<DiagnosticObservationUpdate> {
    observe_diagnostic_page_task(
        &scenario.page_source,
        &scenario.page,
        &scenario.page_ref,
        &scenario.frame(true),
        previous,
        "2026-11-13T10:02:00Z",
    )
}

#[test]
fn parallel_task_incident_retains_both_receipts_while_its_coordinator_waits_on_something_else() {
    let scenario = Scenario::new();
    let update = observe(&scenario, None).unwrap();
    assert_eq!(update.revisions.len(), 1);
    assert_eq!(update.checkpoint.episodes.len(), 1);
    let episode = &update.checkpoint.episodes[0];
    assert_eq!(episode.lifecycle, DiagnosticLifecycle::Uncertain);
    assert_eq!(episode.analysis, DiagnosticAnalysisStatus::Pending);
    assert!(episode.recovery.is_none());
    assert_eq!(episode.first, scenario.page_ref);
    assert_eq!(episode.latest, scenario.page_ref);
    assert_eq!(episode.first_seen_at, scenario.page.task_wait.as_ref().unwrap().observed_at);
    assert_eq!(episode.last_seen_at, episode.first_seen_at);
    assert_eq!(episode.scope.source, scenario.source.source);
    assert_eq!(update.revisions[0].inputs, [scenario.page_ref.clone(), scenario.refs[0].clone()]);
    assert_eq!(scenario.rows[0].wait.as_ref().unwrap().native_type.as_deref(), Some("CXCONSUMER"));
    assert!(scenario.rows[0].wait.as_ref().unwrap().identified_resources.is_empty());
    let (_, question) = &update.focuses[0];
    let DiagnosticFocus::PageTaskHolders { focus } = question else { panic!("actual task question") };
    assert!(episode.matches_focus(question));
    assert!(episode.matches_page_task_holder_focus(focus));
    assert!(question.page_wait().is_none());
    assert_eq!(focus.task, scenario.page.task_wait.clone().unwrap());
    let restored: DiagnosticCheckpoint =
        serde_json::from_slice(&serde_json::to_vec(&update.checkpoint).unwrap()).unwrap();
    assert_eq!(restored, update.checkpoint);
}

#[test]
fn exact_replay_is_idempotent_but_reused_task_address_in_a_new_capture_never_extends_the_old_point() {
    let mut scenario = Scenario::new();
    let first = observe(&scenario, None).unwrap();
    let replay = observe(&scenario, Some(&first.checkpoint)).unwrap();
    assert!(replay.revisions.is_empty());
    assert_eq!(replay.checkpoint, first.checkpoint);
    assert_eq!(replay.focuses, first.focuses);
    scenario.rebind_page_as("independent-same-clock-same-address-task-page");
    let next = observe(&scenario, Some(&replay.checkpoint)).unwrap();
    assert_eq!(next.revisions.len(), 1);
    assert_eq!(next.checkpoint.episodes.len(), 2);
    assert_eq!(next.checkpoint.episodes[0], first.checkpoint.episodes[0]);
    assert_ne!(next.checkpoint.episodes[1].id, first.checkpoint.episodes[0].id);
    let DiagnosticSignal::PageTaskWait { task: original, .. } = &first.checkpoint.episodes[0].signal else { panic!() };
    let DiagnosticSignal::PageTaskWait { task: next, .. } = &next.checkpoint.episodes[1].signal else { panic!() };
    assert_eq!(original, next, "a native task address alone is never a cross-capture generation");
}

#[test]
fn missing_original_authority_or_rows_stays_unavailable_without_becoming_an_empty_healthy_incident() {
    for missing in ["activity-role", "page-role", "activity-owner", "page-owner", "activity-row", "partial-read"] {
        let mut scenario = Scenario::new();
        match missing {
            "activity-role" => {
                let SourceObservation::Activity(native) = &mut scenario.source.observation else { panic!() };
                native.database_writable = None;
            }
            "page-role" => {
                let SourceObservation::PageWaitTargets { database_writable, .. } =
                    &mut scenario.page_source.observation
                else {
                    panic!()
                };
                *database_writable = None;
            }
            "activity-owner" => scenario.source.partition = SourcePartition::Target,
            "page-owner" => scenario.page_source.partition = SourcePartition::Target,
            "activity-row" => {
                scenario.rows.clear();
                let SourceObservation::Activity(native) = &mut scenario.source.observation else { panic!() };
                native.observations = 0;
                native.native_rows = 0;
                native.coverage = StateSourceCoverage::collected(scenario.source.source.clone(), 0);
            }
            _ => (),
        }
        scenario.rebind_frame();
        scenario.rebind_page();
        let update = observe_diagnostic_page_task(
            &scenario.page_source,
            &scenario.page,
            &scenario.page_ref,
            &scenario.frame(missing != "partial-read"),
            None,
            "2026-11-13T10:02:00Z",
        )
        .unwrap();
        assert!(update.revisions.is_empty(), "{missing}");
        assert!(update.checkpoint.episodes.is_empty(), "{missing}");
        assert!(update.focuses.is_empty(), "{missing}");
        assert!(scenario.page.task_wait.is_some(), "the positive raw task remains independently inspectable");
    }
}

#[test]
fn exact_page_payload_and_original_task_statement_member_and_role_are_not_replaceable() {
    let scenario = Scenario::new();
    let update = observe(&scenario, None).unwrap();
    let episode = &update.checkpoint.episodes[0];
    let DiagnosticFocus::PageTaskHolders { focus } = &update.focuses[0].1 else { panic!() };
    for field in [
        "parent",
        "page",
        "session-generation",
        "request-generation",
        "statement",
        "task",
        "resource",
        "member",
        "epoch",
        "role",
    ] {
        let mut other = (**focus).clone();
        match field {
            "parent" => other.participant.parent.payload_sha256 = digest(b"another-parent"),
            "page" => other.page.payload_sha256 = digest(b"another-page"),
            "session-generation" => other.participant.original.key.session.generation = "another-login".into(),
            "request-generation" => {
                other.participant.original.key.request.as_mut().unwrap().generation = "another-request".into()
            }
            "statement" => other.participant.statement_identity = NativeIdentity::new("another-statement"),
            "task" => other.task.execution_context_id += 1,
            "resource" => other.task.resource.native.id = "5:1:43".into(),
            "member" => {
                let SourcePartition::InstanceLocal { member: Some(member), .. } = &mut other.participant.partition
                else {
                    panic!()
                };
                member.native.id = "another-member".into();
            }
            "epoch" => other.participant.epoch = "another-process".into(),
            _ => other.participant.database_writable = Some(false),
        }
        assert!(!episode.matches_page_task_holder_focus(&other), "{field}");
    }
    let mut tampered = Scenario::new();
    tampered.page.task_wait.as_mut().unwrap().wait_duration_ms = Some("1001".into());
    assert!(observe(&tampered, Some(&update.checkpoint)).is_err());
}

#[test]
fn complete_activity_absence_cannot_resolve_or_advance_the_original_task_point() {
    let scenario = Scenario::new();
    let task = observe(&scenario, None).unwrap();
    let mut source = scenario.source.clone();
    let SourceObservation::Activity(native) = &mut source.observation else { panic!() };
    native.at = "2026-11-13T10:03:00Z".into();
    native.observed_from = native.at.clone();
    native.observed_until = native.at.clone();
    native.native_rows = 0;
    native.observations = 0;
    native.coverage = StateSourceCoverage::collected(source.source.clone(), 0);
    let input = DiagnosticCaptureRef {
        capture: CaptureIdentity {
            observation_id: "complete-later-activity-roster".into(),
            revision: Some(CaptureRevision {
                outbox_id: "controlled-original-page-fixture".into(),
                owner_epoch: "1".into(),
                sequence: "3".into(),
            }),
        },
        manifest_sha256: digest(b"controlled later empty Activity manifest"),
        source_scope_id: source.captured_range().unwrap().scope_id,
    };
    assert!(input.is_valid());
    assert!(source.observation.covers_request());
    let later = observe_diagnostic_activity(
        &source,
        &input,
        &[],
        Some(&task.checkpoint),
        &ActivityPolicy::default(),
        "2026-11-13T10:03:01Z",
    )
    .unwrap();
    assert!(later.revisions.is_empty());
    assert_eq!(later.checkpoint.episodes, task.checkpoint.episodes);
    let mut fake_recovered = task.checkpoint.episodes[0].clone();
    fake_recovered.lifecycle = DiagnosticLifecycle::Resolved;
    fake_recovered.recovery = Some(DiagnosticRecoveryProof::CompleteScope { capture: input });
    assert!(!fake_recovered.is_valid());
}

#[test]
fn task_admission_preserves_the_independent_coordinator_question_and_never_reuses_its_completion() {
    let mut scenario = Scenario::new();
    let wait = scenario.rows[0].wait.as_mut().unwrap();
    wait.native_type = Some("PAGELATCH_SH".into());
    wait.identified_resources = vec![ObservedWaitResource {
        kind: WaitResourceKind::MemoryPage,
        native: NativeIdentity::new("5:1:42").qualified("source", "sys.dm_exec_requests.wait_resource"),
    }];
    scenario.page.request = scenario.rows[0].clone();
    scenario.rebind_frame();
    scenario.rebind_page();
    let rows = scenario
        .rows
        .iter()
        .zip(&scenario.refs)
        .map(|(observation, reference)| DiagnosticActivityRow { reference, observation })
        .collect::<Vec<_>>();
    let coordinator = observe_diagnostic_activity(
        &scenario.source,
        &scenario.refs[0].capture_ref(),
        &rows,
        None,
        &ActivityPolicy::default(),
        "2026-11-13T10:02:00Z",
    )
    .unwrap();
    assert!(
        coordinator
            .checkpoint
            .episodes
            .iter()
            .any(|episode| matches!(episode.signal, DiagnosticSignal::PageWait { .. }))
    );
    let next = observe(&scenario, Some(&coordinator.checkpoint)).unwrap();
    assert_eq!(next.checkpoint.episodes.len(), coordinator.checkpoint.episodes.len() + 1);
    assert!(coordinator.checkpoint.episodes.iter().all(|episode| next.checkpoint.episodes.contains(episode)));
    let task = next
        .checkpoint
        .episodes
        .iter()
        .find(|episode| matches!(episode.signal, DiagnosticSignal::PageTaskWait { .. }))
        .unwrap();
    assert_eq!(task.analysis, DiagnosticAnalysisStatus::Pending);
    assert!(task.report.is_none());
    for (_, coordinator_focus) in &coordinator.focuses {
        assert!(!task.matches_focus(coordinator_focus));
    }
}

#[test]
fn task_revisions_require_both_original_prerequisites_even_before_any_investigation() {
    let scenario = Scenario::new();
    let update = observe(&scenario, None).unwrap();
    let original = &update.revisions[0];
    for reference in [&scenario.refs[0], &scenario.page_ref] {
        let mut missing = original.clone();
        missing.inputs.retain(|input| input != reference);
        missing.id = missing.expected_id().unwrap();
        assert!(!missing.is_valid());
    }
}

#[test]
fn own_holder_result_finishes_only_the_task_question_and_keeps_all_original_receipts_in_history() {
    for unavailable in [false, true] {
        let scenario = Scenario::new();
        let admitted = observe(&scenario, None).unwrap();
        let DiagnosticFocus::PageTaskHolders { focus } = &admitted.focuses[0].1 else { panic!() };
        let (_, context, mut point) = fixture();
        point.focus_key = focus.key().unwrap();
        if unavailable {
            for native in [&mut point.before, &mut point.after] {
                native.holder = MssqlPageTaskHolder {
                    relation: PageTaskHolderRelation::Unavailable,
                    matching_owner_rows: 0,
                    task: None,
                    request: None,
                };
            }
        }
        assert!(focus.accepts_observation(&context, &point));
        let source = CollectionDescription {
            target: context.target.clone(),
            partition: context.partition.clone(),
            source: scenario.page.source.clone(),
            continuity: SourceContinuity::Unverified,
            observation: SourceObservation::PageTaskHolders {
                snapshot: StateSnapshotDescription {
                    dataset: SnapshotDataset::PageTaskHolders,
                    observed_from: context.observed_from.clone(),
                    observed_until: context.observed_until.clone(),
                    records: 1,
                    sources: vec![StateSourceCoverage::observed(scenario.page.source.clone(), 1, false)],
                },
                focus: focus.clone(),
                database_writable: context.database_writable,
            },
        };
        let capture = CaptureIdentity {
            observation_id: format!("guarded-holder-result-{unavailable}"),
            revision: Some(CaptureRevision {
                outbox_id: "controlled-original-page-fixture".into(),
                owner_epoch: "1".into(),
                sequence: "4".into(),
            }),
        };
        let exported = ExportedRecord::new(
            &source,
            &capture,
            TelemetryRecord::Operational(OperationalObservation {
                source: source.source.clone(),
                key: point.key().unwrap(),
                fact: OperationalFact::PageTaskHolders(Box::new(point)),
            }),
        )
        .unwrap();
        let reference =
            DiagnosticEvidenceRef::from_export(&source, &digest(b"actual controlled result manifest"), &exported, 0)
                .unwrap();
        let TelemetryRecord::Operational(row) = &exported.record else { panic!() };
        let mut nonzero = reference.clone();
        nonzero.row_ordinal = 1;
        assert!(
            analyze_diagnostic_page_task_holders(
                &source,
                &nonzero.capture_ref(),
                &[DiagnosticPageTaskHolderRow { reference: &nonzero, observation: row }],
                &admitted.checkpoint,
                "2026-11-13T10:02:00Z",
                None,
            )
            .is_err(),
            "a singleton aggregate has exactly its original ordinal zero"
        );
        let update = analyze_diagnostic_page_task_holders(
            &source,
            &reference.capture_ref(),
            &[DiagnosticPageTaskHolderRow { reference: &reference, observation: row }],
            &admitted.checkpoint,
            "2026-11-13T10:02:00Z",
            None,
        )
        .unwrap();
        assert!(update.checkpoint.episodes.is_empty(), "settled point questions do not fill the volatile working set");
        let [revision] = update.revisions.as_slice() else { panic!("immutable result history") };
        let episode = &revision.episode;
        assert_eq!(episode.lifecycle, DiagnosticLifecycle::Uncertain);
        assert_eq!(episode.first_seen_at, admitted.checkpoint.episodes[0].first_seen_at);
        assert_eq!(episode.last_seen_at, admitted.checkpoint.episodes[0].last_seen_at);
        assert!(episode.recovery.is_none());
        assert_eq!(
            episode.analysis,
            if unavailable { DiagnosticAnalysisStatus::Unresolved } else { DiagnosticAnalysisStatus::Analyzed }
        );
        assert!(matches!(&episode.report, Some(DiagnosticReport::PageTaskHolders { observation, page, parent })
            if observation == &reference && page.as_ref() == &scenario.page_ref && parent.as_ref() == &scenario.refs[0]));
        assert!(revision.inputs.contains(&scenario.page_ref));
        assert!(revision.inputs.contains(&scenario.refs[0]));
        assert!(revision.inputs.contains(&reference));
        assert!(episode.report.as_ref().unwrap().matches_task_holder_input(&source, [(&reference, row)]));
        let mut wrong = episode.clone();
        wrong.report = Some(DiagnosticReport::PageWait { observation: reference, blocking_contexts: vec![] });
        assert!(!wrong.is_valid(), "a holder result cannot satisfy a coordinator or writer question");
    }
}
