use super::*;
use culprit_core::{Engine, MeasurementScope, ObservedWaitResource, WaitResourceKind};

pub fn activity_observation(statement: DefinitionPayload) -> ActivityObservation {
    ActivityObservation {
        session_context: None,
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
            class: None,
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
    first.request_counters.as_mut().unwrap().page_server_reads = Some("9007199254740993".into());
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
        class: None,
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

/// A separate original Activity population for the controlled scalar cache entry. The SQL token and
/// session generations are recorded facts; no future completed-plan counter is copied into this frame.
pub fn scalar_blocking_activity_evidence() -> (CollectionDescription, Vec<TelemetryRecord>) {
    let (plan_source, definitions) = parameter_evidence(ParameterEvidenceCase::AccessMismatch);
    let TelemetryRecord::ObjectDefinition(definition) = &definitions[0] else { unreachable!() };
    let ObjectDefinitionFact::MssqlRetainedPlan(native) = &definition.fact else { unreachable!() };
    let context = &native.context;
    let mut waiter = activity_observation(DefinitionPayload::Unavailable { reason: PayloadUnavailable::NotCollected });
    waiter.key.session.generation = "2020-01-01T11:00:00.0030000".into();
    waiter.key.request.as_mut().unwrap().generation = "2020-01-01T11:59:58.0030000".into();
    waiter.waiting_on_lock = Some(true);
    waiter.wait.as_mut().unwrap().native_type = Some("LCK_M_S".into());
    waiter.wait.as_mut().unwrap().owners =
        vec![ActivityWaitOwner::Session { id: "62".into(), generation: Some("2020-01-01T11:01:00.0030000".into()) }];
    waiter.statement_identity = Some(
        NativeIdentity::new(context.identity.sql_handle.trim_start_matches("0x"))
            .qualified("native_object", "sys.dm_exec_requests.sql_handle")
            .qualified("database_id", context.database_id.to_string())
            .qualified("statement_start_offset", context.identity.statement_start_bytes.to_string())
            .qualified("statement_end_offset", context.identity.statement_end_bytes.to_string()),
    );
    waiter.query_hash = context.query_hash.as_ref().map(|hash| hash.trim_start_matches("0x").to_owned());
    waiter.database = Some(plan_source.target.database().unwrap().database.clone());
    let mut owner = waiter.clone();
    owner.key = ActivityKey {
        session: NativeActivityIdentity { id: "62".into(), generation: "2020-01-01T11:01:00.0030000".into() },
        request: None,
        transaction: Some("native-original-blocker-transaction".into()),
    };
    owner.idle = true;
    owner.scope = ActivityScopeBasis::TransactionDatabase;
    owner.state = "sleeping".into();
    owner.command = None;
    owner.elapsed_ms = None;
    owner.open_transactions = Some(1);
    owner.transaction_age_ms = Some("40000".into());
    owner.request_counters = None;
    owner.waiting_on_lock = Some(false);
    owner.wait = None;
    owner.statement_identity = None;
    owner.query_hash = None;
    let records = vec![waiter, owner].into_iter().map(TelemetryRecord::from).collect::<Vec<_>>();
    let SourcePartition::InstanceLocal { epoch, .. } = &plan_source.partition else { unreachable!() };
    let mut source =
        frame(2, epoch.clone(), StateSourceCoverage::collected(culprit_core::source_id::SESSIONS.into(), 2));
    source.target = plan_source.target;
    source.partition = plan_source.partition;
    let SourceObservation::Activity(snapshot) = &mut source.observation else { unreachable!() };
    snapshot.database_writable = Some(true);
    // A different native acquisition interval, not a simultaneous-plan or same-clock assertion.
    snapshot.observed_from = "2020-01-01T11:59:59.1000000Z".into();
    snapshot.at = "2020-01-01T11:59:59.1500000Z".into();
    snapshot.observed_until = "2020-01-01T11:59:59.2000000Z".into();
    let borrowed = records
        .iter()
        .map(|record| {
            let TelemetryRecord::Activity(observation) = record else { unreachable!() };
            ActivityFrameRow { observation, reference: None }
        })
        .collect::<Vec<_>>();
    ActivityFrameEvidence::new(&source, &borrowed, true).unwrap();
    (source, records)
}

fn frame(native_rows: u32, epoch: Option<String>, coverage: StateSourceCoverage) -> CollectionDescription {
    let description = CollectionDescription {
        partition: culprit_core::telemetry::SourcePartition::Target,
        target: culprit_core::DatabaseIdentity::server(Engine::Mssql, "db", "fixture.invalid", 1433, None).into(),
        source: culprit_core::source_id::SESSIONS.into(),
        continuity: SourceContinuity::Unverified,
        observation: SourceObservation::Activity(ActivitySnapshotDescription {
            database_writable: None,
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
        super::assert_redacted_definition_payloads(&expected.record);
    }
    let encoded = serde_json::to_string(capture.records()).unwrap();
    assert!(!encoded.contains("private-activity-canary"));
}
