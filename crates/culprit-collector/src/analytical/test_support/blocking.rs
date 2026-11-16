//! One canonical retained lock closure for warehouse, offline-consumer and privacy conformance.
use super::*;
use culprit_core::{Engine, MeasurementScope};

const SOURCE: &str = "postgres-blocking";
const EPOCH: &str = "private-blocking-native-process";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BlockingCase {
    Matched,
    UnverifiedOriginal,
    IdleOwner,
    PreparedOwner,
    HiddenOwner,
    LookupLimited,
    RequestChanged,
    RequestMissing,
    ParticipantGone,
    Unavailable,
}

pub fn blocking_evidence_cases() -> Vec<(CollectionDescription, Vec<TelemetryRecord>)> {
    use BlockingCase::*;
    [
        Matched,
        UnverifiedOriginal,
        IdleOwner,
        PreparedOwner,
        HiddenOwner,
        LookupLimited,
        RequestChanged,
        RequestMissing,
        ParticipantGone,
        Unavailable,
    ]
    .into_iter()
    .map(blocking_evidence)
    .collect()
}

pub fn blocking_evidence(case: BlockingCase) -> (CollectionDescription, Vec<TelemetryRecord>) {
    let mut waiter = participant("42", "SELECT id FROM orders WHERE tenant_id=9264");
    waiter.waiting_on_lock = Some(true);
    waiter.wait = Some(ActivityWait {
        class: None,
        identified_resources: vec![],
        native_type: Some("Lock: relation".into()),
        elapsed_ms: Some("40000".into()),
        owners: vec![],
        resources: None,
    });
    let mut original_source = original_source();
    if case == BlockingCase::UnverifiedOriginal {
        original_source.partition = SourcePartition::Target;
    }
    let recovery = Recovery::capture(
        "fixture:blocking-parent",
        &digest(b"blocking-native-schema"),
        original_source.clone(),
        vec![waiter.clone().into()],
        850_000,
    );
    let exported = ExportedRecord::new(&original_source, &recovery.manifest.capture, waiter.clone().into()).unwrap();
    let focus = ActivityRequestFocus {
        parent: DiagnosticEvidenceRef::from_export(
            &original_source,
            &recovery.manifest.capture_manifest_sha256,
            &exported,
            0,
        )
        .unwrap(),
        target: original_source.target.clone(),
        partition: original_source.partition.clone(),
        database_writable: Some(true),
        source: original_source.source.clone(),
        epoch: EPOCH.into(),
        original_observed_at: "2020-01-01T12:00:00Z".into(),
        statement_identity: waiter.statement_identity.clone().unwrap(),
        original: waiter.clone(),
    };
    assert!(focus.matches_parent(&original_source, &waiter, &focus.parent));
    let mut owner = participant("43", "VACUUM orders");
    owner.wait = None;
    owner.waiting_on_lock = Some(false);
    if case == BlockingCase::IdleOwner {
        owner.key.request = None;
        owner.key.transaction = Some("2020-01-01T11:57:00Z".into());
        owner.idle = true;
        owner.state = "idle in transaction".into();
        owner.transaction_age_ms = Some("180000".into());
        owner.open_transactions = Some(1);
        owner.request_counters = None;
        owner.statement_identity = None;
        owner.statement = DefinitionPayload::Unavailable { reason: PayloadUnavailable::NotCollected };
    }
    waiter.wait.as_mut().unwrap().owners = match case {
        BlockingCase::PreparedOwner => {
            vec![ActivityWaitOwner::Transaction { native_id: "0".into(), owner: TransactionWaitOwner::Prepared }]
        }
        BlockingCase::HiddenOwner | BlockingCase::LookupLimited => {
            vec![ActivityWaitOwner::Session { id: "43".into(), generation: None }]
        }
        _ => vec![ActivityWaitOwner::Session {
            id: owner.key.session.id.clone(),
            generation: Some(owner.key.session.generation.clone()),
        }],
    };
    let membership = match case {
        BlockingCase::ParticipantGone => FocusedRequestMembership::NotObserved,
        BlockingCase::Unavailable => FocusedRequestMembership::Unavailable,
        _ => FocusedRequestMembership::Observed,
    };
    let rows = if membership != FocusedRequestMembership::Observed {
        vec![]
    } else if matches!(case, BlockingCase::PreparedOwner | BlockingCase::HiddenOwner | BlockingCase::LookupLimited) {
        vec![waiter]
    } else {
        vec![waiter, owner]
    };
    let mut coverage = if case == BlockingCase::Unavailable {
        StateSourceCoverage::unavailable(SOURCE.into(), culprit_core::ErrorCode::PermissionDenied, None)
    } else {
        StateSourceCoverage::observed(SOURCE.into(), rows.len() as u32, false)
    };
    if case == BlockingCase::LookupLimited {
        coverage.stopped = Some(CollectionStop::RowLimit);
    }
    let result = ActivityBlockingFocusResult {
        focus,
        membership,
        binding: match case {
            BlockingCase::UnverifiedOriginal => FocusedRequestBinding::UnverifiedOriginalScope,
            BlockingCase::ParticipantGone => FocusedRequestBinding::ParticipantMissing,
            _ => FocusedRequestBinding::Matched,
        },
        request_end: match case {
            BlockingCase::RequestChanged => BlockingRequestState::Changed,
            BlockingCase::RequestMissing => BlockingRequestState::Missing,
            _ if membership == FocusedRequestMembership::Observed => BlockingRequestState::SameStatement,
            _ => BlockingRequestState::NotRead,
        },
        closure: BlockingClosureDescription {
            status: match case {
                BlockingCase::LookupLimited => BlockingClosureStatus::LookupLimit,
                _ if membership == FocusedRequestMembership::Observed => BlockingClosureStatus::Observed,
                _ => BlockingClosureStatus::NotRead,
            },
            max_lookups: MAX_BLOCKING_LOOKUPS,
            max_depth: MAX_BLOCKING_DEPTH,
            max_participants: MAX_BLOCKING_PARTICIPANTS,
        },
    };
    let source = CollectionDescription {
        target: original_source.target,
        partition: partition(),
        source: SOURCE.into(),
        continuity: SourceContinuity::Unverified,
        observation: SourceObservation::FocusedBlocking {
            snapshot: ActivitySnapshotDescription {
                epoch: Some(EPOCH.into()),
                database_writable: Some(true),
                observed_from: "2020-01-01T12:00:01Z".into(),
                observed_until: "2020-01-01T12:00:02Z".into(),
                at: "2020-01-01T12:00:01.1Z".into(),
                scope: MeasurementScope::Database,
                population: ActivityPopulation::ActiveRequestsOpenTransactionsAndBlockers,
                native_rows: rows.len() as u32,
                observations: rows.len() as u32,
                coverage,
            },
            result: Box::new(result),
        },
    };
    let borrowed = rows.iter().map(|observation| ActivityFrameRow { observation, reference: None }).collect::<Vec<_>>();
    let frame = ActivityFrameEvidence::new(&source, &borrowed, true).unwrap();
    let SourceObservation::FocusedBlocking { result, .. } = &source.observation else { unreachable!() };
    assert!(result.accepts_frame(&frame), "{case:?}");
    (source, rows.into_iter().map(TelemetryRecord::from).collect())
}

fn original_source() -> CollectionDescription {
    CollectionDescription {
        target: culprit_core::DatabaseIdentity::server(Engine::Postgres, "db", "fixture.invalid", 5432, None).into(),
        partition: partition(),
        source: "pg_stat_activity".into(),
        continuity: SourceContinuity::Unverified,
        observation: SourceObservation::Activity(ActivitySnapshotDescription {
            epoch: Some(EPOCH.into()),
            database_writable: Some(true),
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
fn partition() -> SourcePartition {
    SourcePartition::InstanceLocal {
        member: Some(SharedResourceIdentity {
            engine: Engine::Postgres,
            kind: SharedResourceKind::Instance,
            namespace: NativeIdentity::new("private-blocking-native-lineage"),
            native: NativeIdentity::new("private-blocking-native-member"),
        }),
        epoch: Some(EPOCH.into()),
    }
}
fn participant(id: &str, sql: &str) -> ActivityObservation {
    let mut row = activity_observation(DefinitionPayload::sql(sql, Engine::Postgres));
    row.key = ActivityKey {
        session: NativeActivityIdentity { id: id.into(), generation: format!("2020-01-01T11:00:{id}Z") },
        request: Some(NativeActivityIdentity { id: "statement".into(), generation: "2020-01-01T11:59:00Z".into() }),
        transaction: None,
    };
    row.scope = ActivityScopeBasis::SessionDatabase;
    row.command = None;
    row.state = "active".into();
    row.statement_identity = Some(NativeIdentity::new(format!("native-pg-statement-{id}")));
    row.client = None;
    row.elapsed_ms = Some("60000".into());
    row.session_counters = ActivityCounters::default();
    row.request_counters = Some(ActivityCounters::default());
    row
}

pub fn assert_blocking_capture(
    capture: &crate::analytical::evidence::EvidenceCapture,
    source: &CollectionDescription,
    rows: &[TelemetryRecord],
) {
    assert_activity_capture(capture, source, rows);
    let references = capture
        .records()
        .iter()
        .map(|row| {
            DiagnosticEvidenceRef::from_export(
                source,
                capture.capture_manifest_sha256(),
                row.row(),
                row.row_ordinal().checked_sub(1).unwrap(),
            )
            .unwrap()
        })
        .collect::<Vec<_>>();
    let activity = capture
        .records()
        .iter()
        .zip(&references)
        .map(|(row, reference)| {
            let TelemetryRecord::Activity(observation) = &row.row().record else { panic!("typed Activity row") };
            ActivityFrameRow { observation, reference: Some(reference) }
        })
        .collect::<Vec<_>>();
    let frame = ActivityFrameEvidence::new(source, &activity, true).unwrap();
    let SourceObservation::FocusedBlocking { result, .. } = &source.observation else { panic!("focused closure") };
    assert!(result.accepts_frame(&frame));
}
