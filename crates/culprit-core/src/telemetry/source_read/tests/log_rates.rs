use super::*;

fn original_log_wait() -> Original {
    let mut value = original();
    let TelemetryRecord::Activity(actor) = &mut value.row.record else { panic!() };
    actor.wait.as_mut().unwrap().class = Some(ActivityWaitClass::LogRateGovernance);
    actor.wait.as_mut().unwrap().native_type = Some("LOG_RATE_GOVERNOR".into());
    let capture = CaptureIdentity { observation_id: "original-log-governance-wait".into(), ..value.row.capture };
    value.row = ExportedRecord::new(&value.source, &capture, value.row.record).unwrap();
    let reference =
        DiagnosticEvidenceRef::from_export(&value.source, &digest(b"original-log-manifest"), &value.row, 0).unwrap();
    let TelemetryRecord::Activity(actor) = &value.row.record else { panic!() };
    value.episode = DiagnosticEpisode::start(
        DiagnosticScope::from_source(&value.source).unwrap(),
        DiagnosticSignal::LogRateGovernance {
            key: actor.key.clone(),
            statement_identity: actor.statement_identity.clone(),
        },
        value.source.observation.activity().unwrap().at.clone(),
        reference,
    )
    .unwrap();
    value.focus = value.episode.source_read_focus().unwrap().unwrap();
    value
}

#[test]
fn log_wait_justifies_its_own_finite_question_without_a_compiler_answer() {
    let value = original_log_wait();
    assert!(value.episode.report.is_none());
    assert_eq!(value.focus.question, SourceReadQuestion::HyperscaleLogComponents);
    assert_eq!(value.focus.prerequisite, SourceReadPrerequisite::ObservedLogRateWait);
    assert!(value.focus.matches_basis(&value.source, &value.row, &value.execution.clone().into()));
    let DiagnosticFocusPlan::Ready(focus) = value.episode.plan_source_read_focus(&value.source, &value.row).unwrap()
    else {
        panic!()
    };
    assert_eq!(focus.references().collect::<Vec<_>>(), [&value.episode.first]);
    let enrolled = value.episode.enroll_source_read(value.focus).unwrap();
    assert!(enrolled.has_owed_question());
    assert!(enrolled.needs_question(&focus));
    assert!(enrolled.report.is_none(), "component context does not manufacture a causal diagnosis");
}

#[test]
fn question_and_prerequisite_are_closed_pairs_and_reject_cross_family_admission() {
    let compiler = original();
    let log = original_log_wait();
    for (mut focus, prerequisite) in [
        (compiler.focus.clone(), log.focus.prerequisite.clone()),
        (log.focus.clone(), compiler.focus.prerequisite.clone()),
    ] {
        focus.prerequisite = prerequisite;
        assert!(!focus.is_valid());
    }
    assert!(!log.focus.matches_basis(&compiler.source, &compiler.row, &compiler.execution.clone().into()));
    assert!(!compiler.focus.matches_basis(&log.source, &log.row, &log.execution.clone().into()));
    for question in SourceReadQuestion::ALL {
        assert_eq!(SourceReadQuestion::for_dataset(question.dataset()), Some(*question));
    }
    assert_eq!(SourceReadQuestion::for_dataset(SnapshotDataset::Operational), None);
}

#[test]
fn log_followup_cannot_borrow_a_current_row_epoch_updateability_or_compiler_cost_receipt() {
    let value = original_log_wait();
    let mut changed = value.row.clone();
    changed.capture.observation_id = "later-same-request".into();
    assert!(!value.focus.matches_basis(&value.source, &changed, &value.execution.clone().into()));
    let mut execution = value.execution.clone();
    execution.database_writable = !execution.database_writable;
    assert!(!value.focus.matches_basis(&value.source, &value.row, &execution.into()));
    let mut source = value.source.clone();
    let SourceObservation::Activity(frame) = &mut source.observation else { panic!() };
    frame.database_writable = None;
    assert!(
        matches!(
            value.episode.plan_source_read_focus(&source, &value.row),
            Err(_) | Ok(DiagnosticFocusPlan::Unavailable(DiagnosticAnalysisIssue::MissingNativeIdentity))
        ),
        "missing original updateability cannot borrow the saved execution's value"
    );
    let enrolled = value.episode.enroll_source_read(value.focus.clone()).unwrap();
    let (mut qualified, component) = recipe(&original());
    assert!(
        DiagnosticInvestigationIntent::new_source_read(
            &enrolled,
            &qualified,
            value.focus.clone(),
            value.execution.clone(),
            &component,
            &CollectionBudget::default()
        )
        .is_err(),
        "a compiler cost receipt cannot approve a different SQL roster"
    );
    qualified.selector = value.focus.question.selector();
    let RecipeCostQualification::Measured(receipt) = &mut qualified.cost else { panic!() };
    receipt.selector = qualified.selector;
    assert!(
        DiagnosticInvestigationIntent::new_source_read(
            &enrolled,
            &qualified,
            value.focus,
            value.execution,
            &component,
            &CollectionBudget::default()
        )
        .is_ok(),
        "controlled receipt only tests routing; the selected native verifier remains mandatory"
    );
}

#[test]
fn actual_wait_survives_resolution_and_completion_retires_the_single_owed_question() {
    let value = original_log_wait();
    let mut enrolled = value.episode.enroll_source_read(value.focus.clone()).unwrap();
    enrolled.lifecycle = DiagnosticLifecycle::Resolved;
    enrolled.recovery = Some(DiagnosticRecoveryProof::CompleteScope { capture: value.episode.first.capture_ref() });
    assert!(enrolled.is_valid());
    assert!(enrolled.has_owed_question(), "recovery does not erase the original incident");
    let collecting = enrolled.transition_source_read(SourceReadStageState::Collecting).unwrap();
    let mut source = description(&value, "2020-01-01T12:00:03Z");
    let SourceObservation::SourceRead { snapshot, .. } = &mut source.observation else { panic!() };
    snapshot.dataset = SnapshotDataset::HyperscaleLogComponents;
    let mut capture = result_capture(&value);
    capture.source_scope_id = source.captured_range().unwrap().scope_id;
    let (mut qualified, component) = recipe(&original());
    qualified.selector = value.focus.question.selector();
    let RecipeCostQualification::Measured(receipt) = &mut qualified.cost else { panic!() };
    receipt.selector = qualified.selector;
    let intent = DiagnosticInvestigationIntent::new_source_read(
        &enrolled,
        &qualified,
        value.focus,
        value.execution,
        &component,
        &CollectionBudget::default(),
    )
    .unwrap();
    assert!(intent.matches_result_source(&source));
    let answered = collecting.complete_source_read(&intent, &source, &capture).unwrap();
    assert_eq!(answered.lifecycle, DiagnosticLifecycle::Resolved);
    assert_eq!(answered.first, enrolled.first);
    assert_eq!(answered.source_read_capture(), Some(&capture));
    assert!(!answered.has_owed_question());
}

#[test]
fn only_source_classified_request_waits_create_log_incidents_not_matching_display_names() {
    for classified in [true, false] {
        let value = original_log_wait();
        let mut row = value.row;
        let TelemetryRecord::Activity(actor) = &mut row.record else { panic!() };
        if !classified {
            actor.wait.as_mut().unwrap().class = None;
        }
        let exported = ExportedRecord::new(&value.source, &row.capture, row.record).unwrap();
        let reference =
            DiagnosticEvidenceRef::from_export(&value.source, &digest(b"observed-log-input"), &exported, 0).unwrap();
        let TelemetryRecord::Activity(actor) = &exported.record else { panic!() };
        let update = observe_diagnostic_activity(
            &value.source,
            &reference.capture_ref(),
            &[DiagnosticActivityRow { reference: &reference, observation: actor }],
            None,
            &ActivityPolicy::default(),
            "2020-01-01T12:00:03Z",
        )
        .unwrap();
        assert_eq!(
            update
                .checkpoint
                .episodes
                .iter()
                .filter(|episode| matches!(episode.signal, DiagnosticSignal::LogRateGovernance { .. }))
                .count(),
            usize::from(classified)
        );
    }
}
