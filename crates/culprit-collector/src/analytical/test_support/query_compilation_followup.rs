//! Controlled Query Store originals exercise the real question/answer contracts, not native cost qualification.
use super::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QueryCompilationFollowupCase {
    Exact,
    Partial,
    Changed,
    Missing,
    Unavailable,
    LargeUnrelated,
}
impl QueryCompilationFollowupCase {
    pub const ALL: &'static [Self] = &[Self::Exact, Self::Partial, Self::Changed, Self::Missing, Self::Unavailable];
}

pub fn query_compilation_followup_result(
    original_source: &CollectionDescription,
    original_records: &[TelemetryRecord],
    focus: &SourceReadFocus,
    execution: &SourceReadExecutionScope,
    case: QueryCompilationFollowupCase,
) -> (CollectionDescription, Vec<TelemetryRecord>) {
    let query = original_records
        .iter()
        .find_map(|row| match DiagnosticOriginalCondition::from_record(original_source, row) {
            Some(DiagnosticOriginalCondition::MssqlQueryHistory { query, .. }) => Some(query),
            _ => None,
        })
        .expect("original failed native Query Store query");
    let (_, mut rows) = query_compilation_evidence();
    let TelemetryRecord::Operational(row) = &mut rows[0] else { unreachable!() };
    let OperationalFact::MssqlQueryCompilation(point) = &mut row.fact else { unreachable!() };
    point.identity = NativeQueryIdentity { query_id: query.id, qualifiers: query.qualifiers };
    if case == QueryCompilationFollowupCase::Changed {
        let at = chrono::DateTime::parse_from_rfc3339(&point.identity.qualifiers["initial_compile_at"]).unwrap()
            + chrono::Duration::seconds(1);
        point.identity.qualifiers.insert("initial_compile_at".into(), at.to_rfc3339());
    }
    row.key = point.key().unwrap();
    if case == QueryCompilationFollowupCase::LargeUnrelated {
        // Valid, inert SQL definitions only. No statement is executed; this tests paid canonical-file work.
        let sql = format!(
            "SELECT {}",
            (0..1024).map(|column| format!("0 AS [column_{column}_{}]", "a".repeat(100))).collect::<Vec<_>>().join(",")
        );
        let selected = rows.pop().unwrap();
        let mut unrelated = Vec::new();
        for query in 10000..10040 {
            let mut record = selected.clone();
            let TelemetryRecord::Operational(native) = &mut record else { unreachable!() };
            let OperationalFact::MssqlQueryCompilation(point) = &mut native.fact else { unreachable!() };
            point.identity.query_id = query.to_string();
            point.statement = DefinitionPayload::sql(&sql, culprit_core::Engine::Mssql);
            native.key = point.key().unwrap();
            unrelated.push(record);
        }
        unrelated.insert(20, selected);
        rows = unrelated;
    }

    if matches!(case, QueryCompilationFollowupCase::Missing | QueryCompilationFollowupCase::Unavailable) {
        rows.clear()
    }
    let source = culprit_core::source_id::QUERY_COMPILATION;
    let coverage = if case == QueryCompilationFollowupCase::Unavailable {
        StateSourceCoverage::unavailable(source.into(), culprit_core::ErrorCode::PermissionDenied, None)
    } else {
        StateSourceCoverage::observed(source.into(), rows.len() as u32, case != QueryCompilationFollowupCase::Partial)
    };
    let context = NativeReadContext {
        target: focus.target.clone(),
        partition: execution.partition().clone(),
        database_writable: Some(execution.database_writable()),
        observed_from: "2020-01-01T12:00:02.000000000Z".into(),
        observed_until: "2020-01-01T12:00:03.000000000Z".into(),
    };
    let source = CollectionDescription {
        target: focus.target.clone(),
        partition: SourcePartition::Target,
        source: "query-compilation-state".into(),
        continuity: SourceContinuity::Unverified,
        observation: SourceObservation::SourceRead {
            snapshot: StateSnapshotDescription {
                dataset: SnapshotDataset::QueryCompilation,
                records: rows.len() as u32,
                observed_from: context.observed_from.clone(),
                observed_until: context.observed_until.clone(),
                sources: vec![coverage],
            },
            focus_key: focus.key().unwrap(),
            execution: Box::new(context),
        },
    };
    (source, rows)
}

pub struct QueryCompilationOriginal {
    pub source: CollectionDescription,
    pub records: Vec<TelemetryRecord>,
    pub original: ExportedRecord,
    pub episode: DiagnosticEpisode,
    pub focus: SourceReadFocus,
    pub execution: SourceReadExecutionScope,
}

pub fn query_compilation_followup_original() -> QueryCompilationOriginal {
    let (source, records) = diagnostic_original_evidence(DiagnosticOriginalCase::HistoryException);
    let capture = CaptureIdentity {
        observation_id: "query-history-compilation-basis".into(),
        revision: Some(CaptureRevision {
            outbox_id: "query-history-compilation-outbox".into(),
            owner_epoch: "1".into(),
            sequence: "1".into(),
        }),
    };
    let original =
        records.iter().cloned().find(|row| DiagnosticOriginalCondition::from_record(&source, row).is_some()).unwrap();
    let original = ExportedRecord::new(&source, &capture, original).unwrap();
    let basis = DiagnosticEvidenceRef::from_export(&source, &digest(b"original-query-history"), &original, 0).unwrap();
    let update = observe_diagnostic_original(
        &source,
        &basis.capture_ref(),
        &[DiagnosticOriginalRow { reference: &basis, record: &original.record }],
        None,
        "2020-01-01T12:00:01.000000000Z",
    )
    .unwrap();
    let episode = update.checkpoint.episodes[0].clone();
    let focus = episode.source_read_focus().unwrap().unwrap();
    let execution =
        SourceReadExecutionScope::from_context(focus.question, &focus.original_context(&source).unwrap()).unwrap();

    QueryCompilationOriginal { source, records, original, episode, focus, execution }
}

pub fn query_compilation_followup_evidence(
    case: QueryCompilationFollowupCase,
) -> (CollectionDescription, Vec<TelemetryRecord>) {
    let QueryCompilationOriginal { source, original, episode, focus, execution, .. } =
        query_compilation_followup_original();
    let capture = original.capture.clone();
    let enrolled = episode.enroll_source_read(focus.clone()).unwrap();
    let collecting = enrolled.transition_source_read(SourceReadStageState::Collecting).unwrap();
    let fixture = investigation_fixture();
    let component = fixture.intent.qualification.component;
    let mut recipe = fixture.recipe;
    recipe.id = "fixture.query-history-compilation-context".into();
    recipe.selector = focus.question.selector();
    let RecipeCostQualification::Measured(receipt) = &mut recipe.cost else { unreachable!() };
    receipt.recipe_id = recipe.id.clone();
    receipt.selector = recipe.selector;
    receipt.scope = RecipeCostScope::NativeRead(execution.clone());
    let intent = DiagnosticInvestigationIntent::new_source_read(
        &collecting,
        &recipe,
        focus.clone(),
        execution.clone(),
        &component,
        &recipe.budget,
    )
    .unwrap();
    let (result_source, result_rows) =
        query_compilation_followup_result(&source, &[original.record.clone()], &focus, &execution, case);
    let result_capture =
        CaptureIdentity { observation_id: "query-history-compilation-result".into(), revision: capture.revision };
    let parent = DiagnosticCaptureRef {
        capture: result_capture.clone(),
        manifest_sha256: digest(b"original-compilation-result"),
        source_scope_id: result_source.captured_range().unwrap().scope_id,
    };
    let exports = result_rows
        .into_iter()
        .map(|row| ExportedRecord::new(&result_source, &result_capture, row).unwrap())
        .collect::<Vec<_>>();
    let refs = exports
        .iter()
        .enumerate()
        .map(|(ordinal, row)| {
            DiagnosticEvidenceRef::from_export(&result_source, &parent.manifest_sha256, row, ordinal as u32).unwrap()
        })
        .collect::<Vec<_>>();
    let rows =
        refs.iter().zip(&exports).filter(|(_, row)| focus.retains_result_record(&row.record)).collect::<Vec<_>>();
    let answered = collecting
        .complete_source_read_from_originals(&intent, &source, &original, &result_source, &parent, &rows)
        .unwrap();
    let at = "2020-01-01T12:00:04.000000000Z";
    let revision = DiagnosticRevision::with_investigation(
        &collecting,
        answered,
        DiagnosticInvestigationEvent {
            intent: Box::new(intent),
            attempt: Some(DiagnosticAttemptReceipt {
                capture: result_capture,
                manifest_sha256: Some(parent.manifest_sha256.clone()),
            }),
            source_work: SourceWorkState::Quiescent,
            outcome: DiagnosticInvestigationOutcome::Captured { capture: parent.clone() },
        },
        at,
    )
    .unwrap()
    .unwrap();
    let mut checkpoint = DiagnosticCheckpointPart {
        id: String::new(),
        controller_version: DIAGNOSTIC_CONTROLLER_VERSION,
        target: source.target.clone(),
        input: parent.clone(),
        part: 0,
        parts: 1,
        episodes: vec![revision.episode.clone()],
    };
    checkpoint.id = checkpoint.expected_id().unwrap();
    assert!(checkpoint.is_valid());
    let diagnostic_source = CollectionDescription {
        target: source.target,
        partition: SourcePartition::Target,
        source: DIAGNOSTICS_SOURCE.into(),
        continuity: SourceContinuity::Unverified,
        observation: SourceObservation::Diagnostics(DiagnosticCaptureDescription {
            controller_version: DIAGNOSTIC_CONTROLLER_VERSION,
            parent,
            step_id: digest(b"query-history-compilation-answer"),
            analyzed_at: at.into(),
            records: 1,
            checkpoint_records: 1,
            scope_bindings: [(revision.episode.scope.key().unwrap(), revision.episode.scope.clone())].into(),
        }),
    };
    let records = vec![
        TelemetryRecord::Operational(OperationalObservation {
            source: DIAGNOSTICS_SOURCE.into(),
            key: revision.id.clone(),
            fact: OperationalFact::DiagnosticRevision(Box::new(revision)),
        }),
        TelemetryRecord::DiagnosticCheckpoint(checkpoint),
    ];
    assert!(records.iter().all(TelemetryRecord::is_valid));
    (diagnostic_source, records)
}

/// This inspects the retained answer only; referenced execution/plan bodies are not silently reopened.
pub fn assert_query_compilation_followup_capture(
    capture: &crate::analytical::evidence::EvidenceCapture,
    source: &CollectionDescription,
    records: &[TelemetryRecord],
    case: QueryCompilationFollowupCase,
) {
    assert_diagnostic_capture(capture, source, records);
    let TelemetryRecord::Operational(native) = &capture.records()[0].row().record else { unreachable!() };
    let OperationalFact::DiagnosticRevision(revision) = &native.fact else { unreachable!() };
    let SourceReadStageState::Answered { answer } = &revision.episode.source_read.as_ref().unwrap().state else {
        unreachable!()
    };
    let SourceReadAnswer::QueryCompilation(answer) = answer.as_ref() else { unreachable!() };
    assert_eq!(answer.unresolved, QueryCompilationGap::ALL);
    assert_eq!(answer.basis, revision.episode.first);
    assert!(
        revision.inputs.iter().all(|reference| reference.capture_ref() == answer.result || reference == &answer.basis)
    );
    assert!(revision.episode.recovery.is_none());
    match case {
        QueryCompilationFollowupCase::Exact
        | QueryCompilationFollowupCase::Partial
        | QueryCompilationFollowupCase::LargeUnrelated => {
            let QueryCompilationSelection::Observed { observation, counters } = &answer.selection else { panic!() };
            assert_eq!(counters.compilations, "9007199254740993");
            assert_eq!(observation.capture_ref(), answer.result);
            assert!(revision.inputs.contains(observation));
        }
        QueryCompilationFollowupCase::Changed => {
            assert_eq!(answer.selection, QueryCompilationSelection::IncarnationChanged)
        }
        _ => assert_eq!(answer.selection, QueryCompilationSelection::NotObserved),
    }
    if case == QueryCompilationFollowupCase::Partial {
        assert_eq!(answer.source_state, culprit_core::SourceState::Partial)
    }
    if case == QueryCompilationFollowupCase::Unavailable {
        assert_eq!(answer.source_state, culprit_core::SourceState::Unavailable)
    }
}
