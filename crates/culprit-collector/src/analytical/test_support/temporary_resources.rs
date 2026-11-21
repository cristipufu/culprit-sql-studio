//! One controlled native corpus feeds the actual controller and source-free consumer controls.
use super::*;

pub fn temporary_resource_basis() -> (CollectionDescription, Vec<TelemetryRecord>) {
    let (mut source, records) = cleanup_evidence(CleanupCase::LocalSnapshotProtector);
    source.observation = SourceObservation::LocalResource {
        snapshot: source.observation.snapshot().unwrap().clone(),
        database_writable: true,
    };
    assert!(source.partition_is_consistent());
    (source, records)
}

pub fn temporary_resource_result(
    focus: &SourceReadFocus,
    execution: &SourceReadExecutionScope,
    state: culprit_core::SourceState,
) -> (CollectionDescription, Vec<TelemetryRecord>) {
    let cases = task_space_evidence_cases();
    let index = match state {
        culprit_core::SourceState::Collected => 0,
        culprit_core::SourceState::Partial => 1,
        culprit_core::SourceState::Empty => 2,
        culprit_core::SourceState::Unavailable => 3,
        culprit_core::SourceState::NotApplicable => panic!("separate unsupported source admission"),
    };
    let (mut source, mut records) = cases[index].clone();
    let mut snapshot = source.observation.snapshot().unwrap().clone();
    snapshot.observed_from = "2020-01-01T12:00:10Z".into();
    snapshot.observed_until = "2020-01-01T12:00:11Z".into();
    for record in &mut records {
        let TelemetryRecord::Operational(native) = record else { panic!() };
        let OperationalFact::TaskSpace(task) = &mut native.fact else { panic!() };
        task.observed_at = "2020-01-01T12:00:10.250000000Z".into();
        if let Some(check) = &mut task.before_check {
            check.observed_at = "2020-01-01T12:00:10.300000000Z".into()
        }
        if let Some(check) = &mut task.after_check {
            check.observed_at = "2020-01-01T12:00:10.700000000Z".into()
        }
        native.key = task.key().unwrap();
    }
    source.target = focus.target.clone();
    source.partition = SourcePartition::Target;
    source.observation = SourceObservation::SourceRead {
        execution: Box::new(NativeReadContext {
            target: focus.target.clone(),
            partition: execution.partition().clone(),
            observed_from: snapshot.observed_from.clone(),
            observed_until: snapshot.observed_until.clone(),
            database_writable: Some(execution.database_writable()),
        }),
        focus_key: focus.key().unwrap(),
        snapshot,
    };
    assert!(source.partition_is_consistent());
    (source, records)
}

/// A typed imported history corpus. Its synthetic receipts exercise canonical behavior, never native authority.
pub fn temporary_resource_history(partial: bool) -> (CollectionDescription, Vec<TelemetryRecord>) {
    let (basis_source, basis_records) = temporary_resource_basis();
    let mut basis_capture = fixture().0.capture;
    basis_capture.observation_id = "temporary-cleanup-original".into();
    let basis_rows = basis_records
        .into_iter()
        .map(|record| ExportedRecord::new(&basis_source, &basis_capture, record).unwrap())
        .collect::<Vec<_>>();
    let basis_refs = basis_rows
        .iter()
        .enumerate()
        .map(|(ordinal, row)| {
            DiagnosticEvidenceRef::from_export(
                &basis_source,
                &digest(b"temporary-original-manifest"),
                row,
                ordinal as u32,
            )
            .unwrap()
        })
        .collect::<Vec<_>>();
    let original_rows = basis_refs
        .iter()
        .zip(&basis_rows)
        .map(|(reference, row)| DiagnosticOriginalRow { reference, record: &row.record })
        .collect::<Vec<_>>();
    let observed = observe_diagnostic_original(
        &basis_source,
        &basis_refs[0].capture_ref(),
        &original_rows,
        None,
        "2020-01-01T12:00:02Z",
    )
    .unwrap();
    let episode = observed.checkpoint.episodes.into_iter().next().unwrap();
    let focus = episode.source_read_focus().unwrap().unwrap();
    let execution = SourceReadExecutionScope::from_context(
        focus.question,
        &NativeReadContext::from_snapshot(&basis_source).unwrap(),
    )
    .unwrap();
    let (result_source, result_records) = temporary_resource_result(
        &focus,
        &execution,
        if partial { culprit_core::SourceState::Partial } else { culprit_core::SourceState::Collected },
    );
    let mut result_capture = basis_capture;
    result_capture.observation_id = "temporary-task-result".into();
    let result_rows = result_records
        .into_iter()
        .map(|record| ExportedRecord::new(&result_source, &result_capture, record).unwrap())
        .collect::<Vec<_>>();
    let result_refs = result_rows
        .iter()
        .enumerate()
        .map(|(ordinal, row)| {
            DiagnosticEvidenceRef::from_export(
                &result_source,
                &digest(b"temporary-result-manifest"),
                row,
                ordinal as u32,
            )
            .unwrap()
        })
        .collect::<Vec<_>>();
    let result = DiagnosticCaptureRef {
        capture: result_capture,
        manifest_sha256: digest(b"temporary-result-manifest"),
        source_scope_id: result_source.captured_range().unwrap().scope_id,
    };
    let rows = result_refs.iter().zip(&result_rows).collect::<Vec<_>>();
    let answer =
        TemporaryResourceAnswer::from_originals(&focus, &basis_source, &basis_rows[0], &result_source, &result, &rows)
            .unwrap();
    let episode = episode
        .enroll_source_read(focus)
        .unwrap()
        .transition_source_read(SourceReadStageState::Collecting)
        .unwrap()
        .transition_source_read(SourceReadStageState::Answered {
            answer: Box::new(SourceReadAnswer::TemporaryResources(Box::new(answer))),
        })
        .unwrap();
    let revision = DiagnosticRevision::between(None, episode, "2020-01-01T12:00:12Z").unwrap().unwrap();
    let parent = basis_refs[0].capture_ref();
    let mut checkpoint = DiagnosticCheckpointPart {
        id: String::new(),
        controller_version: DIAGNOSTIC_CONTROLLER_VERSION,
        target: basis_source.target.clone(),
        input: parent.clone(),
        part: 0,
        parts: 1,
        episodes: vec![revision.episode.clone()],
    };
    checkpoint.id = checkpoint.expected_id().unwrap();
    let source = CollectionDescription {
        target: basis_source.target,
        partition: SourcePartition::Target,
        source: DIAGNOSTICS_SOURCE.into(),
        continuity: SourceContinuity::Unverified,
        observation: SourceObservation::Diagnostics(DiagnosticCaptureDescription {
            controller_version: DIAGNOSTIC_CONTROLLER_VERSION,
            parent,
            step_id: digest(b"temporary-answer-step"),
            analyzed_at: revision.analyzed_at.clone(),
            records: 1,
            checkpoint_records: 1,
            scope_bindings: [(revision.episode.scope.key().unwrap(), revision.episode.scope.clone())].into(),
        }),
    };
    let record = TelemetryRecord::Operational(OperationalObservation {
        source: DIAGNOSTICS_SOURCE.into(),
        key: revision.id.clone(),
        fact: OperationalFact::DiagnosticRevision(Box::new(revision)),
    });
    assert!(record.is_valid() && checkpoint.is_valid());
    (source, vec![record, TelemetryRecord::DiagnosticCheckpoint(checkpoint)])
}
