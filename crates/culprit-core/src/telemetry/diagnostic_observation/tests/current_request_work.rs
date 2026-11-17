use super::*;

fn current_actor() -> ActivityObservation {
    let mut row = actor();
    row.key.session = NativeActivityIdentity { id: "51".into(), generation: "700".into() };
    row.key.request = Some(NativeActivityIdentity { id: "19".into(), generation: "1000".into() });
    row.statement_identity = row
        .key
        .request
        .as_ref()
        .and_then(|request| crate::engines::mysql_activity_statement_identity(&row.key.session, request));
    row.wait = None;
    row.state = "executing".into();
    row.elapsed_ms = Some("120000".into());
    row
}

fn current_frame(
    sequence: u64,
    observations: Vec<ActivityObservation>,
    local: bool,
) -> (CollectionDescription, DiagnosticCaptureRef, Vec<(DiagnosticEvidenceRef, ActivityObservation)>) {
    let (mut source, mut input, rows) = frame(sequence, observations, true);
    source.target = DatabaseIdentity::server(Engine::Mysql, "app", "fixture.invalid", 3306, None).into();
    if local {
        source.partition = SourcePartition::InstanceLocal {
            member: Some(SharedResourceIdentity {
                engine: Engine::Mysql,
                kind: SharedResourceKind::Node,
                namespace: NativeIdentity::new("synthetic-native-system"),
                native: NativeIdentity::new("synthetic-native-member"),
            }),
            epoch: source.observation.activity().unwrap().epoch.clone(),
        };
    }
    let SourceObservation::Activity(native) = &mut source.observation else { panic!() };
    native.database_writable = Some(true);
    input.source_scope_id = source.captured_range().unwrap().scope_id;
    let rows = rows
        .into_iter()
        .enumerate()
        .map(|(ordinal, (_, observation))| {
            let exported =
                ExportedRecord::new(&source, &input.capture, TelemetryRecord::from(observation.clone())).unwrap();
            let reference =
                DiagnosticEvidenceRef::from_export(&source, &input.manifest_sha256, &exported, ordinal as u32).unwrap();
            (reference, observation)
        })
        .collect();
    (source, input, rows)
}

fn original() -> (DiagnosticObservationUpdate, ActivityRequestFocus) {
    let update = observe(&current_frame(1, vec![current_actor()], true), None);
    let [(_, DiagnosticFocus::CurrentRequestWork { focus })] = update.focuses.as_slice() else {
        panic!("one existing long-running question")
    };
    let focus = focus.as_ref().clone();
    (update, focus)
}

fn result(
    focus: &ActivityRequestFocus,
    binding: FocusedRequestBinding,
    membership: FocusedRequestMembership,
    complete: bool,
) -> (CollectionDescription, DiagnosticCaptureRef, Vec<(DiagnosticEvidenceRef, OperationalObservation)>) {
    let request = focus.original.key.request.as_ref().unwrap();
    let point = CurrentRequestWorkObservation {
        observed_at: "2020-01-01T00:02:00Z".into(),
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
    let count = u32::from(membership == FocusedRequestMembership::Observed);
    let source = CollectionDescription {
        target: focus.target.clone(),
        partition: if binding == FocusedRequestBinding::UnverifiedOriginalScope {
            SourcePartition::InstanceLocal { member: None, epoch: None }
        } else {
            focus.partition.clone()
        },
        source: "synthetic-current-request-work".into(),
        continuity: SourceContinuity::Unverified,
        observation: SourceObservation::CurrentRequestWork {
            profile: CurrentRequestWorkProfile::MysqlStatementEvent,
            snapshot: StateSnapshotDescription {
                dataset: SnapshotDataset::CurrentRequestWork,
                observed_from: point.observed_at.clone(),
                observed_until: point.observed_at.clone(),
                records: count,
                sources: vec![StateSourceCoverage::observed("synthetic-current-request-work".into(), count, complete)],
            },
            result: Box::new(FocusedRequestResult {
                focus: Some(Box::new(focus.clone())),
                database_writable: focus.database_writable,
                binding,
                membership,
            }),
        },
    };
    let input = DiagnosticCaptureRef {
        capture: CaptureIdentity {
            observation_id: "synthetic-current-work-result".into(),
            revision: Some(CaptureRevision {
                outbox_id: "owned-outbox".into(),
                owner_epoch: "1".into(),
                sequence: "20".into(),
            }),
        },
        manifest_sha256: digest(b"sealed current work result"),
        source_scope_id: source.captured_range().unwrap().scope_id,
    };
    let rows = if count == 0 {
        vec![]
    } else {
        let row = OperationalObservation {
            source: source.source.clone(),
            key: point.key().unwrap(),
            fact: OperationalFact::CurrentRequestWork(Box::new(point)),
        };
        let exported = ExportedRecord::new(&source, &input.capture, TelemetryRecord::Operational(row.clone())).unwrap();
        vec![(DiagnosticEvidenceRef::from_export(&source, &input.manifest_sha256, &exported, 0).unwrap(), row)]
    };
    (source, input, rows)
}

fn apply(
    input: &(CollectionDescription, DiagnosticCaptureRef, Vec<(DiagnosticEvidenceRef, OperationalObservation)>),
    previous: &DiagnosticCheckpoint,
) -> DiagnosticFocusUpdate {
    let rows = input
        .2
        .iter()
        .map(|(reference, observation)| DiagnosticCurrentRequestWorkRow { reference, observation })
        .collect::<Vec<_>>();
    analyze_diagnostic_current_request_work(&input.0, &input.1, &rows, previous, "2020-01-01T01:00:00Z", None).unwrap()
}

#[test]
fn existing_long_running_activity_gets_one_source_bound_question_without_a_new_signal_or_threshold() {
    let (update, focus) = original();
    assert_eq!(update.checkpoint.episodes.len(), 1);
    let episode = &update.checkpoint.episodes[0];
    assert!(matches!(episode.signal, DiagnosticSignal::Activity { condition: ActivityCondition::LongRunning, .. }));
    assert!(episode.matches_current_request_work_focus(&focus));
    let question = DiagnosticFocus::CurrentRequestWork { focus: Box::new(focus.clone()) };
    assert_eq!(question.selector(), RecipeSelector::CurrentRequestWork);
    assert_eq!(DiagnosticFocus::from_selection(&question.database_selection()), Some(question));
    assert_eq!(focus.original, current_actor());
}

#[test]
fn logical_original_scope_stays_structural_but_cannot_borrow_current_member_authority() {
    let frame = current_frame(1, vec![current_actor()], false);
    let update = observe(&frame, None);
    assert!(update.focuses.is_empty());
    let episode = &update.checkpoint.episodes[0];
    assert_eq!(episode.issue, Some(DiagnosticAnalysisIssue::MissingNativeIdentity));
    assert_eq!(episode.analysis, DiagnosticAnalysisStatus::Unresolved);
    assert_eq!(episode.first, frame.2[0].0);
    assert_eq!(episode.latest, frame.2[0].0);
    assert_eq!(update.revisions.len(), 1);
    assert_eq!(update.revisions[0].episode, *episode, "immutable history records the explicit missing-origin issue");
    let focus = ActivityRequestFocus {
        parent: frame.2[0].0.clone(),
        target: frame.0.target.clone(),
        partition: SourcePartition::Target,
        database_writable: Some(true),
        source: frame.0.source.clone(),
        epoch: "native-epoch".into(),
        original_observed_at: frame.0.observation.activity().unwrap().at.clone(),
        original: frame.2[0].1.clone(),
        statement_identity: frame.2[0].1.statement_identity.clone().unwrap(),
    };
    assert!(focus.is_current_request_work_focus(), "logical scope is a valid standalone unverified lookup");
    assert!(!episode.matches_current_request_work_focus(&focus));
    assert_eq!(
        episode.plan_focus(&frame.0, &frame.2[0].1, &frame.2[0].0).unwrap(),
        DiagnosticFocusPlan::NotRequired,
        "observation already recorded the terminal missing-origin gap"
    );
    let pending = DiagnosticEpisode::start(
        episode.scope.clone(),
        episode.signal.clone(),
        episode.first_seen_at.clone(),
        episode.first.clone(),
    )
    .unwrap();
    for state in [DiagnosticAnalysisStatus::Pending, DiagnosticAnalysisStatus::Deferred] {
        let mut question = pending.clone();
        question.analysis = state;
        question.issue = Some(DiagnosticAnalysisIssue::CostUnqualified);
        assert_eq!(
            question.plan_focus(&frame.0, &frame.2[0].1, &frame.2[0].0).unwrap(),
            DiagnosticFocusPlan::Unavailable(DiagnosticAnalysisIssue::MissingNativeIdentity)
        );
    }
    assert!(!episode.analysis.needs_work(), "reporting missing authority does not reopen a terminal stage");
}

#[test]
fn unsupported_or_missing_native_event_tokens_never_become_a_hash_or_sql_based_focus() {
    for identity in [None, Some(NativeIdentity::new("same-query-hash"))] {
        let mut actor = current_actor();
        actor.statement_identity = identity;
        actor.query_hash = Some("same-query-hash".into());
        let frame = current_frame(1, vec![actor], true);
        let update = observe(&frame, None);
        assert!(update.focuses.is_empty());
        assert_eq!(update.checkpoint.episodes[0].issue, Some(DiagnosticAnalysisIssue::MissingNativeIdentity));
        let observed = &update.checkpoint.episodes[0];
        assert_eq!(observed.analysis, DiagnosticAnalysisStatus::Unresolved);
        assert_eq!(
            observed.plan_focus(&frame.0, &frame.2[0].1, &frame.2[0].0).unwrap(),
            DiagnosticFocusPlan::NotRequired
        );
        let pending = DiagnosticEpisode::start(
            observed.scope.clone(),
            observed.signal.clone(),
            observed.first_seen_at.clone(),
            observed.first.clone(),
        )
        .unwrap();
        assert_eq!(
            pending.plan_focus(&frame.0, &frame.2[0].1, &frame.2[0].0).unwrap(),
            DiagnosticFocusPlan::Unavailable(DiagnosticAnalysisIssue::MissingNativeIdentity)
        );
    }
    let mut ordinary = actor();
    ordinary.elapsed_ms = Some("120000".into());
    ordinary.wait = None;
    let unsupported = frame(1, vec![ordinary], true);
    let update = observe(&unsupported, None);
    assert!(update.focuses.is_empty(), "ordinary other-source long-running facts retain their own existing policy");
    assert_eq!(
        update.checkpoint.episodes[0].plan_focus(&unsupported.0, &unsupported.2[0].1, &unsupported.2[0].0).unwrap(),
        DiagnosticFocusPlan::NotRequired
    );
}

#[test]
fn matched_native_work_retains_exact_raw_evidence_without_rewriting_the_symptom_or_runtime() {
    let (first, focus) = original();
    let captured = result(&focus, FocusedRequestBinding::Matched, FocusedRequestMembership::Observed, true);
    let update = apply(&captured, &first.checkpoint);
    let next = &update.checkpoint.episodes[0];
    assert_eq!(next.analysis, DiagnosticAnalysisStatus::Analyzed);
    assert_eq!(next.lifecycle, DiagnosticLifecycle::Active);
    assert_eq!(next.latest, first.checkpoint.episodes[0].latest);
    assert_eq!(next.last_seen_at, first.checkpoint.episodes[0].last_seen_at);
    assert!(next.recovery.is_none());
    let report = next.report.as_ref().unwrap();
    assert_eq!(report.evidence(), &captured.2[0].0);
    assert_eq!(report.references().count(), 1);
    assert!(
        report.matches_current_request_work_input(
            &captured.0,
            captured.2.iter().map(|(reference, row)| (reference, row))
        )
    );
    assert!(update.revisions[0].inputs.contains(&focus.parent));
    assert!(update.revisions[0].inputs.contains(&captured.2[0].0));
    let baseline = current_frame(1, vec![current_actor()], true);
    assert_eq!(
        next.plan_focus(&baseline.0, &baseline.2[0].1, &baseline.2[0].0).unwrap(),
        DiagnosticFocusPlan::NotRequired,
        "a completed exact native lookup cannot run again"
    );
}

#[test]
fn missing_changed_unverified_and_partial_points_preserve_the_incident_and_analysis_debt() {
    let (first, focus) = original();
    for (binding, membership) in [
        (FocusedRequestBinding::Matched, FocusedRequestMembership::NotObserved),
        (FocusedRequestBinding::ParticipantMissing, FocusedRequestMembership::NotObserved),
        (FocusedRequestBinding::ParticipantChanged, FocusedRequestMembership::NotObserved),
        (FocusedRequestBinding::UnverifiedOriginalScope, FocusedRequestMembership::Unavailable),
        (FocusedRequestBinding::Matched, FocusedRequestMembership::Unavailable),
        (FocusedRequestBinding::Matched, FocusedRequestMembership::Observed),
    ] {
        let captured = result(&focus, binding, membership, false);
        let update = apply(&captured, &first.checkpoint);
        let next = &update.checkpoint.episodes[0];
        assert_eq!(next.analysis, DiagnosticAnalysisStatus::Partial, "{binding:?}/{membership:?}");
        assert_eq!(next.lifecycle, DiagnosticLifecycle::Active);
        assert!(next.recovery.is_none() && next.analysis.needs_work());
        assert_eq!(next.first, first.checkpoint.episodes[0].first);
        let proof = DiagnosticRecoveryProof::FocusedParticipantGone {
            capture: captured.1.clone(),
            parent: Box::new(focus.parent.clone()),
        };
        assert!(!proof.matches_source(next, &captured.0, &captured.1));
    }
}

#[test]
fn symptom_recovery_retains_native_work_debt_and_later_analysis_cannot_reopen_or_rewrite_it() {
    let (first, focus) = original();
    let recovered = observe(&current_frame(3, vec![], true), Some(&first.checkpoint));
    let recovery = recovered.checkpoint.episodes[0].recovery.clone();
    let unavailable =
        result(&focus, FocusedRequestBinding::ParticipantMissing, FocusedRequestMembership::NotObserved, false);
    let partial = apply(&unavailable, &recovered.checkpoint);
    let next = &partial.checkpoint.episodes[0];
    assert_eq!(next.lifecycle, DiagnosticLifecycle::Resolved);
    assert_eq!(next.analysis, DiagnosticAnalysisStatus::Partial);
    assert_eq!(next.recovery, recovery);
    assert!(next.retained_in_checkpoint());
    let captured = result(&focus, FocusedRequestBinding::Matched, FocusedRequestMembership::Observed, true);
    let analyzed = apply(&captured, &partial.checkpoint);
    assert!(analyzed.checkpoint.episodes.is_empty());
    let historical = &analyzed.revisions[0].episode;
    assert_eq!(historical.lifecycle, DiagnosticLifecycle::Resolved);
    assert_eq!(historical.recovery, recovery);
    assert_eq!(historical.first, first.checkpoint.episodes[0].first);
    assert!(historical.report.is_some());
}

#[test]
fn altered_raw_points_or_receipts_fail_and_partial_evidence_cannot_claim_complete_analysis() {
    let (first, focus) = original();
    let mut captured = result(&focus, FocusedRequestBinding::Matched, FocusedRequestMembership::Observed, true);
    let OperationalFact::CurrentRequestWork(point) = &mut captured.2[0].1.fact else { panic!() };
    point.native.mysql_mut().unwrap().created_tmp_disk_tables = Some("999".into());
    let rows = captured
        .2
        .iter()
        .map(|(reference, observation)| DiagnosticCurrentRequestWorkRow { reference, observation })
        .collect::<Vec<_>>();
    assert!(
        analyze_diagnostic_current_request_work(
            &captured.0,
            &captured.1,
            &rows,
            &first.checkpoint,
            "2020-01-01T01:00:00Z",
            None
        )
        .is_err()
    );
    let partial = result(&focus, FocusedRequestBinding::Matched, FocusedRequestMembership::Observed, false);
    let update = apply(&partial, &first.checkpoint);
    let mut forged = update.checkpoint.episodes[0].clone();
    forged.analysis = DiagnosticAnalysisStatus::Analyzed;
    let exported =
        ExportedRecord::new(&partial.0, &partial.1.capture, TelemetryRecord::Operational(partial.2[0].1.clone()))
            .unwrap();
    assert!(!forged.report.as_ref().unwrap().matches_observation(&forged, &partial.0, &exported));
}

#[test]
fn a_valid_point_from_an_undeclared_native_source_cannot_supply_the_report() {
    let (first, focus) = original();
    let mut captured = result(&focus, FocusedRequestBinding::Matched, FocusedRequestMembership::Observed, true);
    let update = apply(&captured, &first.checkpoint);
    let report = update.checkpoint.episodes[0].report.as_ref().unwrap();
    captured.2[0].1.source = "another-native-source".into();
    assert!(captured.2[0].1.is_valid());
    assert!(
        !report.matches_current_request_work_input(
            &captured.0,
            captured.2.iter().map(|(reference, row)| (reference, row))
        )
    );
}
