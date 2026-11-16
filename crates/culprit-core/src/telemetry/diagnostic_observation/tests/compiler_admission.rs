use super::*;

fn baseline() -> (CollectionDescription, DiagnosticCaptureRef, Vec<(DiagnosticEvidenceRef, ActivityObservation)>) {
    let mut row = actor();
    row.wait.as_mut().unwrap().class = Some(ActivityWaitClass::CompilationAdmission);
    row.wait.as_mut().unwrap().native_type = Some("native-compiler-admission".into());
    row.wait.as_mut().unwrap().identified_resources.clear();
    request_memory::memory_frame(1, vec![row], true)
}

fn question() -> (DiagnosticCheckpoint, ActivityRequestFocus) {
    let baseline = baseline();
    let observed = observe(&baseline, None);
    let focus = observed
        .focuses
        .into_iter()
        .find_map(|(_, focus)| match focus {
            DiagnosticFocus::CompilationAdmission { focus } => Some(*focus),
            _ => None,
        })
        .unwrap();
    (observed.checkpoint, focus)
}

fn captured(
    focus: ActivityRequestFocus,
    observed: bool,
) -> (CollectionDescription, DiagnosticCaptureRef, Vec<(DiagnosticEvidenceRef, OperationalObservation)>) {
    let at = "2020-01-01T00:02:00Z";
    let mut rows = Vec::new();
    if observed {
        let waiter = CompilerWaiterObservation { observed_at: at.into(), request: focus.original.clone() };
        rows.push(OperationalObservation {
            source: crate::source_id::COMPILATION_GATEWAYS.into(),
            key: waiter.key().unwrap(),
            fact: OperationalFact::CompilerWaiter(Box::new(waiter)),
        });
        rows.push(super::super::super::compiler_admission::tests::gateway("2", 3));
    }
    let source = CollectionDescription {
        target: focus.target.clone(),
        partition: focus.partition.clone(),
        source: crate::source_id::COMPILATION_GATEWAYS.into(),
        continuity: SourceContinuity::Unverified,
        observation: SourceObservation::FocusedCompilationAdmission {
            snapshot: StateSnapshotDescription {
                dataset: SnapshotDataset::CompilerAdmission,
                observed_from: at.into(),
                observed_until: at.into(),
                records: rows.len() as u32,
                sources: vec![StateSourceCoverage::observed(
                    crate::source_id::COMPILATION_GATEWAYS.into(),
                    rows.len() as u32,
                    true,
                )],
            },
            result: Box::new(ActivityCompilationAdmissionFocusResult {
                focus,
                observed_from: at.into(),
                observed_until: at.into(),
                source: FocusSourceState::Matched,
                membership: if observed {
                    CompilationAdmissionMembership::Observed
                } else {
                    CompilationAdmissionMembership::NotObserved
                },
                gateways: if observed {
                    vec![match &rows[1].fact {
                        OperationalFact::CounterSet(counters) => counters.identity.clone(),
                        _ => panic!(),
                    }]
                } else {
                    vec![]
                },
            }),
        },
    };
    let input = DiagnosticCaptureRef {
        capture: CaptureIdentity {
            observation_id: "compiler-result".into(),
            revision: Some(CaptureRevision {
                outbox_id: "owned-outbox".into(),
                owner_epoch: "1".into(),
                sequence: "2".into(),
            }),
        },
        manifest_sha256: digest(b"compiler-result-manifest"),
        source_scope_id: source.captured_range().unwrap().scope_id,
    };
    let rows = rows
        .into_iter()
        .enumerate()
        .map(|(ordinal, row)| {
            let exported =
                ExportedRecord::new(&source, &input.capture, TelemetryRecord::Operational(row.clone())).unwrap();
            (
                DiagnosticEvidenceRef::from_export(&source, &input.manifest_sha256, &exported, ordinal as u32).unwrap(),
                row,
            )
        })
        .collect();
    (source, input, rows)
}

#[test]
fn exact_compiler_waiter_produces_a_distinct_question_and_retains_every_gateway_receipt_without_resolving() {
    let (checkpoint, focus) = question();
    assert!(focus.is_compilation_admission_focus());
    assert!(!focus.is_request_memory_focus());
    let (source, input, rows) = captured(focus.clone(), true);
    let rows = rows
        .iter()
        .map(|(reference, observation)| DiagnosticCompilerRow { reference, observation })
        .collect::<Vec<_>>();
    let update =
        analyze_diagnostic_compilation_admission(&source, &input, &rows, &checkpoint, "2020-01-01T00:03:00Z", None)
            .unwrap();
    let episode =
        update.checkpoint.episodes.iter().find(|episode| episode.matches_compilation_admission_focus(&focus)).unwrap();
    assert_eq!(episode.lifecycle, DiagnosticLifecycle::Active);
    assert_eq!(episode.analysis, DiagnosticAnalysisStatus::Analyzed);
    let report = episode.report.as_ref().unwrap();
    assert_eq!(report.references().count(), 2);
    assert!(report.matches_compiler_admission_input(&source, rows.iter().map(|row| (row.reference, row.observation))));
    let revision = update.revisions.iter().find(|revision| revision.episode.id == episode.id).unwrap();
    assert!(revision.inputs.contains(&focus.parent));
    assert!(report.references().all(|reference| revision.inputs.contains(reference)));
}

#[test]
fn missing_waiter_missing_gateway_duplicate_or_replaced_statement_never_settles_the_original_as_recovered() {
    let (checkpoint, focus) = question();
    let (source, input, _) = captured(focus.clone(), false);
    let update =
        analyze_diagnostic_compilation_admission(&source, &input, &[], &checkpoint, "2020-01-01T00:03:00Z", None)
            .unwrap();
    let episode =
        update.checkpoint.episodes.iter().find(|episode| episode.matches_compilation_admission_focus(&focus)).unwrap();
    assert_eq!(episode.lifecycle, DiagnosticLifecycle::Active);
    assert_eq!(episode.analysis, DiagnosticAnalysisStatus::Unresolved);
    for defect in ["missing-gateway", "duplicate", "generation", "statement", "database", "clock"] {
        let (source, input, mut native) = captured(focus.clone(), true);
        match defect {
            "missing-gateway" => {
                native.pop();
            }
            "duplicate" => native.push(native[0].clone()),
            _ => {
                let OperationalFact::CompilerWaiter(waiter) = &mut native[0].1.fact else { panic!() };
                match defect {
                    "generation" => waiter.request.key.request.as_mut().unwrap().generation = "replacement".into(),
                    "statement" => waiter.request.statement_identity = Some(NativeIdentity::new("replacement")),
                    "database" => waiter.request.database = Some("foreign-db".into()),
                    "clock" => waiter.observed_at = "2020-01-01T00:00:00Z".into(),
                    _ => unreachable!(),
                }
            }
        }
        let rows = native
            .iter()
            .map(|(reference, observation)| DiagnosticCompilerRow { reference, observation })
            .collect::<Vec<_>>();
        assert!(
            analyze_diagnostic_compilation_admission(&source, &input, &rows, &checkpoint, "2020-01-01T00:03:00Z", None)
                .is_err(),
            "{defect}"
        );
    }
}
