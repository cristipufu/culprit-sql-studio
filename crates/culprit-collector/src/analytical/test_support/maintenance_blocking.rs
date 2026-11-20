//! Controlled, independently sealed maintenance and blocking points share one exact original Activity focus.
use super::*;
use culprit_core::MaintenanceDirection;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MaintenanceBlockingCase {
    Matched,
    UnverifiedOriginal,
    LaterDifferentOwner,
    SamePidNewGeneration,
    ProgressUnavailable,
    BlockingUnavailable,
    BlockingLimited,
    MismatchedOriginal,
    ChangedCurrentEpoch,
}

pub struct MaintenanceBlockingFixture {
    pub maintenance_source: CollectionDescription,
    pub maintenance_rows: Vec<TelemetryRecord>,
    pub blocking_source: CollectionDescription,
    pub blocking_rows: Vec<TelemetryRecord>,
}

pub fn maintenance_blocking_evidence(case: MaintenanceBlockingCase) -> MaintenanceBlockingFixture {
    let base = match case {
        MaintenanceBlockingCase::UnverifiedOriginal | MaintenanceBlockingCase::ChangedCurrentEpoch => {
            BlockingCase::UnverifiedOriginal
        }
        MaintenanceBlockingCase::BlockingUnavailable => BlockingCase::Unavailable,
        MaintenanceBlockingCase::BlockingLimited => BlockingCase::LookupLimited,
        _ => BlockingCase::Matched,
    };
    let (original_closure, original_rows) = blocking_evidence(base);
    let SourceObservation::FocusedBlocking { snapshot: later, result: closure } = &original_closure.observation else {
        unreachable!()
    };
    let at = "2020-01-01T12:00:00.5Z";
    let point = MaintenanceProgressObservation {
        observed_at: at.into(),
        actor: closure.focus.original.key.clone(),
        statement: Some(closure.focus.statement_identity.clone()),
        database_oid: "16384".into(),
        relation_oid: "16420".into(),
        index_oid: Some("16421".into()),
        command: MaintenanceCommand::CreateIndexConcurrently,
        phase: "waiting for writers before build".into(),
        work: MaintenanceProgressWork::PostgreSqlIndexBuild {
            progress: Box::new(PostgresIndexBuildProgress {
                lockers_total: Some("9007199254740993".into()),
                lockers_done: Some("0".into()),
                current_locker_pid: Some("43".into()),
                ..Default::default()
            }),
        },
    };
    let maintenance_rows = if case == MaintenanceBlockingCase::ProgressUnavailable {
        vec![]
    } else {
        vec![TelemetryRecord::Operational(OperationalObservation {
            source: "pg_stat_progress_create_index".into(),
            key: point.key().unwrap(),
            fact: OperationalFact::MaintenanceProgress(Box::new(point)),
        })]
    };
    let coverage = if case == MaintenanceBlockingCase::ProgressUnavailable {
        StateSourceCoverage::unavailable(
            "pg_stat_progress_create_index".into(),
            culprit_core::ErrorCode::PermissionDenied,
            None,
        )
    } else {
        StateSourceCoverage::observed("pg_stat_progress_create_index".into(), 1, false)
    };
    let maintenance_source = CollectionDescription {
        target: original_closure.target.clone(),
        partition: original_closure.partition.clone(),
        source: "postgres-maintenance-progress".into(),
        continuity: SourceContinuity::Unverified,
        observation: SourceObservation::MaintenanceProgress {
            snapshot: StateSnapshotDescription {
                dataset: SnapshotDataset::MaintenanceProgress,
                observed_from: at.into(),
                observed_until: at.into(),
                records: maintenance_rows.len() as u32,
                sources: vec![coverage],
            },
            result: Box::new(FocusedRequestResult {
                focus: Some(Box::new(closure.focus.clone())),
                database_writable: later.database_writable,
                binding: closure.binding,
                membership: if maintenance_rows.is_empty() {
                    FocusedRequestMembership::Unavailable
                } else {
                    FocusedRequestMembership::Observed
                },
            }),
        },
    };
    let (mut blocking_source, mut blocking_rows) = if case == MaintenanceBlockingCase::MismatchedOriginal {
        blocking_evidence(BlockingCase::UnverifiedOriginal)
    } else {
        (original_closure, original_rows)
    };
    if matches!(case, MaintenanceBlockingCase::LaterDifferentOwner | MaintenanceBlockingCase::SamePidNewGeneration) {
        let [TelemetryRecord::Activity(waiter), TelemetryRecord::Activity(owner)] = blocking_rows.as_mut_slice() else {
            unreachable!()
        };
        if case == MaintenanceBlockingCase::LaterDifferentOwner {
            owner.key.session.id = "44".into();
        }
        owner.key.session.generation = "2020-01-01T11:00:44Z".into();
        waiter.wait.as_mut().unwrap().owners = vec![ActivityWaitOwner::Session {
            id: owner.key.session.id.clone(),
            generation: Some(owner.key.session.generation.clone()),
        }];
    }
    if case == MaintenanceBlockingCase::ChangedCurrentEpoch {
        let SourcePartition::InstanceLocal { epoch, .. } = &mut blocking_source.partition else { unreachable!() };
        *epoch = Some("different-current-native-process".into());
        let SourceObservation::FocusedBlocking { snapshot, .. } = &mut blocking_source.observation else {
            unreachable!()
        };
        snapshot.epoch = epoch.clone();
    }
    assert!(maintenance_source.partition_is_consistent());
    assert!(blocking_source.partition_is_consistent());
    for (source, rows) in [(&maintenance_source, &maintenance_rows), (&blocking_source, &blocking_rows)] {
        for row in rows {
            assert!(ExportedRecord::new(source, &fixture().0.capture, row.clone()).is_ok());
        }
    }
    MaintenanceBlockingFixture { maintenance_source, maintenance_rows, blocking_source, blocking_rows }
}

/// One original Activity frame carries each independent maintenance blocking direction.
/// Current statement context never establishes which statement acquired a session's locks.
pub fn maintenance_activity_evidence(direction: MaintenanceDirection) -> (CollectionDescription, Vec<TelemetryRecord>) {
    let (mut source, _) = activity_evidence_cases().remove(0);
    let current = |session: &str, sql: &str| {
        let mut row = activity_observation(DefinitionPayload::sql(sql, culprit_core::Engine::Mssql));
        row.key.session.id = session.into();
        row.key.session.generation = format!("native-login-{session}");
        row.key.request.as_mut().unwrap().generation = format!("native-request-{session}");
        row.statement_identity = Some(NativeIdentity::new(format!("native-statement-{session}")));
        row.waiting_on_lock = Some(false);
        row.wait = None;
        row
    };
    let mut maintenance = current("81", "ALTER INDEX ix ON dbo.orders REBUILD");
    let mut affected = current("82", "SELECT payload FROM dbo.orders WHERE tenant_id=9264");
    let owner = current("83", "UPDATE dbo.orders SET payload=9264 WHERE id=42");
    let lock_wait = |owner: &ActivityObservation| ActivityWait {
        class: None,
        native_type: Some("LCK_M_X".into()),
        elapsed_ms: Some("40000".into()),
        owners: vec![ActivityWaitOwner::Session {
            id: owner.key.session.id.clone(),
            generation: Some(owner.key.session.generation.clone()),
        }],
        identified_resources: vec![],
        resources: None,
    };
    let blocking = matches!(direction, MaintenanceDirection::BlockingSession | MaintenanceDirection::Both);
    let waiting = matches!(direction, MaintenanceDirection::Waiting | MaintenanceDirection::Both);
    if blocking {
        affected.waiting_on_lock = Some(true);
        affected.wait = Some(lock_wait(&maintenance));
    }
    if waiting {
        maintenance.waiting_on_lock = Some(true);
        maintenance.wait = Some(lock_wait(&owner));
    }
    let mut records = vec![TelemetryRecord::from(maintenance)];
    if blocking {
        records.push(affected.into());
    }
    if waiting {
        records.push(owner.into());
    }
    let SourceObservation::Activity(native) = &mut source.observation else { unreachable!() };
    native.native_rows = records.len() as u32;
    native.observations = native.native_rows;
    native.coverage = StateSourceCoverage::collected(source.source.clone(), native.native_rows);
    (source, records)
}
