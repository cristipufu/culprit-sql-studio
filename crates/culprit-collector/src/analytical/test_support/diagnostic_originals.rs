//! Controlled original facts reuse existing native-shaped corpora. They are not runtime/native qualification.
use super::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DiagnosticOriginalCase {
    Memory,
    MemoryEmpty,
    MemoryChanged,
    MemoryAmbiguousPool,
    MysqlError,
    MysqlWarning,
    PostgresFailure,
    Cleanup,
    CleanupEmpty,
    CleanupPartial,
    CleanupDenied,
    Temporary,
    TemporaryZero,
    TemporaryUnknown,
    TemporaryNegativePeer,
    HistoryException,
    HistoryAborted,
    HistoryRegular,
    HistoryUnknown,
    HistoryZero,
    HistoryMultipleFragments,
    HistoryMissingNamespace,
    HistoryUnresolved,
}

pub fn diagnostic_original_evidence(case: DiagnosticOriginalCase) -> (CollectionDescription, Vec<TelemetryRecord>) {
    use DiagnosticOriginalCase::*;
    let (qualified, _) = cleanup_evidence(CleanupCase::LocalSnapshotProtector);
    if matches!(case, MysqlError | MysqlWarning | PostgresFailure) {
        let engine = if case == PostgresFailure { culprit_core::Engine::Postgres } else { culprit_core::Engine::Mysql };
        let (mut source, mut rows) =
            native_failure_evidence_cases().into_iter().find(|(source, _)| source.target.engine() == engine).unwrap();
        let CollectionTarget::Shared(owner) = &source.target else { unreachable!() };
        source.partition = SourcePartition::InstanceLocal {
            member: Some(owner.clone()),
            epoch: Some(digest(b"controlled-native-error-process")),
        };
        if case == MysqlWarning {
            for row in &mut rows {
                let TelemetryRecord::Operational(row) = row else { unreachable!() };
                let OperationalFact::MysqlErrorLog(row) = &mut row.fact else { unreachable!() };
                row.priority = "Warning".into();
            }
        }
        assert!(source.partition_is_consistent());
        assert!(rows.iter().all(TelemetryRecord::is_valid));
        return (source, rows);
    }
    if matches!(
        case,
        HistoryException
            | HistoryAborted
            | HistoryRegular
            | HistoryUnknown
            | HistoryZero
            | HistoryMultipleFragments
            | HistoryMissingNamespace
            | HistoryUnresolved
    ) {
        return history(case, &qualified);
    }
    let (mut source, rows) = match case {
        Memory | MemoryEmpty | MemoryChanged | MemoryAmbiguousPool => {
            let (mut source, mut rows) = memory_failure_evidence();
            if case != MemoryAmbiguousPool {
                let SourcePartition::InstanceLocal { member: Some(member), .. } = &qualified.partition else {
                    unreachable!()
                };
                source.target = CollectionTarget::Shared(member.clone());
                source.partition = qualified.partition.clone();
            }
            if case == MemoryEmpty {
                rows.clear();
            }
            if case == MemoryChanged {
                let TelemetryRecord::Operational(row) = &mut rows[0] else { unreachable!() };
                let OperationalFact::MssqlOutOfMemory(row) = &mut row.fact else { unreachable!() };
                row.oom_cause = 12;
                row.event_time_without_offset = "2020-01-01T11:00:00.003".into();
            }
            (source, rows)
        }
        Cleanup | CleanupEmpty | CleanupPartial | CleanupDenied => cleanup_evidence(if case == CleanupEmpty {
            CleanupCase::LocalRecoveredHorizon
        } else if case == CleanupDenied {
            CleanupCase::LocalUnavailableHorizon
        } else {
            CleanupCase::LocalSnapshotProtector
        }),
        Temporary | TemporaryZero | TemporaryUnknown | TemporaryNegativePeer => {
            let (mut source, mut rows) = task_space_evidence_cases().remove(0);
            source.partition = qualified.partition.clone();
            for record in &mut rows {
                let TelemetryRecord::Operational(row) = record else { unreachable!() };
                let OperationalFact::TaskSpace(task) = &mut row.fact else { unreachable!() };
                task.user_allocated_pages = Some(if case == TemporaryZero { "0" } else { "64" }.into());
                task.user_deallocated_pages = Some("0".into());
                task.internal_allocated_pages = if case == TemporaryUnknown { None } else { Some("0".into()) };
                task.internal_deallocated_pages = Some("0".into());
            }
            if case == TemporaryNegativePeer {
                let TelemetryRecord::Operational(row) = &mut rows[1] else { unreachable!() };
                let OperationalFact::TaskSpace(task) = &mut row.fact else { unreachable!() };
                task.request.as_mut().unwrap().session_context = Some(Box::new(ActivitySessionContext {
                    lifecycle: SessionLifecycle::PoolResetPrelogin,
                    login_age_ms: None,
                    last_request_end_age_ms: None,
                    client_library: None,
                    client_process_id: None,
                    native: None,
                }));
            }
            (source, rows)
        }
        _ => unreachable!(),
    };
    let SourceObservation::Snapshot(snapshot) = &mut source.observation else { unreachable!() };
    snapshot.records = rows.len() as u32;
    if matches!(case, Memory | MemoryEmpty | MemoryChanged | MemoryAmbiguousPool) {
        snapshot.sources[0] =
            StateSourceCoverage::observed(culprit_core::source_id::MEMORY_FAILURES.into(), rows.len() as u32, true);
    }
    if case == CleanupPartial {
        let coverage = snapshot
            .sources
            .iter_mut()
            .find(|coverage| coverage.source == culprit_core::source_id::OPEN_TRANSACTIONS)
            .unwrap();
        *coverage = StateSourceCoverage::observed(coverage.source.clone(), coverage.records, false);
    }
    assert!(source.partition_is_consistent());
    assert!(rows.iter().all(TelemetryRecord::is_valid));
    (source, rows)
}

fn history(
    case: DiagnosticOriginalCase,
    qualified: &CollectionDescription,
) -> (CollectionDescription, Vec<TelemetryRecord>) {
    use DiagnosticOriginalCase::*;
    let mut fixture = super::page_server_reads::page_server_read_evidence_cases()
        .into_iter()
        .find(|case| case.name == "single_fragment_with_definitions")
        .unwrap();
    let mut source = fixture.source;
    source.source = culprit_core::source_id::QUERY_STORE.into();
    let SourceObservation::QueryHistory(history) = &mut source.observation else { unreachable!() };
    if let Some(catalog) = &mut history.population_catalog {
        catalog.source = source.source.clone();
    }
    history.acquisition_owner = Some(Box::new(NativeReadContext {
        target: source.target.clone(),
        partition: qualified.partition.clone(),
        observed_from: "2020-01-01T12:00:00Z".into(),
        observed_until: "2020-01-01T12:00:01Z".into(),
        database_writable: Some(true),
    }));
    let runtime_position =
        fixture.records.iter().position(|record| matches!(record, TelemetryRecord::QueryRuntime(_))).unwrap();
    let TelemetryRecord::QueryRuntime(mut runtime) = fixture.records.remove(runtime_position) else { unreachable!() };
    let mut definitions = fixture.records;
    for definition in &mut definitions {
        match definition {
            TelemetryRecord::QueryDefinition(query) => query.id = query.identity.definition_id(&source).unwrap(),
            TelemetryRecord::PlanDefinition(plan) => {
                plan.id = plan.identity.definition_id(&source).unwrap();
                plan.query_definition_id = Some(plan.identity.query.definition_id(&source).unwrap());
            }
            _ => unreachable!(),
        }
    }
    for reference in [&mut runtime.key.references.query, &mut runtime.key.references.plan].into_iter().flatten() {
        reference.definition_id = definitions.iter().find_map(|definition| match definition {
            TelemetryRecord::QueryDefinition(query) if query.identity.native() == reference.native => {
                Some(query.id.clone())
            }
            TelemetryRecord::PlanDefinition(plan) if plan.identity.native() == reference.native => {
                Some(plan.id.clone())
            }
            _ => None,
        });
        assert!(reference.definition_id.is_some(), "only original matching native definitions close these identities");
    }
    let execution_type = match case {
        HistoryAborted => "3",
        HistoryRegular => "0",
        HistoryUnknown => "2",
        _ => "4",
    };
    if case == HistoryUnknown {
        let SourceObservation::QueryHistory(history) = &mut source.observation else { unreachable!() };
        // This controlled source explicitly retains an unrecognized native value. It must still satisfy
        // its own declared dimension contract; the interpreter does not classify it as a failure.
        history.dimensions.get_mut("execution_type").unwrap().push(execution_type.into());
    }
    runtime.key.native.qualifiers.insert("execution_type".into(), execution_type.into());
    runtime.key.cohort.as_mut().unwrap().qualifiers.insert("execution_type".into(), execution_type.into());
    runtime.executions = if case == HistoryZero { "0" } else { "7" }.into();
    let mut rows = vec![TelemetryRecord::QueryRuntime(runtime.clone())];
    if case == HistoryMultipleFragments {
        runtime.key.native.id = "9999".into();
        runtime.executions = "11".into();
        rows.push(TelemetryRecord::QueryRuntime(runtime));
    }
    if case == HistoryUnresolved {
        for record in &mut rows {
            let TelemetryRecord::QueryRuntime(runtime) = record else { unreachable!() };
            for reference in [&mut runtime.key.references.query, &mut runtime.key.references.plan].into_iter().flatten()
            {
                reference.definition_id = None;
            }
        }
    } else {
        rows.extend(definitions);
    }
    if case == HistoryMissingNamespace {
        let CollectionTarget::Database(database) = &mut source.target else { unreachable!() };
        database.incarnation = None;
        let SourceObservation::QueryHistory(history) = &mut source.observation else { unreachable!() };
        history.acquisition_owner.as_mut().unwrap().target = source.target.clone();
    }
    assert!(source.partition_is_consistent());
    (source, rows)
}
