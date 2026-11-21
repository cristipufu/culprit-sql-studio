use super::*;

struct Originals {
    original_source: CollectionDescription,
    original: ExportedRecord,
    focus: SourceReadFocus,
    source: CollectionDescription,
    capture: DiagnosticCaptureRef,
    rows: Vec<(DiagnosticEvidenceRef, ExportedRecord)>,
}
impl Originals {
    fn answer(&self) -> Result<QueryCompilationAnswer> {
        QueryCompilationAnswer::from_originals(
            &self.focus,
            &self.original_source,
            &self.original,
            &self.source,
            &self.capture,
            &self.rows.iter().map(|(reference, row)| (reference, row)).collect::<Vec<_>>(),
        )
    }
    fn replace_points(&mut self, points: Vec<MssqlQueryCompilation>, complete: bool) {
        let SourceObservation::SourceRead { snapshot, .. } = &mut self.source.observation else { panic!() };
        snapshot.records = points.len() as u32;
        snapshot.sources = vec![StateSourceCoverage::observed(
            crate::source_id::QUERY_COMPILATION.into(),
            points.len() as u32,
            complete,
        )];
        self.rows = points
            .into_iter()
            .enumerate()
            .map(|(ordinal, point)| {
                let row = ExportedRecord::new(
                    &self.source,
                    &self.capture.capture,
                    TelemetryRecord::Operational(OperationalObservation {
                        source: crate::source_id::QUERY_COMPILATION.into(),
                        key: point.key().unwrap(),
                        fact: OperationalFact::MssqlQueryCompilation(Box::new(point)),
                    }),
                )
                .unwrap();
                (
                    DiagnosticEvidenceRef::from_export(
                        &self.source,
                        &self.capture.manifest_sha256,
                        &row,
                        ordinal as u32,
                    )
                    .unwrap(),
                    row,
                )
            })
            .filter(|(_, row)| self.focus.retains_result_record(&row.record))
            .collect();
    }
}
fn query() -> NativeQueryIdentity {
    NativeQueryIdentity {
        query_id: "3".into(),
        qualifiers: [
            ("context_settings_id".into(), "5".into()),
            ("object_id".into(), "0".into()),
            ("query_hash".into(), "0123456789ABCDEF".into()),
            ("initial_compile_at".into(), "2020-01-01T10:00:00.000000000Z".into()),
        ]
        .into(),
    }
}
fn point() -> MssqlQueryCompilation {
    MssqlQueryCompilation {
        identity: query(),
        statement: DefinitionPayload::Unavailable { reason: PayloadUnavailable::NotCollected },
        origin: StatementOrigin::Application,
        last_compile_at: "2020-01-01T11:00:00.000000000Z".into(),
        compilations: "9007199254740993".into(),
        average_duration_us: 123.5,
        timings: MssqlCompilationTimings::default(),
        memory: None,
        seconds_since_first_compile: 7203,
        reset: crate::ResetMarker::Unknown,
    }
}
fn originals() -> Originals {
    let mut context = crate::telemetry::native_read_context::tests::read_context();
    let CollectionTarget::Database(database) = &mut context.target else { panic!() };
    database.incarnation = Some("owned-native-logical-database".into());
    let interval = TelemetryInterval {
        native_id: "8".into(),
        from: "2020-01-01T11:00:00.000000000Z".into(),
        until: "2020-01-01T12:00:00.000000000Z".into(),
    };
    let original_source = CollectionDescription {
        target: context.target.clone(),
        partition: SourcePartition::Target,
        source: crate::source_id::QUERY_STORE.into(),
        continuity: SourceContinuity::Unverified,
        observation: SourceObservation::QueryHistory(QueryHistoryDescription {
            interval,
            acquisition_owner: Some(Box::new(context.clone())),
            population_catalog: None,
            population_binding: QueryHistoryPopulationBinding::Unknown,
            waits: None,
            dimensions: Default::default(),
            native_properties: Default::default(),
        }),
    };
    let query = query();
    let plan = NativePlanIdentity {
        plan_id: "7".into(),
        query: query.clone(),
        qualifiers: [("initial_compile_at".into(), "2020-01-01T10:00:01.000000000Z".into())].into(),
    };
    let references = StatementReferences {
        query: Some(NativeDefinitionReference {
            native: query.native(),
            definition_id: Some(query.definition_id(&original_source).unwrap()),
        }),
        plan: Some(NativeDefinitionReference {
            native: plan.native(),
            definition_id: Some(plan.definition_id(&original_source).unwrap()),
        }),
    };
    let native = NativeIdentity::new("101").qualified("plan_id", "7").qualified("execution_type", "4");
    let mut cohort = native.clone();
    cohort.id = "query-store-execution-population".into();
    cohort.qualifiers.remove("plan_id");
    let capture = CaptureIdentity {
        observation_id: "owned-original-history".into(),
        revision: Some(CaptureRevision {
            outbox_id: "owned-outbox".into(),
            owner_epoch: "1".into(),
            sequence: "1".into(),
        }),
    };
    let original = ExportedRecord::new(
        &original_source,
        &capture,
        TelemetryRecord::QueryRuntime(RuntimeMeasurement {
            key: NativeMeasurementKey { native, references, cohort: Some(cohort) },
            executions: "2".into(),
            metrics: vec![],
            page_server_reads: None,
            first_execution_at: None,
            last_execution_at: None,
        }),
    )
    .unwrap();
    let basis =
        DiagnosticEvidenceRef::from_export(&original_source, &digest(b"original-history-manifest"), &original, 0)
            .unwrap();
    let focus = SourceReadFocus {
        basis: Box::new(basis),
        target: original_source.target.clone(),
        question: SourceReadQuestion::CompilationHistory,
        prerequisite: SourceReadPrerequisite::ObservedQueryHistoryFailure { query: query.clone() },
    };
    context.observed_from = "2020-01-01T12:00:02.000000000Z".into();
    context.observed_until = "2020-01-01T12:00:03.000000000Z".into();
    let source = CollectionDescription {
        target: original_source.target.clone(),
        partition: SourcePartition::Target,
        source: "query-compilation-state".into(),
        continuity: SourceContinuity::Unverified,
        observation: SourceObservation::SourceRead {
            snapshot: StateSnapshotDescription {
                dataset: SnapshotDataset::QueryCompilation,
                records: 0,
                observed_from: context.observed_from.clone(),
                observed_until: context.observed_until.clone(),
                sources: vec![StateSourceCoverage::collected(crate::source_id::QUERY_COMPILATION.into(), 0)],
            },
            focus_key: focus.key().unwrap(),
            execution: Box::new(context),
        },
    };
    let capture = DiagnosticCaptureRef {
        capture: CaptureIdentity { observation_id: "owned-compilation-answer".into(), revision: capture.revision },
        manifest_sha256: digest(b"compilation-manifest"),
        source_scope_id: source.captured_range().unwrap().scope_id,
    };
    let mut values = Originals { original_source, original, focus, source, capture, rows: vec![] };
    values.replace_points(vec![point()], true);
    values
}

#[test]
fn exact_query_incarnation_keeps_original_plan_population_and_independent_lifetime_totals_without_cause() {
    let values = originals();
    let answer = values.answer().unwrap();
    assert!(answer.is_valid());
    assert_eq!(answer.execution_type, 4);
    assert_eq!(answer.original_interval.native_id, "8");
    assert_eq!(answer.plan.plan_id, "7");
    assert_eq!(answer.unresolved, QueryCompilationGap::ALL);
    let QueryCompilationSelection::Observed { observation, counters } = &answer.selection else { panic!() };
    assert_eq!(observation, &values.rows[0].0);
    assert_eq!(counters.compilations, "9007199254740993");
    assert_eq!(counters.average_duration_us, 123.5);
    assert_eq!(counters.reset, crate::ResetMarker::Unknown);
    assert_eq!(answer.references().count(), 2);
    let wire = serde_json::to_value(&answer).unwrap();
    assert!(wire.get("harmful_reuse").is_none());
    assert!(wire.get("statement").is_none());
    assert_eq!(serde_json::from_value::<QueryCompilationAnswer>(wire).unwrap(), answer);
}
#[test]
fn reused_query_number_and_changed_context_hash_object_or_initial_compile_never_replace_original() {
    for (qualifier, replacement) in [
        ("initial_compile_at", "2020-01-01T12:00:02.000000000Z"),
        ("context_settings_id", "6"),
        ("object_id", "1999"),
        ("query_hash", "FEDCBA9876543210"),
    ] {
        let mut values = originals();
        let mut changed = point();
        changed.identity.qualifiers.insert(qualifier.into(), replacement.into());
        values.replace_points(vec![changed], true);
        assert_eq!(values.answer().unwrap().selection, QueryCompilationSelection::IncarnationChanged, "{qualifier}");
    }
    let mut values = originals();
    let mut unrelated = point();
    unrelated.identity.query_id = "99".into();
    values.replace_points(vec![unrelated], true);
    assert_eq!(values.answer().unwrap().selection, QueryCompilationSelection::NotObserved);
}
#[test]
fn selected_row_keeps_original_position_and_ambiguous_native_query_candidates_refuse() {
    let mut values = originals();
    let mut other = point();
    other.identity.query_id = "2".into();
    values.replace_points(vec![other, point()], true);
    let QueryCompilationSelection::Observed { observation, .. } = values.answer().unwrap().selection else { panic!() };
    assert_eq!(observation.row_ordinal, 1);
    values.rows[0].0.row_ordinal = 2;
    assert!(values.answer().is_err());
    let mut values = originals();
    values.replace_points(vec![point(), point()], true);
    assert!(values.answer().is_err(), "ambiguous complete rosters yield no plausible survivor");
    let mut values = originals();
    let SourceObservation::SourceRead { snapshot, .. } = &mut values.source.observation else { panic!() };
    snapshot.records = MAX_QUERY_COMPILATION_OBSERVATIONS as u32 + 1;
    snapshot.sources =
        vec![StateSourceCoverage::collected(crate::source_id::QUERY_COMPILATION.into(), snapshot.records)];
    assert!(values.answer().is_err(), "filtering cannot widen the complete native roster ceiling");
}
#[test]
fn partial_empty_and_unavailable_sources_retain_unknowns_instead_of_false_recovery_or_healthy_zero() {
    let mut values = originals();
    values.replace_points(vec![point()], false);
    let answer = values.answer().unwrap();
    assert_eq!(answer.source_state, crate::SourceState::Partial);
    assert!(matches!(answer.selection, QueryCompilationSelection::Observed { .. }));
    values.replace_points(vec![], true);
    let answer = values.answer().unwrap();
    assert_eq!(answer.source_state, crate::SourceState::Empty);
    assert_eq!(answer.selection, QueryCompilationSelection::NotObserved);
    let SourceObservation::SourceRead { snapshot, .. } = &mut values.source.observation else { panic!() };
    snapshot.sources = vec![StateSourceCoverage::unavailable(
        crate::source_id::QUERY_COMPILATION.into(),
        crate::ErrorCode::PermissionDenied,
        None,
    )];
    let answer = values.answer().unwrap();
    assert_eq!(answer.source_state, crate::SourceState::Unavailable);
    assert_eq!(answer.selection, QueryCompilationSelection::NotObserved);
    assert_eq!(answer.unresolved, QueryCompilationGap::ALL);
}
#[test]
fn missing_namespace_acquisition_owner_process_or_role_withholds_current_read_authority() {
    for missing in ["namespace", "owner", "process", "role"] {
        let mut values = originals();
        match missing {
            "namespace" => {
                let CollectionTarget::Database(db) = &mut values.original_source.target else { panic!() };
                db.incarnation = None;
            }
            _ => {
                let SourceObservation::QueryHistory(history) = &mut values.original_source.observation else {
                    panic!()
                };
                match missing {
                    "owner" => history.acquisition_owner = None,
                    "role" => history.acquisition_owner.as_mut().unwrap().database_writable = None,
                    _ => {
                        let SourcePartition::InstanceLocal { epoch, .. } =
                            &mut history.acquisition_owner.as_mut().unwrap().partition
                        else {
                            panic!()
                        };
                        *epoch = None;
                    }
                }
            }
        }
        assert!(values.answer().is_err(), "{missing}");
    }
}
#[test]
fn foreign_or_changed_native_read_owner_and_backward_bounds_cannot_answer_original_namespace() {
    for changed in ["database", "process", "role", "clock"] {
        let mut values = originals();
        let SourceObservation::SourceRead { execution, .. } = &mut values.source.observation else { panic!() };
        match changed {
            "database" => {
                let CollectionTarget::Database(db) = &mut execution.target else { panic!() };
                db.incarnation = Some("foreign-db".into());
            }
            "process" => {
                let SourcePartition::InstanceLocal { epoch, .. } = &mut execution.partition else { panic!() };
                *epoch = Some("replacement-process".into());
            }
            "role" => execution.database_writable = Some(false),
            _ => {
                execution.observed_from = "2020-01-01T11:59:59.000000000Z".into();
            }
        }
        assert!(values.answer().is_err(), "{changed}");
    }
}
#[test]
fn answer_requires_original_receipts_and_cannot_drop_competing_evidence_or_relabel_counters() {
    let mut values = originals();
    values.rows[0].0.payload_sha256 = digest(b"other-content");
    assert!(values.answer().is_err());
    let mut answer = originals().answer().unwrap();
    answer.unresolved.pop();
    assert!(!answer.is_valid());
    let mut answer = originals().answer().unwrap();
    let QueryCompilationSelection::Observed { counters, .. } = &mut answer.selection else { panic!() };
    counters.identity.qualifiers.insert("initial_compile_at".into(), "2020-01-01T10:01:00.000000000Z".into());
    assert!(!answer.is_valid());
}

#[test]
fn sealed_question_selects_changed_incarnation_candidates_but_cannot_change_the_original_query_context() {
    let values = originals();
    let mut changed = point();
    changed.identity.qualifiers.insert("context_settings_id".into(), "6".into());
    let candidate = TelemetryRecord::Operational(OperationalObservation {
        source: crate::source_id::QUERY_COMPILATION.into(),
        key: changed.key().unwrap(),
        fact: OperationalFact::MssqlQueryCompilation(Box::new(changed)),
    });
    assert!(values.focus.retains_result_record(&candidate), "keep a changed native incarnation for explicit refusal");
    let mut other = point();
    other.identity.query_id = "99".into();
    let unrelated = TelemetryRecord::Operational(OperationalObservation {
        source: crate::source_id::QUERY_COMPILATION.into(),
        key: other.key().unwrap(),
        fact: OperationalFact::MssqlQueryCompilation(Box::new(other)),
    });
    assert!(!values.focus.retains_result_record(&unrelated));
    let mut foreign = unrelated.clone();
    let TelemetryRecord::Operational(row) = &mut foreign else { panic!() };
    row.source = "foreign-native-profile".into();
    assert!(
        values.focus.retains_result_record(&foreign),
        "foreign families must reach refusal, not disappear as unrelated queries"
    );
    let mut forged = values.focus.clone();
    let SourceReadPrerequisite::ObservedQueryHistoryFailure { query } = &mut forged.prerequisite else { panic!() };
    query.qualifiers.insert("context_settings_id".into(), "6".into());
    let execution = SourceReadExecutionScope::from_context(
        values.focus.question,
        &values.focus.original_context(&values.original_source).unwrap(),
    )
    .unwrap();
    assert!(!forged.matches_basis(&values.original_source, &values.original, &execution));
    let update = observe_diagnostic_original(
        &values.original_source,
        &values.focus.basis.capture_ref(),
        &[DiagnosticOriginalRow { reference: &values.focus.basis, record: &values.original.record }],
        None,
        "2020-01-01T12:00:01.000000000Z",
    )
    .unwrap();
    let episode = &update.checkpoint.episodes[0];
    let stage = SourceReadStage { focus: values.focus.clone(), state: SourceReadStageState::Pending };
    assert!(stage.matches_episode(episode));
    let wrong = SourceReadStage { focus: forged, state: SourceReadStageState::Pending };
    assert!(!wrong.matches_episode(episode));
    assert!(!SourceReadStage { state: SourceReadStageState::Captured { capture: values.capture }, ..stage }.is_valid());
}
