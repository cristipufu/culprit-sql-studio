//! Controlled native index-build points exercise archive semantics, never production work authority.
use super::*;
use culprit_core::{Engine, MeasurementScope};

pub fn maintenance_progress_evidence_cases() -> Vec<(CollectionDescription, Vec<TelemetryRecord>)> {
    [Engine::Postgres, Engine::Yugabyte]
        .into_iter()
        .flat_map(|engine| {
            [
                (MaintenanceProgressBinding::Matched, MaintenanceProgressMembership::Observed),
                (MaintenanceProgressBinding::UnverifiedOriginalScope, MaintenanceProgressMembership::Observed),
                (MaintenanceProgressBinding::ParticipantMissing, MaintenanceProgressMembership::NotObserved),
                (MaintenanceProgressBinding::Matched, MaintenanceProgressMembership::Unavailable),
            ]
            .into_iter()
            .map(move |(binding, membership)| evidence(engine, binding, membership))
        })
        .collect()
}

fn evidence(
    engine: Engine,
    binding: MaintenanceProgressBinding,
    membership: MaintenanceProgressMembership,
) -> (CollectionDescription, Vec<TelemetryRecord>) {
    let original = original_source(engine);
    let mut focus = original_focus(&original);
    if binding == MaintenanceProgressBinding::UnverifiedOriginalScope {
        // The original Activity reader did not establish a compute member. This read cannot supply it retroactively.
        let mut unqualified = original.clone();
        unqualified.partition = SourcePartition::Target;
        focus = original_focus(&unqualified);
    }
    let at = "2020-01-01T12:00:01Z";
    let mut records = Vec::new();
    if membership == MaintenanceProgressMembership::Observed {
        let point = MaintenanceProgressObservation {
            observed_at: at.into(),
            actor: focus.original.key.clone(),
            statement: Some(focus.statement_identity.clone()),
            database_oid: "16384".into(),
            relation_oid: "16420".into(),
            index_oid: "0".into(),
            command: IndexBuildCommand::CreateIndexConcurrently,
            phase: match engine {
                Engine::Postgres => "building index: scanning table",
                Engine::Yugabyte => "backfilling",
                _ => unreachable!("fixture engines are explicit"),
            }
            .into(),
            work: match engine {
                Engine::Postgres => IndexBuildProgressWork::PostgreSql {
                    progress: Box::new(PostgresIndexBuildProgress {
                        blocks_total: Some("9007199254740993".into()),
                        blocks_done: Some("0".into()),
                        current_locker_pid: Some("0".into()),
                        ..Default::default()
                    }),
                },
                Engine::Yugabyte => IndexBuildProgressWork::Ysql {
                    progress: Box::new(YsqlIndexBuildProgress {
                        tuples_done: Some("9007199254740993".into()),
                        tuples_total_estimate: Some("42".into()),
                    }),
                },
                _ => unreachable!("fixture engines are explicit"),
            },
        };
        records.push(TelemetryRecord::Operational(OperationalObservation {
            source: "pg_stat_progress_create_index".into(),
            key: point.key().unwrap(),
            fact: OperationalFact::MaintenanceProgress(Box::new(point)),
        }));
    }
    let coverage = if membership == MaintenanceProgressMembership::Unavailable {
        StateSourceCoverage::unavailable(
            "pg_stat_progress_create_index".into(),
            culprit_core::ErrorCode::PermissionDenied,
            None,
        )
    } else {
        StateSourceCoverage::observed("pg_stat_progress_create_index".into(), records.len() as u32, true)
    };
    let source = CollectionDescription {
        target: original.target,
        partition: original.partition,
        source: "pg_stat_progress_create_index".into(),
        continuity: SourceContinuity::Unverified,
        observation: SourceObservation::MaintenanceProgress {
            snapshot: StateSnapshotDescription {
                dataset: SnapshotDataset::MaintenanceProgress,
                observed_from: at.into(),
                observed_until: at.into(),
                records: records.len() as u32,
                sources: vec![coverage],
            },
            result: Box::new(MaintenanceProgressResult {
                focus: Some(Box::new(focus.clone())),
                database_writable: role(engine),
                binding,
                membership,
            }),
        },
    };
    assert!(source.matches_request(&CollectionRequest {
        subject: CollectionSubject::Database,
        selection: CollectionSelection::MaintenanceProgress { focus: Box::new(focus) },
        budget: CollectionBudget::default(),
    }));
    (source, records)
}

fn original_source(engine: Engine) -> CollectionDescription {
    CollectionDescription {
        target: culprit_core::DatabaseIdentity::server(engine, "db", "fixture.invalid", 5432, None).into(),
        partition: SourcePartition::InstanceLocal {
            member: Some(SharedResourceIdentity {
                engine,
                kind: if engine == Engine::Postgres { SharedResourceKind::Instance } else { SharedResourceKind::Node },
                namespace: NativeIdentity::new("private-maintenance-namespace"),
                native: NativeIdentity::new("private-maintenance-member"),
            }),
            epoch: Some("private-maintenance-process-epoch".into()),
        },
        source: "pg_stat_activity".into(),
        continuity: SourceContinuity::Unverified,
        observation: SourceObservation::Activity(ActivitySnapshotDescription {
            epoch: Some("private-maintenance-process-epoch".into()),
            database_writable: role(engine),
            observed_from: "2020-01-01T12:00:00Z".into(),
            observed_until: "2020-01-01T12:00:00Z".into(),
            at: "2020-01-01T12:00:00Z".into(),
            scope: MeasurementScope::Database,
            population: ActivityPopulation::ClientBackends,
            native_rows: 1,
            observations: 1,
            coverage: StateSourceCoverage::collected("pg_stat_activity".into(), 1),
        }),
    }
}

fn original_focus(source: &CollectionDescription) -> ActivityRequestFocus {
    let mut actor = activity_observation(DefinitionPayload::sql(
        "CREATE INDEX CONCURRENTLY private_maintenance_index ON private_maintenance_table (id)",
        source.target.engine(),
    ));
    actor.scope = ActivityScopeBasis::SessionDatabase;
    actor.command = None;
    actor.state = "active".into();
    actor.wait = None;
    actor.key.request.as_mut().unwrap().id = "statement".into();
    actor.statement_identity = Some(NativeIdentity::new("private-maintenance-native-statement"));
    let capture = fixture().0.capture;
    let row = ExportedRecord::new(source, &capture, TelemetryRecord::from(actor)).unwrap();
    let parent =
        DiagnosticEvidenceRef::from_export(source, &digest(b"original maintenance manifest"), &row, 0).unwrap();
    let TelemetryRecord::Activity(actor) = row.record else { unreachable!() };
    ActivityRequestFocus {
        parent,
        target: source.target.clone(),
        partition: source.partition.clone(),
        database_writable: role(source.target.engine()),
        source: source.source.clone(),
        epoch: "private-maintenance-process-epoch".into(),
        original_observed_at: "2020-01-01T12:00:00Z".into(),
        statement_identity: actor.statement_identity.clone().unwrap(),
        original: *actor,
    }
}

fn role(engine: Engine) -> Option<bool> {
    match engine {
        Engine::Postgres => Some(true),
        Engine::Yugabyte => None,
        _ => unreachable!("fixture engines are explicit"),
    }
}
