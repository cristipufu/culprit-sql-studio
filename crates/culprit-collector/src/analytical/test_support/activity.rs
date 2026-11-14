use super::*;
use culprit_core::{Engine, MeasurementScope, ObservedWaitResource, WaitResourceKind};

pub fn activity_observation(statement: DefinitionPayload) -> ActivityObservation {
    ActivityObservation {
        blocking_status: ActivityBlockingStatus::Known,
        waiting_on_lock: None,
        key: ActivityKey {
            session: NativeActivityIdentity { id: "61".into(), generation: "native-login-generation".into() },
            request: Some(NativeActivityIdentity { id: "0".into(), generation: "native-request-generation".into() }),
            transaction: None,
        },
        idle: false,
        scope: ActivityScopeBasis::RequestDatabase,
        state: "suspended".into(),
        database: Some("db".into()),
        command: Some("SELECT".into()),
        last_request_age_ms: None,
        open_transactions: None,
        elapsed_ms: Some("9007199254740993".into()),
        transaction_age_ms: None,
        session_counters: ActivityCounters {
            cpu_ms: Some("11".into()),
            logical_reads: Some("18446744073709551615".into()),
            ..Default::default()
        },
        request_counters: Some(ActivityCounters {
            cpu_ms: Some("0".into()),
            physical_reads: Some("0".into()),
            ..Default::default()
        }),
        wait: Some(ActivityWait {
            identified_resources: vec![],
            resources: None,
            native_type: Some("LCK_M_X".into()),
            elapsed_ms: Some("40000".into()),
            owners: vec![ActivityWaitOwner::Session { id: "62".into(), generation: None }],
        }),
        query_hash: None,
        statement_identity: Some(
            NativeIdentity::new("AB".repeat(64))
                .qualified("native_object", "sys.dm_exec_requests.sql_handle")
                .qualified("database_id", "7")
                .qualified("statement_start_offset", "0")
                .qualified("statement_end_offset", "-1"),
        ),
        client: Some(ActivityClient {
            application: Some("collector-name".into()),
            user: Some(String::new()),
            host: None,
        }),
        statement,
    }
}

/// Complete metadata, incomplete native coverage and genuinely empty/unavailable sources stay distinct.
pub fn activity_evidence_cases() -> Vec<(CollectionDescription, Vec<TelemetryRecord>)> {
    let mut first = activity_observation(DefinitionPayload::sql(
        "SELECT payload FROM dbo.orders WHERE tenant_id=9264 AND token='private-activity-canary'",
        Engine::Mssql,
    ));
    first.waiting_on_lock = Some(true);
    let mut page = first.clone();
    page.key.session.id = "63".into();
    page.open_transactions = Some(0);
    page.elapsed_ms = Some("1".into());
    page.statement_identity = None;
    page.statement = DefinitionPayload::Unavailable { reason: PayloadUnavailable::NotCollected };
    page.blocking_status = ActivityBlockingStatus::Unknown;
    page.waiting_on_lock = Some(false);
    page.wait = Some(ActivityWait {
        identified_resources: vec![ObservedWaitResource {
            kind: WaitResourceKind::MemoryPage,
            native: NativeIdentity::new("7:1:4144").qualified("source", "native-wait-resource"),
        }],
        native_type: Some("PAGELATCH_EX".into()),
        elapsed_ms: None,
        owners: vec![ActivityWaitOwner::Unknown { native_id: "-5".into(), reason: UnknownWaitOwner::UntrackedLatch }],
        resources: Some(vec![]),
    });
    let mut idle = first.clone();
    idle.key.session.id = "64".into();
    idle.key.request = None;
    idle.key.transaction = Some("native-transaction".into());
    idle.idle = true;
    idle.waiting_on_lock = None;
    idle.scope = ActivityScopeBasis::TransactionDatabase;
    idle.state = "idle".into();
    idle.open_transactions = Some(1);
    idle.elapsed_ms = None;
    idle.transaction_age_ms = Some("18446744073709551615".into());
    idle.request_counters = None;
    idle.wait = None;
    idle.statement_identity = None;
    idle.client = None;
    idle.statement = DefinitionPayload::Unavailable { reason: PayloadUnavailable::Restricted };
    let rows = vec![first, page, idle].into_iter().map(TelemetryRecord::from).collect::<Vec<_>>();
    let source = frame(
        rows.len() as u32,
        Some("native-source-generation".into()),
        StateSourceCoverage::collected(culprit_core::source_id::SESSIONS.into(), rows.len() as u32),
    );
    let partial_rows = rows[..2].to_vec();
    let mut partial = StateSourceCoverage::observed(culprit_core::source_id::SESSIONS.into(), 2, false);
    partial.stopped = Some(CollectionStop::RowLimit);
    vec![
        (source, rows),
        (frame(3, None, partial), partial_rows),
        (
            frame(
                0,
                Some("native-source-generation".into()),
                StateSourceCoverage::collected(culprit_core::source_id::SESSIONS.into(), 0),
            ),
            vec![],
        ),
        (
            frame(
                0,
                None,
                StateSourceCoverage::unavailable(
                    culprit_core::source_id::SESSIONS.into(),
                    culprit_core::ErrorCode::PermissionDenied,
                    None,
                ),
            ),
            vec![],
        ),
    ]
}

fn frame(native_rows: u32, epoch: Option<String>, coverage: StateSourceCoverage) -> CollectionDescription {
    let description = CollectionDescription {
        target: culprit_core::DatabaseIdentity::server(Engine::Mssql, "db", "fixture.invalid", 1433, None).into(),
        source: culprit_core::source_id::SESSIONS.into(),
        continuity: SourceContinuity::Unverified,
        observation: SourceObservation::Activity(ActivitySnapshotDescription {
            epoch,
            observed_from: "2020-01-01T12:00:00.000000000Z".into(),
            at: "2020-01-01T12:00:00.000100000Z".into(),
            observed_until: "2020-01-01T12:00:00.000200000Z".into(),
            scope: MeasurementScope::Database,
            population: ActivityPopulation::ActiveRequestsOpenTransactionsAndBlockers,
            native_rows,
            observations: coverage.records,
            coverage,
        }),
    };
    assert!(description.observation.activity().unwrap().is_valid());
    description
}

pub fn assert_activity_capture(
    capture: &crate::analytical::evidence::EvidenceCapture,
    source: &CollectionDescription,
    rows: &[TelemetryRecord],
) {
    assert!(capture.is_read_complete(), "{:?}", capture.gaps());
    assert_eq!(capture.source(), source);
    assert_eq!(capture.records().len(), rows.len());
    for record in rows {
        let expected = ExportedRecord::new(source, capture.capture(), record.clone()).unwrap();
        assert!(capture.records().iter().any(|row| row.row() == &expected), "exact native activity metadata");
    }
    let encoded = serde_json::to_string(capture.records()).unwrap();
    assert!(!encoded.contains("private-activity-canary"));
    assert!(!encoded.contains("9264"));
}
