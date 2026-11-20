use super::*;
use culprit_core::{Engine, MeasurementScope, SourceState};

pub(in crate::views) fn current(session: &str, sql: &str) -> ActivityObservation {
    ActivityObservation {
        session_context: None,
        blocking_status: ActivityBlockingStatus::Known,
        waiting_on_lock: Some(false),
        key: ActivityKey {
            session: NativeActivityIdentity { id: session.into(), generation: format!("login-{session}") },
            request: Some(NativeActivityIdentity { id: "0".into(), generation: format!("request-{session}") }),
            transaction: None,
        },
        idle: false,
        scope: ActivityScopeBasis::RequestDatabase,
        state: "running".into(),
        database: Some("db".into()),
        command: None,
        last_request_age_ms: None,
        open_transactions: None,
        elapsed_ms: Some("9007199254740993".into()),
        transaction_age_ms: None,
        session_counters: ActivityCounters { cpu_ms: Some("999".into()), ..Default::default() },
        request_counters: Some(ActivityCounters { cpu_ms: Some("0".into()), ..Default::default() }),
        wait: None,
        query_hash: None,
        statement_identity: Some(NativeIdentity::new(format!("statement-{session}"))),
        client: None,
        statement: DefinitionPayload::sql(sql, Engine::Mssql),
    }
}

pub(in crate::views) fn waiting(mut row: ActivityObservation, owner: &ActivityObservation) -> ActivityObservation {
    row.waiting_on_lock = Some(true);
    row.wait = Some(ActivityWait {
        class: None,
        native_type: Some("LCK_M_X".into()),
        elapsed_ms: Some("1".into()),
        owners: vec![ActivityWaitOwner::Session {
            id: owner.key.session.id.clone(),
            generation: Some(owner.key.session.generation.clone()),
        }],
        identified_resources: vec![],
        resources: None,
    });
    row
}

pub(in crate::views) fn frame(rows: usize) -> CollectionDescription {
    CollectionDescription {
        target: culprit_core::DatabaseIdentity::server(Engine::Mssql, "db", "fixture.invalid", 1433, None).into(),
        partition: SourcePartition::Target,
        source: "captured-activity".into(),
        continuity: SourceContinuity::Unverified,
        observation: SourceObservation::Activity(ActivitySnapshotDescription {
            epoch: Some("native-process".into()),
            database_writable: Some(true),
            observed_from: "2020-01-01T12:00:00Z".into(),
            observed_until: "2020-01-01T12:00:01Z".into(),
            at: "2020-01-01T12:00:00.5Z".into(),
            scope: MeasurementScope::Database,
            population: ActivityPopulation::ActiveRequestsOpenTransactionsAndBlockers,
            native_rows: rows as u32,
            observations: rows as u32,
            coverage: StateSourceCoverage::collected("captured-activity".into(), rows as u32),
        }),
    }
}

fn report(
    source: &CollectionDescription,
    observations: &[ActivityObservation],
    complete: bool,
) -> Result<MaintenanceReport> {
    explain(
        source,
        &observations.iter().map(|observation| ActivityFrameRow { observation, reference: None }).collect::<Vec<_>>(),
        complete,
    )
}

#[test]
fn native_lock_references_distinguish_maintenance_blocking_waiting_and_both_directions() {
    let maintenance = current("57", "ALTER INDEX ix ON dbo.orders REBUILD");
    let waiter = waiting(current("58", "SELECT id FROM dbo.orders"), &maintenance);
    let owner = current("59", "UPDATE dbo.orders SET value = 1");
    let blocking = report(&frame(2), &[maintenance.clone(), waiter.clone()], true).unwrap();
    let entry = &blocking.entries[0];
    assert_eq!(entry.direction, MaintenanceDirection::BlockingSession);
    assert_eq!(entry.affected[0].activity, waiter.key);
    assert_eq!(entry.affected_count, 1);
    assert!(entry.issues.contains(&MaintenanceRelationIssue::SessionOwnershipOnly));
    assert_eq!(entry.explanation, t!("view.maintenance.entry.blocking", count = "1"));

    let maintenance = waiting(maintenance, &owner);
    let victim = report(&frame(2), &[maintenance.clone(), owner.clone()], true).unwrap();
    assert_eq!(victim.entries[0].direction, MaintenanceDirection::Waiting);
    assert_eq!(victim.entries[0].affected_count, 0);
    assert_eq!(victim.entries[0].waiting_on, maintenance.wait.as_ref().unwrap().owners);
    let both = report(&frame(3), &[maintenance, owner, waiter], true).unwrap();
    assert_eq!(both.entries[0].direction, MaintenanceDirection::Both);
    assert_eq!(both.entries[0].investigation, t!("view.maintenance.investigate.both"));
    assert_eq!(both.entries[0].verify, t!("view.maintenance.verify.both"));
}

#[test]
fn multiple_current_requests_on_a_blocking_session_remain_competing_context() {
    let maintenance = current("57", "ALTER INDEX ix ON dbo.orders REBUILD");
    let mut peer = current("57", "SELECT id FROM dbo.orders");
    peer.key.request.as_mut().unwrap().id = "1".into();
    peer.key.request.as_mut().unwrap().generation = "parallel-request".into();
    peer.statement_identity = Some(NativeIdentity::new("other-current-statement"));
    let mut waiter = waiting(current("58", "SELECT id FROM dbo.orders"), &maintenance);
    let repeated_owner = waiter.wait.as_ref().unwrap().owners[0].clone();
    waiter.wait.as_mut().unwrap().owners.push(repeated_owner);
    let result = report(&frame(3), &[maintenance, peer.clone(), waiter], true).unwrap();
    let entry = &result.entries[0];
    assert_eq!(entry.affected_count, 1, "duplicate native owner references do not multiply the affected request");
    assert_eq!(entry.competing_request_count, 1);
    assert_eq!(entry.competing_requests[0].activity, peer.key);
    assert!(entry.issues.contains(&MaintenanceRelationIssue::MultipleOwnerRequests));
    assert!(entry.explanation.contains(&t!("view.maintenance.entry.competing", count = "1")));
}

fn mars_peer_with_lifecycle(original: &ActivityObservation, lifecycle: SessionLifecycle) -> ActivityObservation {
    let mut peer = current(&original.key.session.id, "SELECT id FROM dbo.orders");
    peer.key.session = original.key.session.clone();
    peer.key.request.as_mut().unwrap().id = "1".into();
    peer.key.request.as_mut().unwrap().generation = "peer-request".into();
    peer.statement_identity = Some(NativeIdentity::new("peer-statement"));
    peer.session_context = Some(Box::new(ActivitySessionContext {
        lifecycle,
        login_age_ms: None,
        last_request_end_age_ms: None,
        client_library: None,
        client_process_id: None,
        native: None,
    }));
    peer
}

const SESSION_AUTHORITY_CONTROLS: [(SessionLifecycle, bool); 4] = [
    (SessionLifecycle::Running, true),
    (SessionLifecycle::Unknown, true),
    (SessionLifecycle::Connecting, false),
    (SessionLifecycle::PoolResetPrelogin, false),
];

#[test]
fn a_negative_maintenance_mars_peer_withholds_blocking_and_competing_links_but_keeps_the_raw_entry() {
    for (lifecycle, established) in SESSION_AUTHORITY_CONTROLS {
        let maintenance = current("57", "ALTER INDEX ix ON dbo.orders REBUILD");
        let peer = mars_peer_with_lifecycle(&maintenance, lifecycle);
        let waiter = waiting(current("58", "SELECT id FROM dbo.orders"), &maintenance);
        let observations = [maintenance.clone(), peer, waiter.clone()];
        let original = serde_json::to_value(&observations).unwrap();
        let result = report(&frame(3), &observations, true).unwrap();
        assert_eq!(result.observed_rows, 3);
        assert_eq!(result.native_rows, 3);
        assert_eq!(result.maintenance_observations, 1);
        let entry = &result.entries[0];
        assert_eq!(entry.reference.activity, maintenance.key);
        assert!(!entry.metrics.is_empty());
        assert_eq!(entry.affected_count, u32::from(established), "{lifecycle:?}");
        assert_eq!(entry.competing_request_count, u32::from(established), "{lifecycle:?}");
        assert_eq!(
            entry.direction,
            if established { MaintenanceDirection::BlockingSession } else { MaintenanceDirection::Unlinked },
            "{lifecycle:?}"
        );
        if established {
            assert_eq!(entry.affected[0].activity, waiter.key);
        } else {
            assert!(entry.affected.is_empty() && entry.competing_requests.is_empty() && entry.waiting_on.is_empty());
            assert_eq!(entry.explanation, t!("view.maintenance.entry.unlinked"));
        }
        assert_eq!(serde_json::to_value(&observations).unwrap(), original, "original evidence stays intact");
    }
}

#[test]
fn a_negative_maintenance_mars_peer_cannot_authorize_a_waiting_direction_or_owner_link() {
    for (lifecycle, established) in SESSION_AUTHORITY_CONTROLS {
        let owner = current("59", "UPDATE dbo.orders SET value = 1");
        let maintenance = waiting(current("57", "ALTER INDEX ix ON dbo.orders REBUILD"), &owner);
        let peer = mars_peer_with_lifecycle(&maintenance, lifecycle);
        let observations = [maintenance.clone(), peer, owner];
        let result = report(&frame(3), &observations, true).unwrap();
        let entry = &result.entries[0];
        assert_eq!(entry.reference.activity, maintenance.key);
        assert_eq!(
            entry.direction,
            if established { MaintenanceDirection::Waiting } else { MaintenanceDirection::Unlinked },
            "{lifecycle:?}"
        );
        assert_eq!(entry.waiting_on.len(), usize::from(established), "{lifecycle:?}");
        assert_eq!(entry.competing_request_count, u32::from(established), "{lifecycle:?}");
        assert_eq!(observations[0].wait, maintenance.wait, "the raw native wait is still retained");
    }
}

#[test]
fn a_negative_dependent_mars_peer_cannot_decorate_an_established_maintenance_as_a_blocker() {
    for (lifecycle, established) in SESSION_AUTHORITY_CONTROLS {
        let maintenance = current("57", "ALTER INDEX ix ON dbo.orders REBUILD");
        let waiter = waiting(current("58", "SELECT id FROM dbo.orders"), &maintenance);
        let peer = mars_peer_with_lifecycle(&waiter, lifecycle);
        let result = report(&frame(3), &[maintenance, waiter, peer], true).unwrap();
        let entry = &result.entries[0];
        assert_eq!(entry.affected_count, u32::from(established), "{lifecycle:?}");
        assert_eq!(entry.affected.len(), usize::from(established), "{lifecycle:?}");
        assert_eq!(
            entry.direction,
            if established { MaintenanceDirection::BlockingSession } else { MaintenanceDirection::Unlinked },
            "{lifecycle:?}"
        );
    }
}

#[test]
fn a_negative_owner_mars_peer_keeps_the_maintenance_wait_with_its_owner_unresolved() {
    for (lifecycle, established) in SESSION_AUTHORITY_CONTROLS {
        let owner = current("59", "UPDATE dbo.orders SET value = 1");
        let peer = mars_peer_with_lifecycle(&owner, lifecycle);
        let maintenance = waiting(current("57", "ALTER INDEX ix ON dbo.orders REBUILD"), &owner);
        let result = report(&frame(3), &[maintenance, owner, peer], true).unwrap();
        let entry = &result.entries[0];
        assert_eq!(entry.direction, MaintenanceDirection::Waiting, "{lifecycle:?}");
        assert_eq!(entry.waiting_on.len(), usize::from(established), "{lifecycle:?}");
        assert_eq!(entry.issues.contains(&MaintenanceRelationIssue::WaitOwnerUnavailable), !established);
        assert_eq!(entry.affected_count, 0);
    }
}

#[test]
fn a_different_disqualified_session_does_not_withdraw_an_established_maintenance_relationship() {
    let maintenance = current("57", "ALTER INDEX ix ON dbo.orders REBUILD");
    let waiter = waiting(current("58", "SELECT id FROM dbo.orders"), &maintenance);
    let unrelated = mars_peer_with_lifecycle(&current("59", "SELECT id FROM dbo.orders"), SessionLifecycle::Connecting);
    let result = report(&frame(3), &[maintenance, waiter.clone(), unrelated], true).unwrap();
    let entry = &result.entries[0];
    assert_eq!(entry.direction, MaintenanceDirection::BlockingSession);
    assert_eq!(entry.affected_count, 1);
    assert_eq!(entry.affected[0].activity, waiter.key);
}

#[test]
fn missing_reused_or_non_lock_owner_references_never_link_maintenance_by_numeric_session_alone() {
    let maintenance = current("57", "ALTER INDEX ix ON dbo.orders REBUILD");
    for shape in ["missing-generation", "changed-generation", "not-lock", "unknown-blocking", "self"] {
        let mut waiter = waiting(current("58", "SELECT id FROM dbo.orders"), &maintenance);
        match shape {
            "missing-generation" => {
                waiter.wait.as_mut().unwrap().owners =
                    vec![ActivityWaitOwner::Session { id: "57".into(), generation: None }]
            }
            "changed-generation" => {
                waiter.wait.as_mut().unwrap().owners =
                    vec![ActivityWaitOwner::Session { id: "57".into(), generation: Some("other-login".into()) }]
            }
            "not-lock" => waiter.waiting_on_lock = Some(false),
            "unknown-blocking" => waiter.blocking_status = ActivityBlockingStatus::Unknown,
            "self" => {
                waiter = waiting(maintenance.clone(), &maintenance);
            }
            _ => unreachable!(),
        }
        let observations = if shape == "self" { vec![waiter] } else { vec![maintenance.clone(), waiter] };
        let result = report(&frame(observations.len()), &observations, true).unwrap();
        assert_eq!(result.entries[0].affected_count, 0, "{shape}");
        assert!(result.entries[0].affected.is_empty(), "{shape}");
    }
}

#[test]
fn unknown_scope_withholds_cross_participant_links_but_preserves_the_maintenance_lock_wait() {
    let maintenance = current("57", "ALTER INDEX ix ON dbo.orders REBUILD");
    let maintenance = waiting(maintenance, &current("59", "SELECT id FROM dbo.orders"));
    let waiter = waiting(current("58", "SELECT id FROM dbo.orders"), &maintenance);
    let mut source = frame(2);
    source.partition = SourcePartition::InstanceLocal { member: None, epoch: None };
    let SourceObservation::Activity(native) = &mut source.observation else { unreachable!() };
    native.epoch = None;
    native.coverage = StateSourceCoverage::observed(source.source.clone(), 2, false);
    let result = report(&source, &[maintenance, waiter], true).unwrap();
    assert_eq!(result.source_state, SourceState::Partial);
    assert_eq!(result.entries[0].direction, MaintenanceDirection::Waiting);
    assert_eq!(result.entries[0].affected_count, 0);
    assert!(result.entries[0].issues.contains(&MaintenanceRelationIssue::NativeScopeUnqualified));
    assert!(result.limitations.contains(&t!("view.maintenance.limit.scope")));
}

#[test]
fn unavailable_statement_native_partial_and_read_gaps_never_become_complete_maintenance_absence() {
    let mut observation = current("57", "SELECT id FROM dbo.orders");
    observation.statement = DefinitionPayload::Unavailable { reason: PayloadUnavailable::NotCollected };
    let result = report(&frame(1), &[observation], true).unwrap();
    assert_eq!(result.unclassified_requests, 1);
    assert_eq!(result.maintenance_observations, 0);
    assert!(result.limitations.contains(&t!("view.maintenance.limit.classification", count = "1")));

    let maintenance = current("57", "ALTER INDEX ix ON dbo.orders REBUILD");
    let source = frame(2);
    let incomplete = report(&source, std::slice::from_ref(&maintenance), false).unwrap();
    assert_eq!(incomplete.source_state, SourceState::Partial);
    assert_eq!(incomplete.native_rows, 2);
    assert_eq!(incomplete.observed_rows, 1);
    assert_eq!(incomplete.entries.len(), 1);
    assert!(incomplete.limitations.contains(&t!("view.maintenance.limit.read")));
    assert!(report(&source, &[maintenance], true).is_err(), "a missing frame row cannot be called complete");

    let mut source = frame(0);
    let SourceObservation::Activity(native) = &mut source.observation else { unreachable!() };
    native.epoch = None;
    native.coverage =
        StateSourceCoverage::unavailable(source.source.clone(), culprit_core::ErrorCode::PermissionDenied, None);
    let unavailable = report(&source, &[], true).unwrap();
    assert_eq!(unavailable.source_state, SourceState::Unavailable);
    assert!(unavailable.limitations.contains(&t!("view.maintenance.limit.unavailable")));
}

#[test]
fn request_counters_stay_exact_and_never_borrow_session_lifetimes_or_raw_sql() {
    let row = current("57", "ALTER INDEX ix ON dbo.orders REBUILD");
    let result = report(&frame(1), &[row], true).unwrap();
    let metrics = &result.entries[0].metrics;
    assert_eq!(
        metrics.iter().find(|metric| metric.is("view.maintenance.metric.elapsed")).unwrap().text.as_deref(),
        Some("9007199254740993 ms")
    );
    assert_eq!(
        metrics.iter().find(|metric| metric.is("view.maintenance.metric.cpu")).unwrap().text.as_deref(),
        Some("0 ms")
    );
    let encoded = serde_json::to_string(&result).unwrap();
    assert!(!encoded.contains("ALTER INDEX") && !encoded.contains("999"));
    let entry = &serde_json::to_value(&result).unwrap()["entries"][0];
    assert!(entry.get("sql").is_none() && entry.get("progress").is_none() && entry.get("fix_sql").is_none());
}

#[test]
fn duplicate_or_conflicting_generations_and_wrong_frame_bounds_are_rejected() {
    let row = current("57", "ALTER INDEX ix ON dbo.orders REBUILD");
    assert!(report(&frame(2), &[row.clone(), row.clone()], true).is_err());
    let mut reused = row.clone();
    reused.key.session.generation = "reused-login".into();
    assert!(report(&frame(2), &[row.clone(), reused], true).is_err());
    let mut wrong_database = row.clone();
    wrong_database.database = Some("other-db".into());
    assert!(report(&frame(1), &[wrong_database], true).is_err());
    let mut source = frame(1);
    let SourceObservation::Activity(native) = &mut source.observation else { unreachable!() };
    native.observed_until = "2019-12-31T12:00:00Z".into();
    assert!(report(&source, &[row], true).is_err());
}

#[test]
fn bounded_related_references_keep_exact_totals_and_do_not_choose_one_waiter() {
    let maintenance = current("57", "ALTER INDEX ix ON dbo.orders REBUILD");
    let mut observations = vec![maintenance.clone()];
    observations.extend(
        (0..20).map(|index| waiting(current(&format!("waiter-{index}"), "SELECT id FROM dbo.orders"), &maintenance)),
    );
    let result = report(&frame(observations.len()), &observations, true).unwrap();
    assert_eq!(result.entries[0].affected_count, 20);
    assert_eq!(result.entries[0].affected.len(), MAX_MAINTENANCE_REFERENCES_PER_ENTRY);
    assert!(result.entries[0].issues.contains(&MaintenanceRelationIssue::ReferencesLimited));
    let observations = (0..MAX_MAINTENANCE_ENTRIES + 1)
        .map(|index| current(&index.to_string(), "ALTER INDEX ix ON dbo.orders REBUILD"))
        .collect::<Vec<_>>();
    let result = report(&frame(observations.len()), &observations, true).unwrap();
    assert_eq!(result.maintenance_observations as usize, observations.len());
    assert_eq!(result.entries.len(), MAX_MAINTENANCE_ENTRIES);
}

#[test]
fn all_source_grammars_use_the_same_directional_report_without_native_command_guessing() {
    for engine in Engine::ALL {
        let sql = match engine.sql_syntax() {
            culprit_core::SqlSyntax::Mssql => "ALTER INDEX ix ON orders REBUILD",
            culprit_core::SqlSyntax::Postgres => "REINDEX INDEX ix",
            culprit_core::SqlSyntax::Mysql => "OPTIMIZE TABLE orders",
            culprit_core::SqlSyntax::Sqlite => "REINDEX ix",
        };
        let mut row = current("57", sql);
        row.statement = DefinitionPayload::sql(sql, engine);
        let mut source = frame(1);
        let CollectionTarget::Database(database) = &mut source.target else { unreachable!() };
        database.engine = engine;
        let result = report(&source, &[row], true).unwrap();
        assert_eq!(result.entries.len(), 1, "{engine:?}");
        assert_eq!(result.entries[0].direction, MaintenanceDirection::Unlinked);
    }
}

#[test]
fn a_positive_lock_wait_stays_visible_when_native_owner_coverage_is_unknown() {
    let mut maintenance =
        waiting(current("57", "ALTER INDEX ix ON dbo.orders REBUILD"), &current("59", "SELECT id FROM dbo.orders"));
    maintenance.blocking_status = ActivityBlockingStatus::Unknown;
    maintenance.wait.as_mut().unwrap().owners.clear();
    let result = report(&frame(1), &[maintenance], true).unwrap();
    assert_eq!(result.entries[0].direction, MaintenanceDirection::Waiting);
    assert_eq!(result.entries[0].affected_count, 0);
    assert!(result.entries[0].issues.contains(&MaintenanceRelationIssue::WaitOwnerUnavailable));
}

fn receipt(source: &CollectionDescription, row: &ActivityObservation, ordinal: u32) -> DiagnosticEvidenceRef {
    let capture = CaptureIdentity {
        observation_id: "maintenance-fixture".into(),
        revision: Some(CaptureRevision {
            outbox_id: digest(b"maintenance-outbox"),
            owner_epoch: "1".into(),
            sequence: "1".into(),
        }),
    };
    let exported = ExportedRecord::new(source, &capture, TelemetryRecord::from(row.clone())).unwrap();
    DiagnosticEvidenceRef::from_export(source, &digest(b"original manifest"), &exported, ordinal).unwrap()
}

fn with_receipt<'a>(
    observation: &'a ActivityObservation,
    reference: &'a DiagnosticEvidenceRef,
) -> ActivityFrameRow<'a> {
    ActivityFrameRow { observation, reference: Some(reference) }
}

#[test]
fn original_source_capture_and_payload_receipts_cannot_be_reassigned_to_different_activity() {
    let row = current("57", "ALTER INDEX ix ON dbo.orders REBUILD");
    let source = frame(1);
    let original = receipt(&source, &row, 0);
    let result = explain(&source, &[with_receipt(&row, &original)], true).unwrap();
    assert_eq!(result.entries[0].reference.evidence.as_ref(), Some(&original));
    let mut outside_frame = original.clone();
    outside_frame.row_ordinal = 1;
    assert!(explain(&source, &[with_receipt(&row, &outside_frame)], true).is_err());
    for field in ["source", "capture", "payload", "native-key"] {
        let mut changed = original.clone();
        match field {
            "source" => changed.source_scope_id = digest(b"another native source"),
            "capture" => changed.capture.revision.as_mut().unwrap().sequence = "2".into(),
            "payload" => changed.payload_sha256 = digest(b"another native body"),
            "native-key" => {
                let other = current("58", "ALTER INDEX ix ON dbo.orders REBUILD");
                changed.native_key = receipt(&source, &other, 0).native_key;
            }
            _ => unreachable!(),
        }
        assert!(explain(&source, &[with_receipt(&row, &changed)], true).is_err(), "{field}");
    }
    let mut changed_row = row.clone();
    changed_row.key.request.as_mut().unwrap().generation = "reused-request".into();
    assert!(
        explain(&source, &[ActivityFrameRow { observation: &changed_row, reference: Some(&original) }], true).is_err()
    );
}

#[test]
fn mixed_maintenance_roles_preserve_exact_original_incoming_and_outgoing_references() {
    let upstream = current("59", "UPDATE dbo.orders SET value = 9264");
    let maintenance = waiting(current("57", "ALTER INDEX ix ON dbo.orders REBUILD"), &upstream);
    let affected = waiting(current("58", "SELECT id FROM dbo.orders"), &maintenance);
    let observations = [maintenance, upstream, affected];
    let original = serde_json::to_value(&observations).unwrap();
    let source = frame(observations.len());
    let receipts =
        observations.iter().enumerate().map(|(index, row)| receipt(&source, row, index as u32)).collect::<Vec<_>>();
    let rows = observations.iter().zip(&receipts).map(|(row, receipt)| with_receipt(row, receipt)).collect::<Vec<_>>();
    let result = explain(&source, &rows, true).unwrap();
    let entry = &result.entries[0];
    assert_eq!(entry.direction, MaintenanceDirection::Both);
    assert_eq!(entry.reference.activity, observations[0].key);
    assert_eq!(entry.reference.evidence.as_ref(), Some(&receipts[0]));
    assert_eq!(entry.waiting_on, observations[0].wait.as_ref().unwrap().owners);
    assert_eq!(entry.affected_count, 1);
    assert_eq!(entry.affected[0].activity, observations[2].key);
    assert_eq!(entry.affected[0].evidence.as_ref(), Some(&receipts[2]));
    assert!(entry.issues.contains(&MaintenanceRelationIssue::SessionOwnershipOnly));
    assert_eq!(entry.investigation, t!("view.maintenance.investigate.both"));
    assert_eq!(entry.verify, t!("view.maintenance.verify.both"));
    assert!(result.limitations.contains(&t!("view.maintenance.limit.history")));
    assert_eq!(serde_json::to_value(&observations).unwrap(), original);
    let json = serde_json::to_string(&result).unwrap();
    assert!(!json.contains("ALTER INDEX") && !json.contains("UPDATE dbo"));
}

#[test]
fn a_missing_upstream_owner_keeps_both_observed_directions_without_manufacturing_owner_context() {
    let maintenance = waiting(
        current("57", "ALTER INDEX ix ON dbo.orders REBUILD"),
        &current("59", "UPDATE dbo.orders SET value = 9264"),
    );
    let affected = waiting(current("58", "SELECT id FROM dbo.orders"), &maintenance);
    let result = report(&frame(2), &[maintenance, affected], true).unwrap();
    let entry = &result.entries[0];
    assert_eq!(entry.direction, MaintenanceDirection::Both);
    assert!(entry.waiting_on.is_empty());
    assert!(entry.issues.contains(&MaintenanceRelationIssue::WaitOwnerUnavailable));
    assert_eq!(entry.affected_count, 1);
    assert_eq!(entry.investigation, t!("view.maintenance.investigate.both"));
    assert!(entry.explanation.contains(&t!("view.maintenance.entry.owner_unknown")));
    assert!(entry.competing_requests.is_empty());
}

#[test]
fn an_unqualified_scope_does_not_turn_a_waiting_rebuild_into_a_two_direction_cause() {
    let maintenance = waiting(
        current("57", "ALTER INDEX ix ON dbo.orders REBUILD"),
        &current("59", "UPDATE dbo.orders SET value = 9264"),
    );
    let affected = waiting(current("58", "SELECT id FROM dbo.orders"), &maintenance);
    let mut source = frame(2);
    source.partition = SourcePartition::InstanceLocal { member: None, epoch: None };
    let SourceObservation::Activity(native) = &mut source.observation else { unreachable!() };
    native.epoch = None;
    native.coverage = StateSourceCoverage::observed(source.source.clone(), 2, false);
    let result = report(&source, &[maintenance, affected], true).unwrap();
    let entry = &result.entries[0];
    assert_eq!(entry.direction, MaintenanceDirection::Waiting);
    assert!(entry.affected.is_empty());
    assert_eq!(entry.investigation, t!("view.maintenance.investigate.waiting"));
    assert_eq!(entry.verify, t!("view.maintenance.verify"));
}

#[test]
fn independent_incoming_or_outgoing_controls_keep_their_original_single_role_advice() {
    for (incoming, outgoing, expected) in [
        (false, true, MaintenanceDirection::BlockingSession),
        (true, false, MaintenanceDirection::Waiting),
        (false, false, MaintenanceDirection::Unlinked),
    ] {
        let owner = current("59", "UPDATE dbo.orders SET value = 9264");
        let mut maintenance = current("57", "ALTER INDEX ix ON dbo.orders REBUILD");
        if incoming {
            maintenance = waiting(maintenance, &owner);
        }
        let mut observations = vec![maintenance.clone(), owner];
        if outgoing {
            observations.push(waiting(current("58", "SELECT id FROM dbo.orders"), &maintenance));
        }
        let result = report(&frame(observations.len()), &observations, true).unwrap();
        let entry = &result.entries[0];
        assert_eq!(entry.direction, expected);
        assert_ne!(entry.investigation, t!("view.maintenance.investigate.both"));
        assert_eq!(entry.verify, t!("view.maintenance.verify"));
    }
}

#[test]
fn archive_references_must_share_one_capture_and_unique_original_positions() {
    let maintenance = current("57", "ALTER INDEX ix ON dbo.orders REBUILD");
    let waiter = waiting(current("58", "SELECT id FROM dbo.orders"), &maintenance);
    let source = frame(2);
    let owner_reference = receipt(&source, &maintenance, 0);
    let waiter_reference = receipt(&source, &waiter, 1);
    assert_eq!(
        explain(
            &source,
            &[with_receipt(&maintenance, &owner_reference), with_receipt(&waiter, &waiter_reference)],
            true
        )
        .unwrap()
        .entries[0]
            .affected_count,
        1
    );
    let duplicate_position = receipt(&source, &waiter, 0);
    assert!(
        explain(
            &source,
            &[with_receipt(&maintenance, &owner_reference), with_receipt(&waiter, &duplicate_position)],
            true
        )
        .is_err()
    );
    let mut other_capture = waiter_reference.clone();
    other_capture.manifest_sha256 = digest(b"other manifest");
    assert!(
        explain(&source, &[with_receipt(&maintenance, &owner_reference), with_receipt(&waiter, &other_capture)], true)
            .is_err()
    );
    assert!(
        explain(
            &source,
            &[
                ActivityFrameRow { observation: &maintenance, reference: Some(&owner_reference) },
                ActivityFrameRow { observation: &waiter, reference: None }
            ],
            true
        )
        .is_err()
    );
}
