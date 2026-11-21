use super::*;
fn completed() -> ReadConnectionWork {
    let recorder = ReadConnectionRecorder::default();
    let attempt = recorder.begin();
    let acquire = attempt.phase(ReadConnectionStage::Acquire, None);
    attempt.connection_use(ReadConnectionUse::AuthenticatedReuse);
    attempt.acquired(1);
    acquire.finish(ReadConnectionOutcome::Completed);
    attempt
        .phase(ReadConnectionStage::FirstQuery, Some(digest(b"private original SQL")))
        .finish(ReadConnectionOutcome::Failed { code: crate::ErrorCode::PermissionDenied, native_code: None });
    let work = recorder.snapshot().unwrap();
    assert!(work.is_valid());
    work
}
#[test]
fn reused_session_and_failed_first_query_remain_separate_from_a_fresh_login_or_database_identity() {
    let work = completed();
    assert_eq!(work.attempted, 1);
    assert_eq!(work.attempts[0].connection_use, ReadConnectionUse::AuthenticatedReuse);
    assert_eq!(work.attempts[0].phases[0].outcome, ReadConnectionOutcome::Completed);
    assert!(matches!(
        work.attempts[0].phases[1].outcome,
        ReadConnectionOutcome::Failed { code: crate::ErrorCode::PermissionDenied, .. }
    ));
    let mut missing = serde_json::to_value(&work).unwrap();
    missing["attempts"][0].as_object_mut().unwrap().remove("reader_generation");
    assert!(serde_json::from_value::<ReadConnectionWork>(missing).is_err());
    let text = serde_json::to_string(&work).unwrap();
    assert!(!text.contains("private original SQL"));
    let replay = serde_json::from_str::<ReadConnectionWork>(&text).unwrap();
    assert!(replay.is_valid());
    assert_eq!(replay.attempted, work.attempted);
    assert_eq!(replay.attempts[0].connection_use, work.attempts[0].connection_use);
    assert_eq!(replay.attempts[0].phases[1].outcome, work.attempts[0].phases[1].outcome);
    assert_eq!(replay.attempts[0].phases[0].client_started_at, work.attempts[0].phases[0].client_started_at);
    assert_eq!(replay.attempts[0].phases[0].client_finished_at, work.attempts[0].phases[0].client_finished_at);
}
#[test]
fn a_dropped_original_phase_remains_unconfirmed_and_never_becomes_completed_on_snapshot() {
    let recorder = ReadConnectionRecorder::default();
    let attempt = recorder.begin();
    let phase = attempt.phase(ReadConnectionStage::Acquire, None);
    assert!(recorder.snapshot().unwrap().has_unconfirmed_work());
    drop(phase);
    let work = recorder.snapshot().unwrap();
    assert!(work.is_valid());
    assert!(work.has_unconfirmed_work());
    assert_eq!(work.unconfirmed_phases, 1);
    assert_eq!(work.attempts[0].phases[0].outcome, ReadConnectionOutcome::Unconfirmed);
    assert_eq!(recorder.snapshot().unwrap(), work);
}
#[test]
fn bounded_retention_preserves_omission_counts_and_unconfirmed_unretained_work() {
    let recorder = ReadConnectionRecorder::default();
    for _ in 0..MAX_READ_CONNECTION_ATTEMPTS + 2 {
        let attempt = recorder.begin();
        attempt
            .phase(ReadConnectionStage::Acquire, None)
            .finish(ReadConnectionOutcome::Failed { code: crate::ErrorCode::ConnectionFailed, native_code: None });
    }
    let attempt = recorder.begin();
    drop(attempt.phase(ReadConnectionStage::Acquire, None));
    let work = recorder.snapshot().unwrap();
    assert!(work.is_valid());
    assert_eq!(work.attempted, 7);
    assert_eq!(work.omitted_attempts, 3);
    assert_eq!(work.attempts.len(), 4);
    assert_eq!(work.omitted_phases, 3);
    assert_eq!(work.unconfirmed_phases, 1);
    assert!(!work.has_complete_observation_roster());
    let recorder = ReadConnectionRecorder::default();
    let attempt = recorder.begin();
    attempt
        .phase(ReadConnectionStage::Acquire, None)
        .finish(ReadConnectionOutcome::Failed { code: crate::ErrorCode::ConnectionFailed, native_code: None });
    for _ in 0..MAX_READ_CONNECTION_PHASES + 1 {
        attempt.phase(ReadConnectionStage::RouteSelection, None).finish(ReadConnectionOutcome::Completed);
    }
    let work = recorder.snapshot().unwrap();
    assert!(work.is_valid());
    assert_eq!(work.attempts[0].phases.len(), 12);
    assert_eq!(work.omitted_phases, 2);
    assert_eq!(work.attempts[0].omitted_phases, 2);
}
#[test]
fn malformed_counts_clocks_generation_and_reuse_cannot_claim_a_valid_original() {
    type Alter = fn(&mut ReadConnectionWork);
    for (name, alter) in [
        ("count", (|work: &mut ReadConnectionWork| work.attempted += 1) as Alter),
        ("ordinal", |work| work.attempts[0].ordinal = 2),
        ("generation", |work| work.attempts[0].reader_generation = None),
        ("clock", |work| work.attempts[0].phases[0].client_started_at = "native unknown".into()),
        ("nonfinite", |work| work.attempts[0].phases[0].client_elapsed_ms = Some(f64::INFINITY)),
        ("reuse-login", |work| work.attempts[0].phases[1].stage = ReadConnectionStage::HandshakeAndLogin),
        ("query-template", |work| work.attempts[0].phases[1].template_id = None),
    ] {
        let mut work = completed();
        alter(&mut work);
        assert!(!work.is_valid(), "{name}");
    }
}
#[test]
fn explicit_nullable_acquisition_field_and_curated_summary_preserve_codes_without_native_authority() {
    let recorder = ReadMetricsRecorder::new(crate::Engine::Mssql);
    let mut metrics = recorder.snapshot().unwrap();
    metrics.connection_work = Some(Box::new(completed()));
    assert!(metrics.is_valid());
    let public = serde_json::to_value(metrics.summary(None).unwrap()).unwrap();
    assert_eq!(public["connection_work"], serde_json::to_value(metrics.connection_work).unwrap());
    let mut raw = serde_json::to_value(recorder.snapshot().unwrap()).unwrap();
    raw.as_object_mut().unwrap().remove("connection_work");
    assert!(serde_json::from_value::<ReadWorkMetrics>(raw).is_err());
}

#[test]
fn ledger_progression_completes_pending_phases_and_preserves_original_completed_fields() {
    let recorder = ReadConnectionRecorder::default();
    let attempt = recorder.begin();
    let acquire = attempt.phase(ReadConnectionStage::Acquire, None);
    let pending = recorder.snapshot().unwrap();
    attempt.connection_use(ReadConnectionUse::FreshConnection);
    attempt.acquired(7);
    acquire.finish(ReadConnectionOutcome::Completed);
    let acquired = recorder.snapshot().unwrap();
    assert!(acquired.follows(&pending));
    assert!(acquired.follows(&acquired));
    let query = attempt.phase(ReadConnectionStage::FirstQuery, Some(digest(b"original")));
    let running = recorder.snapshot().unwrap();
    assert!(running.follows(&acquired));
    query.finish(ReadConnectionOutcome::Failed { code: crate::ErrorCode::PermissionDenied, native_code: Some(229) });
    let completed = recorder.snapshot().unwrap();
    assert!(completed.follows(&running));
    assert!(!running.follows(&completed));
    assert!(!pending.follows(&completed));
}
#[test]
fn valid_but_changed_or_disappeared_completed_entries_are_not_recorder_progress() {
    type Alter = fn(&mut ReadConnectionWork);
    let original = completed();
    for (name, alter) in [
        (
            "empty",
            (|work: &mut ReadConnectionWork| {
                work.attempts.clear();
                work.attempted = 0;
            }) as Alter,
        ),
        ("generation", |work| work.attempts[0].reader_generation = Some("99".into())),
        ("classification", |work| work.attempts[0].connection_use = ReadConnectionUse::FreshConnection),
        ("original outcome", |work| work.attempts[0].phases[1].outcome = ReadConnectionOutcome::Completed),
        ("elapsed", |work| work.attempts[0].phases[1].client_elapsed_ms = Some(1234.0)),
        ("template", |work| work.attempts[0].phases[1].template_id = Some(digest(b"foreign"))),
    ] {
        let mut changed = original.clone();
        alter(&mut changed);
        assert!(changed.is_valid(), "{name}");
        assert!(!changed.follows(&original), "{name}");
    }
}
