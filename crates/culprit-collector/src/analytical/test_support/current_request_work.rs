//! Controlled current MySQL/MariaDB events replay through the ordinary authenticated operational envelope.
use super::*;
use culprit_core::{Engine, MeasurementScope, SourceState};

mod postgres_copy;
pub use postgres_copy::postgres_copy_evidence_cases;

const SOURCE: &str = "mysql-current-request-work";
const EPOCH: &str = "private-current-work-native-process";
const PARENT_AT: &str = "2020-01-01T12:00:00Z";
const FROM: &str = "2020-01-01T12:00:01Z";
const UNTIL: &str = "2020-01-01T12:00:01.800000000Z";

#[derive(Clone, Copy, PartialEq, Eq)]
enum Case {
    Matched,
    LogicalOriginal,
    Partial,
    SessionDatabase,
    UnlinkedStage,
    DisabledStage,
    DeniedStage,
    NoStage,
    MariaStage,
    Missing,
    Changed,
    Denied,
    LongSql,
}

pub fn current_request_work_evidence_cases() -> Vec<(CollectionDescription, Vec<TelemetryRecord>)> {
    use Case::*;
    [
        Matched,
        LogicalOriginal,
        Partial,
        SessionDatabase,
        UnlinkedStage,
        DisabledStage,
        DeniedStage,
        NoStage,
        MariaStage,
        Missing,
        Changed,
        Denied,
        LongSql,
    ]
    .into_iter()
    .map(evidence)
    .collect()
}

fn evidence(case: Case) -> (CollectionDescription, Vec<TelemetryRecord>) {
    let sql = if case == Case::LongSql {
        format!(
            "SELECT {} FROM orders WHERE customer_id=9264",
            (0..180).map(|index| format!("private_current_column_{index}")).collect::<Vec<_>>().join(",")
        )
    } else {
        "SELECT customer_id FROM orders WHERE customer_id=9264 AND token='private-current-work-sql'".into()
    };
    let (parent_source, original) = original(case, &sql);
    let focus = focus_from_original(parent_source.clone(), original);
    let binding = match case {
        Case::LogicalOriginal => FocusedRequestBinding::UnverifiedOriginalScope,
        Case::Missing => FocusedRequestBinding::ParticipantMissing,
        Case::Changed => FocusedRequestBinding::ParticipantChanged,
        _ => FocusedRequestBinding::Matched,
    };
    let membership = match case {
        Case::Missing | Case::Changed => FocusedRequestMembership::NotObserved,
        Case::Denied => FocusedRequestMembership::Unavailable,
        _ => FocusedRequestMembership::Observed,
    };
    let stage = match case {
        Case::UnlinkedStage => MysqlStageObservation::Unlinked,
        Case::DisabledStage => {
            MysqlStageObservation::Unavailable { reason: MysqlStageUnavailable::InstrumentationDisabled }
        }
        Case::DeniedStage => MysqlStageObservation::Unavailable { reason: MysqlStageUnavailable::PermissionDenied },
        Case::NoStage => MysqlStageObservation::NotObserved,
        _ => MysqlStageObservation::Observed {
            event: Box::new(MysqlStageEvent {
                thread_id: "107".into(),
                event_id: "4".into(),
                nesting_event_id: "3".into(),
                nesting_event_type: "STATEMENT".into(),
                event_name: "stage/sql/copy to tmp table".into(),
                timer_start_ps: None,
                timer_wait_ps: Some("0".into()),
                work_completed: (case != Case::MariaStage).then(|| "9007199254740993".into()),
                work_estimated: (case != Case::MariaStage).then(|| "42".into()),
            }),
        },
    };
    let rows = if membership == FocusedRequestMembership::Observed {
        let point = CurrentRequestWorkObservation {
            observed_at: FROM.into(),
            actor: focus.original.key.clone(),
            database: "db".into(),
            scope: focus.original.scope,
            statement_identity: focus.statement_identity.clone(),
            statement: DefinitionPayload::sql(&sql, Engine::Mysql),
            native: CurrentRequestWorkNative::Mysql {
                work: Box::new(MysqlCurrentRequestWork {
                    thread_id: "107".into(),
                    statement_event_id: "3".into(),
                    timer_start_ps: "0".into(),
                    timer_wait_ps: Some("18446744073709551615".into()),
                    rows_examined: Some("9007199254740993".into()),
                    rows_sent: Some("0".into()),
                    created_tmp_tables: Some("1".into()),
                    created_tmp_disk_tables: Some("0".into()),
                    sort_merge_passes: None,
                    stage,
                }),
            },
        };
        vec![TelemetryRecord::Operational(OperationalObservation {
            source: SOURCE.into(),
            key: point.key().unwrap(),
            fact: OperationalFact::CurrentRequestWork(Box::new(point)),
        })]
    } else {
        vec![]
    };
    let complete = !matches!(
        case,
        Case::Partial
            | Case::SessionDatabase
            | Case::UnlinkedStage
            | Case::DisabledStage
            | Case::DeniedStage
            | Case::Missing
            | Case::Changed
            | Case::Denied
    );
    let mut coverage = if case == Case::Denied {
        StateSourceCoverage::unavailable(SOURCE.into(), culprit_core::ErrorCode::PermissionDenied, None)
    } else {
        StateSourceCoverage::observed(SOURCE.into(), rows.len() as u32, complete)
    };
    if !complete {
        coverage.stopped = Some(CollectionStop::SourceUnavailable);
    }
    let source = CollectionDescription {
        target: parent_source.target,
        partition: partition(case),
        source: SOURCE.into(),
        continuity: SourceContinuity::Unverified,
        observation: SourceObservation::CurrentRequestWork {
            profile: CurrentRequestWorkProfile::MysqlStatementEvent,
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
    for record in &rows {
        let TelemetryRecord::Operational(OperationalObservation {
            fact: OperationalFact::CurrentRequestWork(point),
            ..
        }) = record
        else {
            unreachable!()
        };
        assert!(point.fits_source_scope(&source));
    }
    (source, rows)
}

fn focus_from_original(parent_source: CollectionDescription, original: ActivityObservation) -> ActivityRequestFocus {
    let recovery = Recovery::capture(
        "fixture:current-work-parent",
        &digest(b"current-work-native-schema"),
        parent_source.clone(),
        vec![TelemetryRecord::from(original.clone())],
        830_001,
    );
    let exported =
        ExportedRecord::new(&parent_source, &recovery.manifest.capture, TelemetryRecord::from(original.clone()))
            .unwrap();
    let reference =
        DiagnosticEvidenceRef::from_export(&parent_source, &recovery.manifest.capture_manifest_sha256, &exported, 0)
            .unwrap();
    let focus = ActivityRequestFocus {
        parent: reference,
        target: parent_source.target.clone(),
        partition: parent_source.partition.clone(),
        database_writable: Some(true),
        source: parent_source.source.clone(),
        epoch: EPOCH.into(),
        original_observed_at: PARENT_AT.into(),
        statement_identity: original.statement_identity.clone().unwrap(),
        original,
    };
    assert!(focus.matches_parent(&parent_source, &focus.original, &focus.parent));
    focus
}

fn original(case: Case, sql: &str) -> (CollectionDescription, ActivityObservation) {
    let mut row = activity_observation(DefinitionPayload::sql(sql, Engine::Mysql));
    row.key = ActivityKey {
        session: NativeActivityIdentity { id: "7".into(), generation: "107".into() },
        request: Some(NativeActivityIdentity { id: "3".into(), generation: "0".into() }),
        transaction: None,
    };
    row.statement_identity =
        culprit_core::engines::mysql_activity_statement_identity(&row.key.session, row.key.request.as_ref().unwrap());
    row.state = "executing".into();
    row.command = Some("Query".into());
    row.wait = None;
    row.waiting_on_lock = None;
    row.blocking_status = ActivityBlockingStatus::Unknown;
    row.elapsed_ms = None;
    row.session_counters = Default::default();
    row.request_counters = Some(Default::default());
    row.scope = if case == Case::SessionDatabase {
        ActivityScopeBasis::SessionDatabase
    } else {
        ActivityScopeBasis::RequestDatabase
    };
    row.client = Some(ActivityClient {
        application: None,
        user: Some("private-current-work-login".into()),
        host: Some("private-current-work-client".into()),
    });
    let source = CollectionDescription {
        target: culprit_core::DatabaseIdentity::server(Engine::Mysql, "db", "fixture.invalid", 3306, None).into(),
        partition: if case == Case::LogicalOriginal { SourcePartition::Target } else { partition(case) },
        source: "mysql-current-activity".into(),
        continuity: SourceContinuity::Unverified,
        observation: SourceObservation::Activity(ActivitySnapshotDescription {
            epoch: Some(EPOCH.into()),
            database_writable: Some(true),
            observed_from: PARENT_AT.into(),
            observed_until: PARENT_AT.into(),
            at: PARENT_AT.into(),
            scope: MeasurementScope::Database,
            population: ActivityPopulation::DefaultDatabaseConnections,
            native_rows: 1,
            observations: 1,
            coverage: StateSourceCoverage::collected("mysql-current-activity".into(), 1),
        }),
    };
    (source, row)
}

fn partition(case: Case) -> SourcePartition {
    let uuid = "f54a461c-0001-4000-8000-000000000001";
    let member = if case == Case::MariaStage {
        SharedResourceIdentity {
            engine: Engine::Mysql,
            kind: SharedResourceKind::Instance,
            namespace: NativeIdentity::new("mariadb-server-uid"),
            native: NativeIdentity::new("q83hHHfky00d9QAi5bwmy+JVlfE="),
        }
    } else {
        SharedResourceIdentity {
            engine: Engine::Mysql,
            kind: SharedResourceKind::Instance,
            namespace: NativeIdentity::new("mysql-server").qualified("server_uuid", uuid),
            native: NativeIdentity::new(uuid),
        }
    };
    SourcePartition::InstanceLocal { member: Some(member), epoch: Some(EPOCH.into()) }
}

pub fn assert_current_request_work_capture(
    capture: &crate::analytical::evidence::EvidenceCapture,
    source: &CollectionDescription,
    rows: &[TelemetryRecord],
) {
    assert_operational_capture(capture, source, rows);
    let SourceObservation::CurrentRequestWork { snapshot, result, .. } = &source.observation else {
        panic!("current request work header")
    };
    assert_eq!(snapshot.dataset, SnapshotDataset::CurrentRequestWork);
    assert!(result.focus.as_ref().unwrap().is_valid());
    for row in capture.records() {
        let TelemetryRecord::Operational(OperationalObservation {
            fact: OperationalFact::CurrentRequestWork(point),
            ..
        }) = &row.row().record
        else {
            panic!("current request work point")
        };
        assert!(point.fits_source_scope(source));
        assert_eq!(&point.actor, result.expected_actor().unwrap());
        match &point.native {
            CurrentRequestWorkNative::Mysql { work } => {
                assert_eq!(work.rows_examined.as_deref(), Some("9007199254740993"));
                assert_eq!(work.created_tmp_disk_tables.as_deref(), Some("0"));
                assert!(work.sort_merge_passes.is_none());
                assert!(
                    work.stage != MysqlStageObservation::Unlinked || snapshot.sources[0].state == SourceState::Partial
                );
            }
            CurrentRequestWorkNative::PostgresCopy { progress } => {
                assert_eq!(progress.bytes_processed.as_deref(), Some("9007199254740993"));
                assert_eq!(progress.bytes_total.as_deref(), Some("0"));
                assert_eq!(progress.tuples_processed.as_deref(), Some("9007199254740993"));
                assert_eq!(progress.relation_oid, "0");
            }
        }
    }
}
