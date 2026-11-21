use super::*;

fn answer() -> TemporaryResourceAnswer {
    let original = super::super::tests::first().episode.first;
    let mut basis = original.clone();
    basis.stream = TelemetryStream::OperationalSamples;
    basis.batch_id = batch_id(&basis.capture.observation_id, basis.stream).unwrap();
    basis.native_key = NativeRecordIdentity::Operational { source: "native-cleanup".into(), key: "boundary".into() };
    let mut selected = basis.clone();
    selected.capture.observation_id = "task-result".into();
    selected.batch_id = batch_id(&selected.capture.observation_id, selected.stream).unwrap();
    selected.source_scope_id = digest(b"task-result-scope");
    selected.native_key = NativeRecordIdentity::Operational { source: "task space".into(), key: "task".into() };
    let partition = SourcePartition::InstanceLocal {
        member: Some(SharedResourceIdentity {
            engine: culprit_core::Engine::Mssql,
            kind: SharedResourceKind::Instance,
            namespace: NativeIdentity::new("private-namespace"),
            native: NativeIdentity::new("private-member"),
        }),
        epoch: Some("private-db-process-epoch".into()),
    };
    let basis_context = NativeReadContext {
        target: culprit_core::DatabaseIdentity::server(
            culprit_core::Engine::Mssql,
            "private-database",
            "private-host",
            1433,
            None,
        )
        .into(),
        partition,
        observed_from: "2020-01-01T12:00:00Z".into(),
        observed_until: "2020-01-01T12:00:01Z".into(),
        database_writable: None,
    };
    let result_context = NativeReadContext {
        observed_from: "2020-01-01T12:00:10Z".into(),
        observed_until: "2020-01-01T12:00:11Z".into(),
        database_writable: Some(false),
        ..basis_context.clone()
    };
    let contributions = TemporaryResourceMechanism::ALL
        .iter()
        .map(|mechanism| match mechanism {
            TemporaryResourceMechanism::CleanupBoundary | TemporaryResourceMechanism::VersionProtection => {
                TemporaryResourceContribution::Observed {
                    mechanism: *mechanism,
                    observations: 1,
                    evidence: vec![basis.clone()],
                }
            }
            TemporaryResourceMechanism::UserAllocations => TemporaryResourceContribution::Observed {
                mechanism: *mechanism,
                observations: 1,
                evidence: vec![selected.clone()],
            },
            _ => TemporaryResourceContribution::Unresolved { mechanism: *mechanism },
        })
        .collect();
    TemporaryResourceAnswer {
        basis,
        basis_context,
        result: selected.capture_ref(),
        result_context,
        source_state: SourceState::Partial,
        contributions,
        observed_tasks: 1,
        checked_task_context_count: 0,
        checked_task_contexts: vec![],
    }
}
#[test]
fn context_words_retain_competing_mechanisms_independent_clocks_and_unproven_allocation_ownership() {
    let answer = answer();
    assert!(answer.is_valid());
    let words = explain(&answer);
    assert!(words.contains(&t!("view.tempdb_question.user", state = "observed")));
    assert!(words.contains(&t!("view.tempdb_question.internal", state = "unresolved")));
    assert!(words.contains(&t!("view.tempdb_question.spill")));
    assert!(words.contains(&t!("view.tempdb_question.generation")));
    assert!(words.contains(&t!("view.tempdb_question.coverage", state = "partial")));
    assert!(words.contains(&t!("view.tempdb_question.role")));
    assert!(words.iter().any(|word| word.contains(&answer.basis_context.observed_until)
        && word.contains(&answer.result_context.observed_until)));
    assert_eq!(answer.basis_context.database_writable, None);
    assert_eq!(answer.result_context.database_writable, Some(false));
}
#[test]
fn an_unavailable_task_source_never_words_native_zero_or_recovery_from_missing_rows() {
    let mut answer = answer();
    answer.source_state = SourceState::Unavailable;
    answer.observed_tasks = 0;
    for contribution in &mut answer.contributions {
        if contribution.mechanism() == TemporaryResourceMechanism::UserAllocations {
            *contribution =
                TemporaryResourceContribution::Unresolved { mechanism: TemporaryResourceMechanism::UserAllocations };
        }
    }
    assert!(answer.is_valid());
    let words = explain(&answer);
    assert!(words.contains(&t!("view.tempdb_question.coverage", state = "unavailable")));
    assert!(words.contains(&t!("view.tempdb_question.user", state = "unresolved")));
    assert!(words.contains(&t!("view.tempdb_question.protection", state = "observed")));
    assert!(words.contains(&t!("view.tempdb_question.checked", n = 0, count = "0")));
}
