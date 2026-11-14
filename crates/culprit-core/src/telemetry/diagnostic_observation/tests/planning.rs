use super::*;

type Baseline = (CollectionDescription, DiagnosticCaptureRef, Vec<(DiagnosticEvidenceRef, ActivityObservation)>);

fn restore(checkpoint: &DiagnosticCheckpoint) -> DiagnosticCheckpoint {
    serde_json::from_slice(&serde_json::to_vec(checkpoint).unwrap()).unwrap()
}

fn ready(episode: &DiagnosticEpisode, baseline: &Baseline) -> ActivityWaitFocus {
    let (reference, original) = &baseline.2[0];
    let PageWaitFocusPlan::Ready(focus) = episode.plan_page_wait_focus(&baseline.0, original, reference).unwrap()
    else {
        panic!("the retained original still owes a qualified page question")
    };
    *focus
}

/// Fixture changes describe another authenticated native capture, rather than modifying a row under its old receipt.
fn rebind(baseline: &mut Baseline) {
    baseline.1.source_scope_id = baseline.0.captured_range().unwrap().scope_id;
    for (ordinal, (reference, row)) in baseline.2.iter_mut().enumerate() {
        let exported =
            ExportedRecord::new(&baseline.0, &baseline.1.capture, TelemetryRecord::from(row.clone())).unwrap();
        *reference =
            DiagnosticEvidenceRef::from_export(&baseline.0, &baseline.1.manifest_sha256, &exported, ordinal as u32)
                .unwrap();
    }
}

#[test]
fn a_restored_partial_positive_reconstructs_the_original_question_without_mutating_its_episode() {
    let baseline = frame(1, vec![actor()], false);
    let observed = observe(&baseline, None);
    let checkpoint = restore(&observed.checkpoint);
    let episode = &checkpoint.episodes[0];
    let focus = ready(episode, &baseline);
    assert_eq!(focus, observed.focuses[0].1);
    assert_eq!(focus.parent, baseline.2[0].0);
    assert_eq!(focus.original, baseline.2[0].1);
    assert!(focus.matches_parent(&baseline.0, &baseline.2[0].1, &baseline.2[0].0));
    assert!(episode.matches_focus(&focus));
    assert_eq!(baseline.0.observation.activity().unwrap().coverage.state, crate::SourceState::Partial);
    assert_eq!(checkpoint, observed.checkpoint, "planning does not advance lifecycle, analysis or revision");
}

#[test]
fn qualified_recovery_before_analysis_does_not_erase_the_owed_original_question() {
    let baseline = frame(1, vec![actor()], true);
    let observed = observe(&baseline, None);
    let recovered = observe(&frame(2, vec![], true), Some(&observed.checkpoint));
    let restored = restore(&recovered.checkpoint);
    let episode = &restored.episodes[0];
    assert_eq!(episode.lifecycle, DiagnosticLifecycle::Resolved);
    assert_eq!(episode.analysis, DiagnosticAnalysisStatus::Pending);
    let focus = ready(episode, &baseline);
    assert_eq!(focus, observed.focuses[0].1);
    assert_eq!(focus.original_observed_at, episode.first_seen_at);
    assert_eq!(restored, recovered.checkpoint, "the first recovery proof remains unchanged");
}

#[test]
fn a_delayed_middle_parent_keeps_its_own_clock_and_counters_after_a_newer_baseline() {
    let first = frame(1, vec![actor()], true);
    let initial = observe(&first, None);
    let mut row = actor();
    row.elapsed_ms = Some("2".into());
    let middle = frame(2, vec![row.clone()], true);
    let second = observe(&middle, Some(&initial.checkpoint));
    row.elapsed_ms = Some("3".into());
    row.request_counters.as_mut().unwrap().logical_reads = Some("99".into());
    let latest = frame(3, vec![row], true);
    let third = observe(&latest, Some(&second.checkpoint));
    let restored = restore(&third.checkpoint);
    let episode = &restored.episodes[0];
    assert_ne!(middle.2[0].0, episode.first);
    assert_ne!(middle.2[0].0, episode.latest);
    assert_ne!(episode.peak.as_ref(), Some(&middle.2[0].0));
    let focus = ready(episode, &middle);
    assert_eq!(focus.parent, middle.2[0].0);
    assert_eq!(focus.original_observed_at, "2020-01-01T00:02:00Z");
    assert_eq!(focus.original.elapsed_ms.as_deref(), Some("2"));
    assert!(focus.original.request_counters.unwrap().logical_reads.is_none());
    assert_eq!(episode.latest, latest.2[0].0);
}

#[test]
fn uncertain_visibility_preserves_the_question_from_its_last_positive_native_row() {
    let baseline = frame(1, vec![actor()], true);
    let observed = observe(&baseline, None);
    let mut unknown = actor();
    unknown.blocking_status = ActivityBlockingStatus::Unknown;
    unknown.wait = None;
    let later = observe(&frame(2, vec![unknown], false), Some(&observed.checkpoint));
    let restored = restore(&later.checkpoint);
    let episode = &restored.episodes[0];
    assert_eq!(episode.lifecycle, DiagnosticLifecycle::Uncertain);
    assert_eq!(ready(episode, &baseline), observed.focuses[0].1);
    assert!(episode.recovery.is_none());
}

#[test]
fn missing_native_identity_is_a_typed_unavailable_question_not_a_malformed_positive() {
    for missing in ["epoch", "statement", "request-scope", "wait-type"] {
        let mut row = actor();
        match missing {
            "statement" => row.statement_identity = None,
            "request-scope" => row.scope = ActivityScopeBasis::SessionDatabase,
            "wait-type" => row.wait.as_mut().unwrap().native_type = None,
            _ => (),
        }
        let mut baseline = frame(1, vec![row], false);
        if missing == "epoch" {
            let SourceObservation::Activity(snapshot) = &mut baseline.0.observation else { panic!() };
            snapshot.epoch = None;
            rebind(&mut baseline);
        }
        let observed = observe(&baseline, None);
        assert!(observed.focuses.is_empty(), "{missing}");
        let mut pending = observed.checkpoint.episodes[0].clone();
        assert_eq!(pending.issue, Some(DiagnosticAnalysisIssue::MissingNativeIdentity));
        // This is the pending episode supplied to the same planner before the observer records its unavailable result.
        pending.analysis = DiagnosticAnalysisStatus::Pending;
        pending.issue = None;
        assert_eq!(
            pending.plan_page_wait_focus(&baseline.0, &baseline.2[0].1, &baseline.2[0].0).unwrap(),
            PageWaitFocusPlan::Unavailable(DiagnosticAnalysisIssue::MissingNativeIdentity),
            "{missing}",
        );
    }
}

#[test]
fn another_source_population_scope_or_epoch_cannot_borrow_a_retained_question() {
    let baseline = frame(1, vec![actor()], true);
    let observed = observe(&baseline, None);
    let episode = &observed.checkpoint.episodes[0];
    for changed in ["source", "target", "population", "scope", "epoch"] {
        let mut other = baseline.clone();
        match changed {
            "source" => {
                other.0.source = "another-native-source".into();
                let SourceObservation::Activity(snapshot) = &mut other.0.observation else { panic!() };
                snapshot.coverage.source = other.0.source.clone();
            }
            "target" => {
                other.0.target =
                    DatabaseIdentity::server(Engine::Mssql, "app", "fixture.invalid", 1433, Some("another-db".into()))
                        .into();
            }
            _ => {
                let SourceObservation::Activity(snapshot) = &mut other.0.observation else { panic!() };
                match changed {
                    "population" => snapshot.population = ActivityPopulation::DefaultDatabaseConnections,
                    "scope" => snapshot.scope = MeasurementScope::Node,
                    _ => snapshot.epoch = Some("recycled-native-epoch".into()),
                }
            }
        }
        rebind(&mut other);
        assert!(episode.plan_page_wait_focus(&other.0, &other.2[0].1, &other.2[0].0).is_err(), "{changed}");
    }
}

#[test]
fn a_recycled_statement_resource_or_actor_cannot_replace_the_original_native_question() {
    let first = frame(1, vec![actor()], true);
    let initial = observe(&first, None);
    let latest = observe(&frame(3, vec![actor()], true), Some(&initial.checkpoint));
    let episode = &latest.checkpoint.episodes[0];
    for changed in ["actor", "statement", "resource"] {
        let mut row = actor();
        match changed {
            "actor" => row.key.request.as_mut().unwrap().generation = "reused-request".into(),
            "statement" => row.statement_identity = Some(NativeIdentity::new("other-statement")),
            _ => row.wait.as_mut().unwrap().identified_resources[0].native.id = "5:1:100".into(),
        }
        let other = frame(2, vec![row], true);
        assert!(episode.plan_page_wait_focus(&other.0, &other.2[0].1, &other.2[0].0).is_err(), "{changed}");
    }
    for sequence in [0, 4] {
        let mut outside = frame(2, vec![actor()], true);
        let SourceObservation::Activity(snapshot) = &mut outside.0.observation else { panic!() };
        let at = format!("2020-01-01T00:{sequence:02}:00Z");
        snapshot.observed_from = at.clone();
        snapshot.observed_until = at.clone();
        snapshot.at = at;
        rebind(&mut outside);
        assert!(episode.plan_page_wait_focus(&outside.0, &outside.2[0].1, &outside.2[0].0).is_err());
    }
}

#[test]
fn a_modified_native_payload_cannot_use_the_original_receipt_even_with_the_same_actor_and_resource() {
    let baseline = frame(1, vec![actor()], true);
    let observed = observe(&baseline, None);
    let episode = &observed.checkpoint.episodes[0];
    let mut changed = baseline.2[0].1.clone();
    changed.elapsed_ms = Some("999".into());
    assert!(episode.plan_page_wait_focus(&baseline.0, &changed, &baseline.2[0].0).is_err());
    let mut receipt = baseline.2[0].0.clone();
    receipt.payload_sha256 = digest(b"another-native-payload");
    assert!(receipt.is_valid());
    assert!(episode.plan_page_wait_focus(&baseline.0, &baseline.2[0].1, &receipt).is_err());
}

#[test]
fn native_question_planning_does_not_promote_cost_qualification_or_repeat_terminal_analysis() {
    let baseline = frame(1, vec![actor()], true);
    let observed = observe(&baseline, None);
    for analysis in [
        DiagnosticAnalysisStatus::Pending,
        DiagnosticAnalysisStatus::Deferred,
        DiagnosticAnalysisStatus::Collecting,
        DiagnosticAnalysisStatus::Partial,
    ] {
        let mut episode = observed.checkpoint.episodes[0].clone();
        episode.analysis = analysis;
        episode.issue = Some(DiagnosticAnalysisIssue::CostUnqualified);
        assert!(episode.is_valid());
        assert_eq!(ready(&episode, &baseline), observed.focuses[0].1);
        assert_eq!(episode.analysis, analysis);
        assert_eq!(episode.issue, Some(DiagnosticAnalysisIssue::CostUnqualified));
    }
    let mut terminal = observed.checkpoint.episodes[0].clone();
    terminal.analysis = DiagnosticAnalysisStatus::Unresolved;
    terminal.issue = Some(DiagnosticAnalysisIssue::NativeContextGone);
    assert_eq!(
        terminal.plan_page_wait_focus(&baseline.0, &baseline.2[0].1, &baseline.2[0].0).unwrap(),
        PageWaitFocusPlan::NotRequired,
    );
    let locks = frame(1, vec![lock_wait_actor()], true);
    let observed = observe(&locks, None);
    assert_eq!(
        observed.checkpoint.episodes[0].plan_page_wait_focus(&locks.0, &locks.2[0].1, &locks.2[0].0).unwrap(),
        PageWaitFocusPlan::NotRequired,
    );
}
