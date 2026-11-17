//! Synthetic selected PostgreSQL COPY points use the same receipt/export framework as MySQL work.
use super::*;

#[derive(Clone, Copy)]
enum CopyCase {
    Matched,
    LogicalOriginal,
    Partial,
    Missing,
    Changed,
    Denied,
    LongSql,
}

pub fn postgres_copy_evidence_cases() -> Vec<(CollectionDescription, Vec<TelemetryRecord>)> {
    [
        CopyCase::Matched,
        CopyCase::LogicalOriginal,
        CopyCase::Partial,
        CopyCase::Missing,
        CopyCase::Changed,
        CopyCase::Denied,
        CopyCase::LongSql,
    ]
    .into_iter()
    .map(evidence)
    .collect()
}

fn evidence(case: CopyCase) -> (CollectionDescription, Vec<TelemetryRecord>) {
    let sql = if matches!(case, CopyCase::LongSql) {
        format!(
            "COPY (SELECT {} FROM orders) TO STDOUT",
            (0..180).map(|column| format!("private_copy_column_{column}")).collect::<Vec<_>>().join(",")
        )
    } else {
        "COPY (SELECT id FROM orders) TO STDOUT".into()
    };
    let (mut source, mut original) = super::original(Case::Matched, &sql);
    source.target =
        culprit_core::DatabaseIdentity::server(Engine::Postgres, "db", "fixture.invalid", 5432, None).into();
    let SourcePartition::InstanceLocal { member: Some(member), .. } = &mut source.partition else { panic!() };
    member.engine = Engine::Postgres;
    member.namespace = NativeIdentity::new("pg-native-test-namespace");
    member.native = NativeIdentity::new("pg-native-test-member");
    if matches!(case, CopyCase::LogicalOriginal) {
        source.partition = SourcePartition::Target;
    }
    source.source = "pg_stat_activity".into();
    let SourceObservation::Activity(frame) = &mut source.observation else { panic!() };
    frame.population = ActivityPopulation::ClientBackends;
    frame.coverage.source = source.source.clone();
    original.key = ActivityKey {
        session: NativeActivityIdentity { id: "7".into(), generation: "2020-01-01T10:00:00.000001Z".into() },
        request: Some(NativeActivityIdentity {
            id: "statement".into(),
            generation: "2020-01-01T11:00:00.000002Z".into(),
        }),
        transaction: None,
    };
    original.scope = ActivityScopeBasis::SessionDatabase;
    original.state = "active".into();
    original.command = None;
    original.statement_identity = Some(culprit_core::engines::postgres_activity_statement_identity(
        &original.key.session.id,
        &original.key.session.generation,
        &original.key.request.as_ref().unwrap().generation,
    ));
    original.statement = DefinitionPayload::sql(&sql, Engine::Postgres);
    let focus = focus_from_original(source, original);
    let binding = match case {
        CopyCase::LogicalOriginal => FocusedRequestBinding::UnverifiedOriginalScope,
        CopyCase::Missing => FocusedRequestBinding::ParticipantMissing,
        CopyCase::Changed => FocusedRequestBinding::ParticipantChanged,
        _ => FocusedRequestBinding::Matched,
    };
    let membership = match case {
        CopyCase::Missing | CopyCase::Changed => FocusedRequestMembership::NotObserved,
        CopyCase::Denied => FocusedRequestMembership::Unavailable,
        _ => FocusedRequestMembership::Observed,
    };
    let rows = if membership == FocusedRequestMembership::Observed {
        let point = CurrentRequestWorkObservation {
            observed_at: FROM.into(),
            actor: focus.original.key.clone(),
            database: "db".into(),
            scope: ActivityScopeBasis::SessionDatabase,
            statement_identity: focus.statement_identity.clone(),
            statement: DefinitionPayload::sql(&sql, Engine::Postgres),
            native: CurrentRequestWorkNative::PostgresCopy {
                progress: Box::new(PostgresCopyProgress {
                    database_oid: "16384".into(),
                    relation_oid: "0".into(),
                    command: PostgresCopyCommand::To,
                    io_type: PostgresCopyIo::Pipe,
                    bytes_processed: Some("9007199254740993".into()),
                    bytes_total: Some("0".into()),
                    tuples_processed: Some("9007199254740993".into()),
                    tuples_excluded: Some("0".into()),
                    tuples_skipped: None,
                }),
            },
        };
        vec![TelemetryRecord::Operational(OperationalObservation {
            source: "pg_stat_progress_copy".into(),
            key: point.key().unwrap(),
            fact: OperationalFact::CurrentRequestWork(Box::new(point)),
        })]
    } else {
        vec![]
    };
    let complete = matches!(case, CopyCase::Matched | CopyCase::LongSql);
    let coverage = if matches!(case, CopyCase::Denied) {
        StateSourceCoverage::unavailable(
            "pg_stat_progress_copy".into(),
            culprit_core::ErrorCode::PermissionDenied,
            None,
        )
    } else {
        StateSourceCoverage::observed("pg_stat_progress_copy".into(), rows.len() as u32, complete)
    };
    let mut partition = focus.partition.clone();
    if partition == SourcePartition::Target {
        let (actual, _) = super::original(Case::Matched, &sql);
        partition = actual.partition;
        let SourcePartition::InstanceLocal { member: Some(member), .. } = &mut partition else { panic!() };
        member.engine = Engine::Postgres;
        member.namespace = NativeIdentity::new("pg-native-test-namespace");
        member.native = NativeIdentity::new("pg-native-test-member");
    }
    let source = CollectionDescription {
        target: focus.target.clone(),
        partition,
        source: "pg_stat_progress_copy".into(),
        continuity: SourceContinuity::Unverified,
        observation: SourceObservation::CurrentRequestWork {
            profile: CurrentRequestWorkProfile::PostgresCopy,
            snapshot: StateSnapshotDescription {
                dataset: SnapshotDataset::CurrentRequestWork,
                observed_from: FROM.into(),
                observed_until: UNTIL.into(),
                records: rows.len() as u32,
                sources: vec![coverage],
            },
            result: Box::new(FocusedRequestResult {
                focus: Some(Box::new(focus.clone())),
                database_writable: Some(true),
                binding,
                membership,
            }),
        },
    };
    assert!(source.matches_request(&CollectionRequest {
        subject: CollectionSubject::Database,
        selection: CollectionSelection::CurrentRequestWork { focus: Box::new(focus) },
        budget: CollectionBudget::default()
    }));
    for row in &rows {
        ExportedRecord::new(
            &source,
            &CaptureIdentity { observation_id: "controlled-copy".into(), revision: None },
            row.clone(),
        )
        .unwrap();
    }
    (source, rows)
}
