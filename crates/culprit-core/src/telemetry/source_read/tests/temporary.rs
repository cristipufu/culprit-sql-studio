use super::*;

struct Fixture {
    source: CollectionDescription,
    original: ExportedRecord,
    episode: DiagnosticEpisode,
    focus: SourceReadFocus,
    execution: SourceReadExecutionScope,
}
fn fixture(snapshot_protector: bool) -> Fixture {
    let mut context = super::super::super::native_read_context::tests::read_context();
    context.target = DatabaseIdentity::server(Engine::Mssql, "sample", "owned.invalid", 1433, None).into();
    let source = CollectionDescription {
        target: context.target.clone(),
        partition: context.partition.clone(),
        source: "native-cleanup-capture".into(),
        continuity: SourceContinuity::Unverified,
        observation: SourceObservation::LocalResource {
            snapshot: StateSnapshotDescription {
                dataset: SnapshotDataset::Cleanup,
                observed_from: context.observed_from.clone(),
                observed_until: context.observed_until.clone(),
                records: 1,
                sources: vec![StateSourceCoverage::observed("native-cleanup-horizon".into(), 1, true)],
            },
            database_writable: true,
        },
    };
    let transaction = MssqlCleanupTransaction {
        session: NativeActivityIdentity { id: "57".into(), generation: "2020-01-01T10:00:00.003".into() },
        transaction: NativeActivityIdentity { id: "1001".into(), generation: "2020-01-01T11:00:00.003".into() },
        observed_time_without_offset: "2020-01-01T12:00:00.003".into(),
        age_seconds: Some("3600".into()),
        program: None,
        native_status: None,
        log_record_count: Some("1".into()),
        log_bytes_used: Some("9007199254740993".into()),
        begin_lsn: None,
        row_version_participant: snapshot_protector,
        snapshot_isolation: snapshot_protector.then_some(true),
        transaction_sequence_number: snapshot_protector.then(|| "123".into()),
        versioning_age_seconds: snapshot_protector.then(|| "300".into()),
    };
    let capture = capture("cleanup-original");
    let original = ExportedRecord::new(
        &source,
        &capture,
        TelemetryRecord::Operational(OperationalObservation {
            source: "native-cleanup-horizon".into(),
            key: "native-oldest-boundary".into(),
            fact: OperationalFact::MssqlCleanupHorizon(MssqlCleanupHorizon {
                oldest_confirmed: Some(Box::new(transaction)),
                oldest_unattributed_versioning: None,
            }),
        }),
    )
    .unwrap();
    let reference =
        DiagnosticEvidenceRef::from_export(&source, &digest(b"cleanup-original-manifest"), &original, 0).unwrap();
    let condition = DiagnosticOriginalCondition::from_record(&source, &original.record).unwrap();
    let mut episode = DiagnosticEpisode::start(
        DiagnosticScope::from_source(&source).unwrap(),
        DiagnosticSignal::OriginalObservation { condition, native_key: original.record.native_identity() },
        context.observed_until.clone(),
        reference.clone(),
    )
    .unwrap();
    episode.report = Some(DiagnosticReport::OriginalObservation { observation: reference, peers: vec![] });
    episode.analysis = DiagnosticAnalysisStatus::Unresolved;
    episode.issue = Some(DiagnosticAnalysisIssue::Unsupported);
    let focus = episode.source_read_focus().unwrap().unwrap();
    let execution = SourceReadExecutionScope::from_context(focus.question, &context).unwrap();
    assert!(episode.is_valid());
    Fixture { source, original, episode, focus, execution }
}
fn capture(id: &str) -> CaptureIdentity {
    CaptureIdentity {
        observation_id: id.into(),
        revision: Some(CaptureRevision {
            outbox_id: "original-outbox".into(),
            owner_epoch: "1".into(),
            sequence: "1".into(),
        }),
    }
}
fn task(execution: u32) -> TaskSpaceObservation {
    let mut request = super::super::super::activity::tests::request("42");
    request.statement = DefinitionPayload::sql("SELECT value FROM dbo.items WHERE id=9274", Engine::Mssql);
    request.statement_identity = Some(NativeIdentity::new("original-statement-handle"));
    TaskSpaceObservation {
        source: crate::source_id::TASK_SPACE.into(),
        observed_at: "2020-01-01T12:00:02.2Z".into(),
        task: TaskSpaceIdentity {
            session_id: "42".into(),
            request_id: "0".into(),
            execution_context_id: execution.to_string(),
        },
        native_database_id: "2".into(),
        bytes_per_page: 8192,
        user_allocated_pages: Some("9007199254740993".into()),
        user_deallocated_pages: Some("9007199254740993".into()),
        internal_allocated_pages: Some("32".into()),
        internal_deallocated_pages: Some("0".into()),
        request: Some(request),
        before_check: Some(TaskRequestCheck {
            observed_at: "2020-01-01T12:00:02.3Z".into(),
            state: TaskRequestState::SameStatement,
        }),
        after_check: Some(TaskRequestCheck {
            observed_at: "2020-01-01T12:00:02.7Z".into(),
            state: TaskRequestState::SameStatement,
        }),
        statement_identity: Some(NativeIdentity::new("original-statement-handle")),
        plan_identity: None,
    }
}
fn result(
    fixture: &Fixture,
    tasks: Vec<TaskSpaceObservation>,
    state: crate::SourceState,
) -> (CollectionDescription, DiagnosticCaptureRef, Vec<(DiagnosticEvidenceRef, ExportedRecord)>) {
    let execution = NativeReadContext {
        target: fixture.source.target.clone(),
        partition: fixture.source.partition.clone(),
        database_writable: Some(true),
        observed_from: "2020-01-01T12:00:02Z".into(),
        observed_until: "2020-01-01T12:00:03Z".into(),
    };
    let mut coverage = StateSourceCoverage::observed(
        crate::source_id::TASK_SPACE.into(),
        tasks.len() as u32,
        state != crate::SourceState::Partial,
    );
    if state == crate::SourceState::Unavailable {
        coverage = StateSourceCoverage::unavailable(
            crate::source_id::TASK_SPACE.into(),
            crate::ErrorCode::PermissionDenied,
            None,
        );
    }
    let source = CollectionDescription {
        target: fixture.source.target.clone(),
        partition: SourcePartition::Target,
        source: crate::source_id::TASK_SPACE.into(),
        continuity: SourceContinuity::Unverified,
        observation: SourceObservation::SourceRead {
            snapshot: StateSnapshotDescription {
                dataset: SnapshotDataset::TaskSpace,
                observed_from: execution.observed_from.clone(),
                observed_until: execution.observed_until.clone(),
                records: tasks.len() as u32,
                sources: vec![coverage],
            },
            execution: Box::new(execution),
            focus_key: fixture.focus.key().unwrap(),
        },
    };
    let capture = capture("task-space-answer");
    let manifest = digest(b"task-space-answer-manifest");
    let rows = tasks
        .into_iter()
        .enumerate()
        .map(|(ordinal, task)| {
            let record = TelemetryRecord::Operational(OperationalObservation {
                source: task.source.clone(),
                key: task.key().unwrap(),
                fact: OperationalFact::TaskSpace(Box::new(task)),
            });
            let row = ExportedRecord::new(&source, &capture, record).unwrap();
            let reference = DiagnosticEvidenceRef::from_export(&source, &manifest, &row, ordinal as u32).unwrap();
            (reference, row)
        })
        .collect();
    let capture = DiagnosticCaptureRef {
        capture,
        manifest_sha256: manifest,
        source_scope_id: source.captured_range().unwrap().scope_id,
    };
    (source, capture, rows)
}
fn answer(fixture: &Fixture, tasks: Vec<TaskSpaceObservation>, state: crate::SourceState) -> TemporaryResourceAnswer {
    let (source, capture, rows) = result(fixture, tasks, state);
    let rows = rows.iter().map(|(reference, row)| (reference, row)).collect::<Vec<_>>();
    TemporaryResourceAnswer::from_originals(
        &fixture.focus,
        &fixture.source,
        &fixture.original,
        &source,
        &capture,
        &rows,
    )
    .unwrap()
}
fn contribution(
    answer: &TemporaryResourceAnswer,
    mechanism: TemporaryResourceMechanism,
) -> &TemporaryResourceContribution {
    answer.contributions.iter().find(|entry| entry.mechanism() == mechanism).unwrap()
}

#[test]
fn local_scope_uses_original_native_generation_without_inventing_global_history_identity() {
    let original = fixture(true);
    assert!(original.source.target.database().unwrap().incarnation.is_none());
    let context = NativeReadContext::from_snapshot(&original.source).unwrap();
    assert!(NativeSourceReadScope::from_context(&context).is_err());
    assert!(original.execution.matches_context(&context));
    assert!(original.focus.matches_basis(&original.source, &original.original, &original.execution));
    assert!(DiagnosticFocus::SourceRead { focus: Box::new(original.focus) }.activity_participant().is_none());
    for defect in ["member", "epoch", "role"] {
        let mut changed = context.clone();
        match defect {
            "member" => {
                let SourcePartition::InstanceLocal { member: Some(member), .. } = &mut changed.partition else {
                    panic!()
                };
                member.native.id = "foreign".into();
            }
            "epoch" => {
                let SourcePartition::InstanceLocal { epoch, .. } = &mut changed.partition else { panic!() };
                *epoch = Some("new-native-generation".into());
            }
            "role" => changed.database_writable = None,
            _ => unreachable!(),
        }
        assert!(!original.execution.matches_context(&changed), "{defect}");
    }
}
#[test]
fn historical_unknown_or_changed_role_is_not_repaired_by_new_execution_authority() {
    let original = fixture(true);
    let mut historical = original.source.clone();
    historical.observation = SourceObservation::Snapshot(historical.observation.snapshot().unwrap().clone());
    let before = NativeReadContext::from_snapshot(&historical).unwrap();
    assert_eq!(before.database_writable, None);
    assert!(original.execution.matches_original_context(&before));
    let mut current = before.clone();
    current.database_writable = Some(false);
    let current = SourceReadExecutionScope::from_context(original.focus.question, &current).unwrap();
    assert!(current.matches_original_context(&before));
    let row = ExportedRecord::new(&historical, &original.original.capture, original.original.record.clone()).unwrap();
    let reference =
        DiagnosticEvidenceRef::from_export(&historical, &digest(b"unknown-role-original-manifest"), &row, 0).unwrap();
    let input = DiagnosticOriginalRow { reference: &reference, record: &row.record };
    let update =
        observe_diagnostic_original(&historical, &reference.capture_ref(), &[input], None, "2020-01-01T12:00:01Z")
            .unwrap();
    let episode = &update.checkpoint.episodes[0];
    assert!(matches!(
        episode.plan_source_read_for_execution(&historical, &row, &current).unwrap(),
        DiagnosticFocusPlan::Ready(_)
    ));
    assert_eq!(before.database_writable, None);
}
#[test]
fn several_native_contexts_coexist_without_spill_bad_estimate_or_exclusive_cause_credit() {
    let original = fixture(true);
    let answer = answer(&original, vec![task(0), task(1)], crate::SourceState::Collected);
    assert!(answer.is_valid());
    for mechanism in [
        TemporaryResourceMechanism::UserAllocations,
        TemporaryResourceMechanism::InternalAllocations,
        TemporaryResourceMechanism::CleanupBoundary,
        TemporaryResourceMechanism::VersionProtection,
    ] {
        assert!(matches!(contribution(&answer, mechanism), TemporaryResourceContribution::Observed { .. }));
    }
    for mechanism in [
        TemporaryResourceMechanism::SpillOrEstimate,
        TemporaryResourceMechanism::VersionGeneration,
        TemporaryResourceMechanism::ConcurrentReservations,
        TemporaryResourceMechanism::DataCapacity,
        TemporaryResourceMechanism::LogCapacity,
        TemporaryResourceMechanism::AllocationLatch,
        TemporaryResourceMechanism::PhysicalIo,
    ] {
        assert_eq!(contribution(&answer, mechanism), &TemporaryResourceContribution::Unresolved { mechanism });
    }
    assert_eq!((answer.observed_tasks, answer.checked_task_context_count), (2, 2));
    assert_ne!(answer.basis_context.observed_until, answer.result_context.observed_from);
}
#[test]
fn a_logged_boundary_alone_is_not_a_version_protector_or_memory_pressure_verdict() {
    let original = fixture(false);
    let answer = answer(&original, vec![task(0)], crate::SourceState::Collected);
    assert!(matches!(
        contribution(&answer, TemporaryResourceMechanism::CleanupBoundary),
        TemporaryResourceContribution::Observed { observations: 1, .. }
    ));
    assert_eq!(
        contribution(&answer, TemporaryResourceMechanism::VersionProtection),
        &TemporaryResourceContribution::Unresolved { mechanism: TemporaryResourceMechanism::VersionProtection }
    );
    assert_eq!(answer.checked_task_context_count, 1);
}
#[test]
fn empty_partial_and_denied_reads_retain_boundary_context_without_negative_cause_or_recovery() {
    let original = fixture(true);
    for state in [crate::SourceState::Empty, crate::SourceState::Partial, crate::SourceState::Unavailable] {
        let answer = answer(&original, vec![], state);
        assert_eq!(answer.source_state, state);
        assert_eq!(answer.observed_tasks, 0);
        assert_eq!(
            contribution(&answer, TemporaryResourceMechanism::InternalAllocations),
            &TemporaryResourceContribution::Unresolved { mechanism: TemporaryResourceMechanism::InternalAllocations }
        );
        assert_eq!(original.episode.lifecycle, DiagnosticLifecycle::Active);
    }
}
#[test]
fn recorded_session_refusals_withdraw_only_their_exact_generation_and_keep_allocations() {
    let original = fixture(true);
    for (different_generation, recorded_refusal, checked) in [(true, false, 2), (false, true, 0), (true, true, 1)] {
        let first = task(0);
        let mut second = task(1);
        let request = second.request.as_mut().unwrap();
        if different_generation {
            request.key.session.generation = "different-native-login".into();
        }
        if recorded_refusal {
            request.session_context = Some(Box::new(ActivitySessionContext {
                lifecycle: SessionLifecycle::PoolResetPrelogin,
                login_age_ms: None,
                last_request_end_age_ms: None,
                client_library: None,
                client_process_id: None,
                native: None,
            }));
        }
        let answer = answer(&original, vec![first, second], crate::SourceState::Collected);
        assert_eq!(answer.observed_tasks, 2);
        assert_eq!(answer.checked_task_context_count, checked);
        assert!(matches!(
            contribution(&answer, TemporaryResourceMechanism::UserAllocations),
            TemporaryResourceContribution::Observed { observations: 2, .. }
        ));
    }
}
#[test]
fn full_roster_validation_rejects_wrong_receipts_missing_rows_and_native_clock_or_owner_drift() {
    let original = fixture(true);
    for defect in ["ordinal", "payload", "missing-row", "member", "epoch", "clock"] {
        let (mut source, capture, mut rows) = result(&original, vec![task(0), task(1)], crate::SourceState::Collected);
        match defect {
            "ordinal" => rows[1].0.row_ordinal = 0,
            "payload" => rows[0].0.payload_sha256 = digest(b"forged"),
            "missing-row" => {
                rows.pop();
            }
            "member" | "epoch" | "clock" => {
                let SourceObservation::SourceRead { execution, .. } = &mut source.observation else { panic!() };
                match defect {
                    "member" => {
                        let SourcePartition::InstanceLocal { member: Some(member), .. } = &mut execution.partition
                        else {
                            panic!()
                        };
                        member.native.id = "foreign".into();
                    }
                    "epoch" => {
                        let SourcePartition::InstanceLocal { epoch, .. } = &mut execution.partition else { panic!() };
                        *epoch = Some("replacement".into());
                    }
                    _ => execution.observed_from = "2020-01-01T11:59:59Z".into(),
                }
            }
            _ => unreachable!(),
        }
        let rows = rows.iter().map(|(reference, row)| (reference, row)).collect::<Vec<_>>();
        assert!(
            TemporaryResourceAnswer::from_originals(
                &original.focus,
                &original.source,
                &original.original,
                &source,
                &capture,
                &rows
            )
            .is_err(),
            "{defect}"
        );
    }
}
#[test]
fn bounded_answer_keeps_full_native_population_counts_and_explicit_receipt_prefixes() {
    let original = fixture(true);
    let answer = answer(&original, (0..200).map(task).collect(), crate::SourceState::Collected);
    assert_eq!((answer.observed_tasks, answer.checked_task_context_count), (200, 200));
    assert_eq!(answer.checked_task_contexts.len(), 8);
    assert!(
        matches!(contribution(&answer,TemporaryResourceMechanism::UserAllocations),TemporaryResourceContribution::Observed{observations:200,evidence,..} if evidence.len()==4)
    );
    assert!(serde_json::to_vec(&answer).unwrap().len() < 64 * 1024);
    assert_eq!(
        serde_json::from_value::<TemporaryResourceAnswer>(serde_json::to_value(&answer).unwrap()).unwrap(),
        answer
    );
}

fn observed_boundary(
    seed: &Fixture,
    age: Option<&str>,
    at: &str,
    previous: Option<&DiagnosticCheckpoint>,
) -> (CollectionDescription, ExportedRecord, DiagnosticObservationUpdate) {
    let mut source = seed.source.clone();
    let SourceObservation::LocalResource { snapshot, .. } = &mut source.observation else { panic!() };
    snapshot.observed_from = at.into();
    snapshot.observed_until = at.into();
    let mut record = seed.original.record.clone();
    let TelemetryRecord::Operational(OperationalObservation {
        fact: OperationalFact::MssqlCleanupHorizon(horizon),
        ..
    }) = &mut record
    else {
        panic!()
    };
    horizon.oldest_confirmed.as_mut().unwrap().age_seconds = age.map(str::to_owned);
    let mut capture = seed.original.capture.clone();
    capture.observation_id = format!("cleanup-age-{}", age.unwrap_or("unknown"));
    let row = ExportedRecord::new(&source, &capture, record).unwrap();
    let reference =
        DiagnosticEvidenceRef::from_export(&source, &digest(capture.observation_id.as_bytes()), &row, 0).unwrap();
    let update = observe_diagnostic_original(
        &source,
        &reference.capture_ref(),
        &[DiagnosticOriginalRow { reference: &reference, record: &row.record }],
        previous,
        at,
    )
    .unwrap();
    (source, row, update)
}

#[test]
fn cleanup_question_requires_the_existing_attention_age_on_the_exact_authenticated_boundary() {
    let seed = fixture(true);
    for (age, expected) in
        [(None, false), (Some("0"), false), (Some("1799"), false), (Some("1800"), true), (Some("3600"), true)]
    {
        let (source, row, update) = observed_boundary(&seed, age, "2020-01-01T12:00:01Z", None);
        assert_eq!(update.checkpoint.episodes.len(), 1, "all original boundary observations remain history");
        let episode = &update.checkpoint.episodes[0];
        let focus = episode.source_read_focus().unwrap().unwrap();
        assert_eq!(focus.matches_original(&source, &row), expected, "{age:?}");
        assert_eq!(focus.matches_basis(&source, &row, &seed.execution), expected, "{age:?}");
        if expected {
            let mut foreign = focus.clone();
            let SourceReadPrerequisite::ObservedCleanupBoundary { transaction, .. } = &mut foreign.prerequisite else {
                panic!()
            };
            transaction.generation = "2020-01-01T11:00:01.003".into();
            assert!(!foreign.matches_original(&source, &row));
        }
    }
}

#[test]
fn a_boundary_first_seen_young_can_pin_its_first_adverse_original_without_rebinding_later() {
    let seed = fixture(true);
    let (_, _, young) = observed_boundary(&seed, Some("30"), "2020-01-01T12:00:01Z", None);
    let (source, row, adverse) =
        observed_boundary(&seed, Some("1830"), "2020-01-01T12:30:01Z", Some(&young.checkpoint));
    let episode = &adverse.checkpoint.episodes[0];
    assert_eq!(episode.first, young.checkpoint.episodes[0].first);
    assert_ne!(episode.latest, episode.first);
    let focus = episode.source_read_focus().unwrap().unwrap();
    assert_eq!(*focus.basis, episode.latest);
    assert!(focus.matches_original(&source, &row));
    let enrolled = episode.enroll_source_read(focus.clone()).unwrap();
    let mut checkpoint = adverse.checkpoint;
    checkpoint.episodes[0] = enrolled;
    let (_, _, later) = observed_boundary(&seed, Some("3600"), "2020-01-01T13:00:01Z", Some(&checkpoint));
    let retained = &later.checkpoint.episodes[0];
    assert_ne!(retained.latest, *focus.basis);
    assert_eq!(retained.source_read_focus().unwrap().unwrap(), focus);
    assert_eq!(retained.first, young.checkpoint.episodes[0].first);
    let deferred = retained
        .transition_source_read(SourceReadStageState::Deferred { issue: DiagnosticAnalysisIssue::CostUnqualified })
        .unwrap();
    assert!(
        DiagnosticRevision::between(Some(&checkpoint.episodes[0]), deferred, "2020-01-01T13:00:02Z").unwrap().is_some(),
        "an original receipt and its legal source-stage transition may advance in the same revision"
    );
}

#[test]
fn local_resource_headers_match_only_their_exact_bounded_snapshot_request() {
    let original = fixture(true);
    let mut request = CollectionRequest {
        subject: CollectionSubject::Database,
        selection: CollectionSelection::Snapshot { dataset: SnapshotDataset::Cleanup },
        budget: CollectionBudget::default(),
    };
    assert!(original.source.matches_request(&request));
    request.selection = CollectionSelection::Snapshot { dataset: SnapshotDataset::TaskSpace };
    assert!(!original.source.matches_request(&request));
    request.selection = CollectionSelection::Snapshot { dataset: SnapshotDataset::Cleanup };
    request.budget.max_snapshot_rows = 0;
    assert!(!original.source.matches_request(&request));
    request.budget = CollectionBudget::default();
    request.selection = CollectionSelection::Snapshot { dataset: SnapshotDataset::Operational };
    let mut foreign = original.source;
    let SourceObservation::LocalResource { snapshot, .. } = &mut foreign.observation else { panic!() };
    snapshot.dataset = SnapshotDataset::Operational;
    assert!(!foreign.matches_request(&request), "a local role header cannot qualify an unrelated source family");
}
