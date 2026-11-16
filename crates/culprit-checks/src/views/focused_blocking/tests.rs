use super::*;
use crate::views::maintenance::tests as native;
use culprit_core::{Engine, MaintenanceDirection, MeasurementScope};

fn evidence(qualified_original: bool) -> (CollectionDescription, Vec<ActivityObservation>) {
    let mut owner = native::current("43", "VACUUM orders");
    let mut waiter = native::waiting(native::current("42", "SELECT id FROM orders"), &owner);
    for row in [&mut waiter, &mut owner] {
        row.scope = ActivityScopeBasis::SessionDatabase;
        row.statement = match &row.statement {
            DefinitionPayload::Available { text, .. } => DefinitionPayload::sql(text, Engine::Postgres),
            _ => unreachable!(),
        };
    }
    let mut source = native::frame(1);
    source.target =
        culprit_core::DatabaseIdentity::server(Engine::Postgres, "db", "fixture.invalid", 5432, None).into();
    let partition = SourcePartition::InstanceLocal {
        member: Some(SharedResourceIdentity {
            engine: Engine::Postgres,
            kind: SharedResourceKind::Instance,
            namespace: NativeIdentity::new("observed-instance-lineage"),
            native: NativeIdentity::new("observed-native-member"),
        }),
        epoch: Some("native-process".into()),
    };
    if qualified_original {
        source.partition = partition.clone();
    }
    let original = source.observation.activity().unwrap();
    let capture = CaptureIdentity {
        observation_id: "original-blocking-capture".into(),
        revision: Some(CaptureRevision {
            outbox_id: digest(b"controlled blocking parent outbox"),
            owner_epoch: "1".into(),
            sequence: "1".into(),
        }),
    };
    assert!(waiter.is_valid(), "original fixture request: {waiter:?}");
    let exported = ExportedRecord::new(&source, &capture, waiter.clone().into()).unwrap();
    assert!(exported.record.is_valid(), "canonical fixture request: {:?}", exported.record);
    assert_eq!(
        ExportedRecord::new(&source, &capture, exported.record.clone()).unwrap(),
        exported,
        "export must be an idempotent boundary before the original receipt is constructed"
    );
    let focus = ActivityRequestFocus {
        parent: DiagnosticEvidenceRef::from_export(&source, &digest(b"original-blocking-manifest"), &exported, 0)
            .unwrap(),
        target: source.target.clone(),
        partition: source.partition.clone(),
        database_writable: original.database_writable,
        source: source.source.clone(),
        epoch: original.epoch.clone().unwrap(),
        original_observed_at: original.at.clone(),
        statement_identity: waiter.statement_identity.clone().unwrap(),
        original: waiter.clone(),
    };
    let rows = vec![waiter, owner];
    source.partition = partition;
    source.source = "postgres-blocking".into();
    source.observation = SourceObservation::FocusedBlocking {
        snapshot: ActivitySnapshotDescription {
            epoch: Some("native-process".into()),
            database_writable: Some(true),
            observed_from: "2020-01-01T12:00:02Z".into(),
            observed_until: "2020-01-01T12:00:03Z".into(),
            at: "2020-01-01T12:00:02.5Z".into(),
            scope: MeasurementScope::Database,
            population: ActivityPopulation::ActiveRequestsOpenTransactionsAndBlockers,
            native_rows: 2,
            observations: 2,
            coverage: StateSourceCoverage::observed("postgres-blocking".into(), 2, false),
        },
        result: Box::new(ActivityBlockingFocusResult {
            focus,
            binding: if qualified_original {
                FocusedRequestBinding::Matched
            } else {
                FocusedRequestBinding::UnverifiedOriginalScope
            },
            membership: FocusedRequestMembership::Observed,
            request_end: BlockingRequestState::SameStatement,
            closure: BlockingClosureDescription {
                status: BlockingClosureStatus::Observed,
                max_lookups: MAX_BLOCKING_LOOKUPS,
                max_depth: MAX_BLOCKING_DEPTH,
                max_participants: MAX_BLOCKING_PARTICIPANTS,
            },
        }),
    };
    (source, rows)
}

fn report(source: &CollectionDescription, rows: &[ActivityObservation], complete: bool) -> BlockingReport {
    explain(
        source,
        &rows.iter().map(|observation| ActivityFrameRow { observation, reference: None }).collect::<Vec<_>>(),
        complete,
    )
    .unwrap()
}

#[test]
fn native_relationships_feed_the_existing_graph_and_maintenance_participation_without_sql_ownership() {
    let (source, rows) = evidence(true);
    let value = report(&source, &rows, true);
    assert_eq!(value.relationships.len(), 1);
    assert!(value.relationships[0].owner_observed);
    let graph = value.blocking.as_ref().unwrap();
    assert_eq!(graph.chains[0].tree.session, "43");
    assert_eq!(graph.chains[0].tree.context.as_ref().unwrap().sql.as_deref(), Some("VACUUM orders"));
    assert_eq!(value.maintenance.as_ref().unwrap().entries[0].direction, MaintenanceDirection::BlockingSession);
    assert_eq!(value.source_state, SourceState::Partial);
    assert!(value.limitations.contains(&t!("view.blocking.limit.session")));
    assert!(value.limitations.contains(&t!("view.blocking.limit.point")));
}

#[test]
fn native_prepared_zero_and_generation_unknown_owners_never_become_sessions_or_first_request_context() {
    let (source, mut rows) = evidence(true);
    rows[0]
        .wait
        .as_mut()
        .unwrap()
        .owners
        .push(ActivityWaitOwner::Transaction { native_id: "0".into(), owner: TransactionWaitOwner::Prepared });
    let value = report(&source, &rows, true);
    assert_eq!(value.relationships.len(), 2);
    assert!(value.limitations.contains(&t!("view.blocking.limit.prepared")));
    let prepared = value
        .blocking
        .as_ref()
        .unwrap()
        .chains
        .iter()
        .find(|chain| {
            matches!(
                chain.tree.owner,
                Some(ActivityWaitOwner::Transaction { owner: TransactionWaitOwner::Prepared, .. })
            )
        })
        .unwrap();
    assert!(prepared.tree.context.is_none());
    assert_eq!(prepared.tree.session, "0");
    assert_eq!(prepared.headline, t!("core.blocking.headline.prepared", n = 1, count = "1"));

    rows[0].wait.as_mut().unwrap().owners[0] = ActivityWaitOwner::Session { id: "43".into(), generation: None };
    rows.truncate(1);
    let mut source = source;
    let SourceObservation::FocusedBlocking { snapshot, .. } = &mut source.observation else { unreachable!() };
    snapshot.native_rows = 1;
    snapshot.observations = 1;
    snapshot.coverage.records = 1;
    let unknown = report(&source, &rows, true);
    assert!(unknown.relationships.iter().all(|edge| !edge.owner_observed));
    assert!(unknown.blocking.unwrap().chains.iter().all(|chain| chain.tree.context.is_none()));
}

#[test]
fn a_current_qualified_graph_keeps_the_unqualified_original_and_missing_archive_body_explicit() {
    let (source, rows) = evidence(false);
    let value = report(&source, &rows, true);
    assert_eq!(value.binding, FocusedRequestBinding::UnverifiedOriginalScope);
    assert!(value.blocking.is_some(), "current native relationships remain useful on their own");
    assert!(value.limitations.contains(&t!("view.blocking.limit.original_scope")));
    let missing = report(&source, &[], false);
    assert_eq!(missing.native_rows, 2);
    assert_eq!(missing.observed_rows, 0);
    assert_eq!(missing.summary, t!("view.blocking.summary.read_gap"));
    assert!(missing.relationships.is_empty() && missing.blocking.is_none());
    assert!(!missing.read_complete);
}

#[test]
fn final_request_changes_and_limits_preserve_the_original_point_without_claiming_recovery() {
    let (mut source, rows) = evidence(true);
    let SourceObservation::FocusedBlocking { result, .. } = &mut source.observation else { unreachable!() };
    result.request_end = BlockingRequestState::Missing;
    result.closure.status = BlockingClosureStatus::DepthLimit;
    let value = report(&source, &rows, true);
    assert_eq!(value.request_end, BlockingRequestState::Missing);
    assert_eq!(value.relationships.len(), 1);
    assert_eq!(value.source_state, SourceState::Partial);
    assert!(value.limitations.contains(&t!("view.blocking.limit.request_end")));
    assert!(
        value
            .limitations
            .iter()
            .any(|line| *line == t!("view.blocking.limit.bounds", lookups = "64", depth = "8", participants = "65"))
    );
}

#[test]
fn missing_or_reused_original_is_not_a_healthy_empty_closure_or_an_unrelated_new_statement() {
    let (mut source, _) = evidence(true);
    let SourceObservation::FocusedBlocking { snapshot, result } = &mut source.observation else { unreachable!() };
    snapshot.native_rows = 0;
    snapshot.observations = 0;
    snapshot.coverage = StateSourceCoverage::observed(source.source.clone(), 0, false);
    result.binding = FocusedRequestBinding::ParticipantChanged;
    result.membership = FocusedRequestMembership::NotObserved;
    result.request_end = BlockingRequestState::NotRead;
    result.closure.status = BlockingClosureStatus::NotRead;
    let value = report(&source, &[], true);
    assert_eq!(value.summary, t!("view.blocking.summary.changed"));
    assert!(value.blocking.is_none() && value.relationships.is_empty());
    assert_eq!(value.source_state, SourceState::Partial);
}

#[test]
fn a_reported_same_session_owner_stays_a_positive_relationship_with_an_explicit_request_ownership_gap() {
    let (mut source, mut rows) = evidence(true);
    let waiter = rows[0].key.session.clone();
    rows[0].wait.as_mut().unwrap().owners =
        vec![ActivityWaitOwner::Session { id: waiter.id, generation: Some(waiter.generation) }];
    rows.truncate(1);
    let SourceObservation::FocusedBlocking { snapshot, .. } = &mut source.observation else { unreachable!() };
    snapshot.observations = 1;
    snapshot.native_rows = 1;
    snapshot.coverage.records = 1;
    let value = report(&source, &rows, true);
    assert_eq!(value.relationships.len(), 1);
    assert!(value.relationships[0].reports_waiter_session_id());
    assert!(value.blocking.is_none());
    assert!(value.limitations.contains(&t!("view.blocking.limit.self_session")));
    assert_eq!(value.summary, t!("view.blocking.summary.observed", n = 1, count = "1"));
    assert_eq!(value.source_state, SourceState::Partial);
}
