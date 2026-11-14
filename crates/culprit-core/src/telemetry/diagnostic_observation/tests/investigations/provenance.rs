use super::*;

const ANALYZED_AT: &str = "2026-11-14T12:00:01Z";

fn attempt(sequence: u64, manifest: bool) -> DiagnosticAttemptReceipt {
    DiagnosticAttemptReceipt {
        capture: CaptureIdentity {
            observation_id: format!("attempt-{sequence}"),
            revision: Some(CaptureRevision {
                outbox_id: "attempt-owner".into(),
                owner_epoch: "2".into(),
                sequence: sequence.to_string(),
            }),
        },
        manifest_sha256: manifest.then(|| digest(format!("attempt-manifest-{sequence}").as_bytes())),
    }
}

fn event(scenario: &Scenario) -> DiagnosticInvestigationEvent {
    DiagnosticInvestigationEvent {
        intent: Box::new(scenario.intent()),
        attempt: None,
        source_work: SourceWorkState::NotStarted,
        outcome: DiagnosticInvestigationOutcome::Admitted,
    }
}

fn stopped(scenario: &Scenario, sequence: u64) -> DiagnosticInvestigationEvent {
    DiagnosticInvestigationEvent {
        attempt: Some(attempt(sequence, true)),
        source_work: SourceWorkState::Quiescent,
        outcome: DiagnosticInvestigationOutcome::Stopped {
            issue: DiagnosticAnalysisIssue::SourceUnavailable {
                code: None,
                stopped: Some(CollectionStop::SourceNotReady),
            },
        },
        ..event(scenario)
    }
}

fn captured(scenario: &Scenario) -> DiagnosticInvestigationEvent {
    let attempt = attempt(3, true);
    let capture = DiagnosticCaptureRef {
        capture: attempt.capture.clone(),
        manifest_sha256: attempt.manifest_sha256.clone().unwrap(),
        source_scope_id: result_source(completed_result(scenario)).captured_range().unwrap().scope_id,
    };
    DiagnosticInvestigationEvent {
        attempt: Some(attempt),
        source_work: SourceWorkState::Quiescent,
        outcome: DiagnosticInvestigationOutcome::Captured { capture },
        ..event(scenario)
    }
}

#[test]
fn attempt_receipts_require_a_full_native_independent_outbox_identity_and_bounded_optional_digest() {
    assert!(attempt(1, false).is_valid(), "allocation can precede an authenticated or completed native source");
    assert!(attempt(1, true).is_valid());
    for changed in ["revision", "namespace", "epoch", "sequence", "identity", "digest"] {
        let mut receipt = attempt(1, true);
        match changed {
            "revision" => receipt.capture.revision = None,
            "namespace" => receipt.capture.revision.as_mut().unwrap().outbox_id.clear(),
            "epoch" => receipt.capture.revision.as_mut().unwrap().owner_epoch = "01".into(),
            "sequence" => receipt.capture.revision.as_mut().unwrap().sequence = "0".into(),
            "identity" => receipt.capture.observation_id = "x".repeat(513),
            _ => receipt.manifest_sha256 = Some("a".repeat(65)),
        }
        assert!(!receipt.is_valid(), "{changed}");
    }
    let mut encoded = serde_json::to_value(attempt(1, false)).unwrap();
    assert!(encoded.get("source_scope_id").is_none());
    encoded["native_clock"] = "2026-11-14T12:00:00Z".into();
    assert!(serde_json::from_value::<DiagnosticAttemptReceipt>(encoded).is_err());
}

#[test]
fn owner_events_keep_admission_allocation_and_explicit_native_settlement_distinct() {
    let scenario = scenario();
    let captured = captured(&scenario);
    for state in
        [SourceWorkState::NotStarted, SourceWorkState::Active, SourceWorkState::Quiescent, SourceWorkState::Unconfirmed]
    {
        for allocated in [false, true] {
            for outcome in [
                DiagnosticInvestigationOutcome::Admitted,
                DiagnosticInvestigationOutcome::Stopped { issue: DiagnosticAnalysisIssue::Interrupted },
                captured.outcome.clone(),
            ] {
                let expected = match outcome {
                    DiagnosticInvestigationOutcome::Admitted => state == SourceWorkState::NotStarted && !allocated,
                    DiagnosticInvestigationOutcome::Stopped { .. } => match state {
                        SourceWorkState::NotStarted | SourceWorkState::Unconfirmed => true,
                        SourceWorkState::Quiescent => allocated,
                        SourceWorkState::Active => false,
                    },
                    DiagnosticInvestigationOutcome::Captured { .. } => state == SourceWorkState::Quiescent && allocated,
                };
                let current = DiagnosticInvestigationEvent {
                    source_work: state,
                    outcome,
                    attempt: allocated.then(|| captured.attempt.clone().unwrap()),
                    ..event(&scenario)
                };
                assert_eq!(current.is_valid(), expected, "{state:?}, allocation {allocated}, {:?}", current.outcome);
                assert_eq!(current.key().is_ok(), expected);
            }
        }
    }
}

#[test]
fn captured_events_bind_the_exact_attempt_and_sealed_manifest_without_faking_a_native_scope() {
    let scenario = scenario();
    let mut unfinished_manifest = stopped(&scenario, 3);
    unfinished_manifest.attempt.as_mut().unwrap().manifest_sha256 = None;
    assert!(unfinished_manifest.is_valid(), "a settled native owner can survive a failed final manifest write");
    let encoded = serde_json::to_vec(&unfinished_manifest).unwrap();
    let restored: DiagnosticInvestigationEvent = serde_json::from_slice(&encoded).unwrap();
    assert_eq!(restored.key().unwrap(), unfinished_manifest.key().unwrap());
    assert!(captured(&scenario).is_valid());
    for changed in ["capture", "revision", "digest", "missing-digest", "scope", "shutdown"] {
        let mut current = captured(&scenario);
        match changed {
            "capture" => current.attempt.as_mut().unwrap().capture.observation_id = "other-attempt".into(),
            "revision" => current.attempt.as_mut().unwrap().capture.revision.as_mut().unwrap().sequence = "4".into(),
            "digest" => current.attempt.as_mut().unwrap().manifest_sha256 = Some(digest(b"other-manifest")),
            "missing-digest" => current.attempt.as_mut().unwrap().manifest_sha256 = None,
            "scope" => {
                let DiagnosticInvestigationOutcome::Captured { capture } = &mut current.outcome else { panic!() };
                capture.source_scope_id.clear();
            }
            _ => current.source_work = SourceWorkState::Unconfirmed,
        }
        assert!(!current.is_valid(), "{changed}");
    }
}

#[test]
fn event_relation_requires_the_original_native_focus_scope_and_monotonic_episode_revision() {
    let scenario = scenario();
    let current = event(&scenario);
    assert!(current.matches_episode(&scenario.episode));
    let mut newer = scenario.episode.clone();
    newer.revision = "3".into();
    assert!(current.matches_episode(&newer));
    for changed in ["episode", "future-revision", "epoch", "source", "population", "scope", "statement", "resource"] {
        let mut other = current.clone();
        match changed {
            "episode" => other.intent.episode_id = digest(b"other-episode"),
            "future-revision" => other.intent.episode_revision = "4".into(),
            "epoch" => {
                other.intent.focus.epoch = "another-generation".into();
                other.intent.qualification.scope.native_epoch = Some(other.intent.focus.epoch.clone());
            }
            "source" => {
                other.intent.focus.source = "other-native-source".into();
                other.intent.qualification.scope.source = other.intent.focus.source.clone();
            }
            "population" => other.intent.qualification.scope.population = ActivityPopulation::ClientBackends,
            "scope" => other.intent.qualification.scope.measurement_scope = MeasurementScope::Server,
            "statement" => {
                other.intent.focus.statement_identity = NativeIdentity::new("different-statement");
                other.intent.focus.original.statement_identity = Some(other.intent.focus.statement_identity.clone());
            }
            _ => {
                other.intent.focus.resource.native = NativeIdentity::new("5:1:100");
                other.intent.focus.original.wait.as_mut().unwrap().identified_resources =
                    vec![other.intent.focus.resource.clone()];
            }
        }
        assert!(
            other.is_valid(),
            "{changed}: this is a separately valid structural question, not authenticated approval"
        );
        assert!(!other.matches_episode(&newer), "{changed}");
    }
}

#[test]
fn event_installation_is_replay_safe_and_distinguishes_new_attempts_at_the_same_deferred_status() {
    let scenario = scenario();
    let mut before = scenario.episode.clone();
    before.analysis = DiagnosticAnalysisStatus::Deferred;
    before.issue =
        Some(DiagnosticAnalysisIssue::SourceUnavailable { code: None, stopped: Some(CollectionStop::SourceNotReady) });
    let first_event = stopped(&scenario, 3);
    let first = DiagnosticRevision::with_investigation(&before, before.clone(), first_event.clone(), ANALYZED_AT)
        .unwrap()
        .unwrap();
    assert_eq!(first.episode.investigation_event, Some(first_event.key().unwrap()));
    assert_eq!(first.investigation.as_ref().unwrap().as_ref(), &first_event);
    assert_eq!(first.episode.analysis, before.analysis);
    assert!(first.is_valid());
    assert!(
        DiagnosticRevision::with_investigation(
            &first.episode,
            first.episode.clone(),
            first_event,
            "2026-11-14T12:01:00Z"
        )
        .unwrap()
        .is_none()
    );

    let second = DiagnosticRevision::with_investigation(
        &first.episode,
        first.episode.clone(),
        stopped(&scenario, 4),
        ANALYZED_AT,
    )
    .unwrap()
    .unwrap();
    assert_eq!(second.previous_revision, Some(first.episode.revision.clone()));
    assert_eq!(second.episode.analysis, first.episode.analysis);
    assert_eq!(second.episode.issue, first.episode.issue);
    assert_ne!(second.id, first.id);
    assert_ne!(second.episode.investigation_event, first.episode.investigation_event);
}

#[test]
fn native_transitions_preserve_compact_history_without_repeating_the_full_event() {
    let scenario = scenario();
    let first = DiagnosticRevision::with_investigation(
        &scenario.episode,
        scenario.episode.clone(),
        event(&scenario),
        ANALYZED_AT,
    )
    .unwrap()
    .unwrap();
    let mut next = first.episode.clone();
    next.analysis = DiagnosticAnalysisStatus::Deferred;
    next.issue = Some(DiagnosticAnalysisIssue::EvidenceUnavailable);
    let native = DiagnosticRevision::between(Some(&first.episode), next, ANALYZED_AT).unwrap().unwrap();
    assert_eq!(native.episode.investigation_event, first.episode.investigation_event);
    assert!(native.investigation.is_none());
    assert!(native.is_valid());
    for changed in [Some(digest(b"forged-event")), None] {
        let mut next = native.episode.clone();
        next.investigation_event = changed;
        assert!(DiagnosticRevision::between(Some(&native.episode), next, ANALYZED_AT).is_err());
    }
    assert!(DiagnosticRevision::between(None, first.episode.clone(), ANALYZED_AT).is_err());
    let head = serde_json::to_value(&native.episode).unwrap();
    assert!(head["investigation_event"].is_string());
    for field in ["intent", "qualification", "recipe_id", "investigation"] {
        assert!(head.get(field).is_none(), "{field} belongs only to immutable history");
    }
}

#[test]
fn delayed_parent_event_is_an_exact_input_and_missing_or_conflicting_provenance_is_rejected() {
    let scenario = scenario();
    let later = frame(2, vec![actor()], true);
    let mut before = scenario.episode.clone();
    before.latest = later.2[0].0.clone();
    before.last_seen_at = later.0.observation.activity().unwrap().at.clone();
    before.revision = "2".into();
    assert!(before.is_valid());
    let original = event(&scenario);
    let revision = DiagnosticRevision::with_investigation(&before, before.clone(), original.clone(), ANALYZED_AT)
        .unwrap()
        .unwrap();
    assert_eq!(revision.inputs.len(), 2);
    assert!(revision.inputs.contains(&scenario.focus.parent));
    let mut missing = revision.clone();
    missing.inputs.retain(|input| input != &scenario.focus.parent);
    missing.id = missing.expected_id().unwrap();
    assert!(!missing.is_valid());
    let mut conflicting = revision.clone();
    conflicting.investigation = Some(Box::new(stopped(&scenario, 3)));
    conflicting.id = conflicting.expected_id().unwrap();
    assert!(!conflicting.is_valid(), "the compact key must name the complete event supplied here");
    let mut detached = revision.clone();
    detached.episode.investigation_event = None;
    detached.id = detached.expected_id().unwrap();
    assert!(!detached.is_valid());
}

#[test]
fn recovered_lifecycle_and_first_native_recovery_proof_survive_owner_events() {
    let scenario = scenario();
    let previous = DiagnosticCheckpoint {
        controller_version: DIAGNOSTIC_CONTROLLER_VERSION,
        target: scenario.episode.scope.target.clone(),
        input: scenario.episode.first.capture_ref(),
        episodes: vec![scenario.episode.clone()],
    };
    let empty = frame(2, vec![], true);
    let recovered = observe(&empty, Some(&previous)).checkpoint.episodes.remove(0);
    assert_eq!(recovered.lifecycle, DiagnosticLifecycle::Resolved);
    assert!(recovered.analysis.needs_work());
    assert!(event(&scenario).matches_episode(&recovered));
    let revision = DiagnosticRevision::with_investigation(&recovered, recovered.clone(), event(&scenario), ANALYZED_AT)
        .unwrap()
        .unwrap();
    assert_eq!(revision.episode.lifecycle, recovered.lifecycle);
    assert_eq!(revision.episode.recovery, recovered.recovery);
    assert_eq!(revision.episode.first, recovered.first);
    assert_eq!(revision.episode.latest, recovered.latest);
    assert_eq!(revision.episode.scope, recovered.scope);
    assert_eq!(revision.episode.analysis, recovered.analysis);
}

#[test]
fn provenance_keys_preserve_exact_native_accounting_and_reject_existing_bounds_or_revision_overflow() {
    let scenario = scenario();
    let mut positive = event(&scenario);
    positive.intent.qualification.read_metrics.templates[0].client_elapsed_ms = 0.0;
    let mut negative = positive.clone();
    negative.intent.qualification.read_metrics.templates[0].client_elapsed_ms = -0.0;
    assert_eq!(positive, negative, "numeric equality alone is not immutable provenance identity");
    assert_ne!(positive.key().unwrap(), negative.key().unwrap());
    for changed in ["native-component", "caller-query-cap", "accounting", "episode-counter"] {
        let mut invalid = positive.clone();
        match changed {
            "native-component" => invalid.intent.qualification.component.revision = Some("x".repeat(513)),
            "caller-query-cap" => invalid.intent.budget.max_queries = CollectionBudget::upper_limits().max_queries + 1,
            "accounting" => invalid.intent.qualification.read_metrics.templates[0].client_elapsed_ms = f64::INFINITY,
            _ => invalid.intent.episode_revision = "18446744073709551616".into(),
        }
        assert!(invalid.key().is_err(), "{changed}");
    }
    let mut exhausted = scenario.episode.clone();
    exhausted.revision = u64::MAX.to_string();
    assert!(
        DiagnosticRevision::with_investigation(&exhausted, exhausted.clone(), event(&scenario), ANALYZED_AT).is_err()
    );
    let mut bad_key = scenario.episode.clone();
    bad_key.investigation_event = Some("x".repeat(65));
    assert!(!bad_key.is_valid());
}
