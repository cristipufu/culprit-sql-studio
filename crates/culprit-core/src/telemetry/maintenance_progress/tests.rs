use super::*;

mod waits;

fn snapshot() -> MaintenanceProgressSnapshot {
    let mut context = super::super::native_read_context::tests::read_context();
    context.target =
        crate::DatabaseIdentity::server(crate::Engine::Postgres, "sample", "owned.invalid", 5432, None).into();
    let SourcePartition::InstanceLocal { member: Some(member), .. } = &mut context.partition else { unreachable!() };
    member.engine = crate::Engine::Postgres;
    MaintenanceProgressSnapshot {
        source: "pg_stat_progress_create_index".into(),
        context,
        coverage: StateSourceCoverage::observed("pg_stat_progress_create_index".into(), 1, false),
        result: FocusedRequestResult {
            focus: None,
            database_writable: Some(true),
            binding: FocusedRequestBinding::Independent,
            membership: FocusedRequestMembership::Observed,
        },
        observations: vec![MaintenanceProgressObservation {
            observed_at: "2020-01-01T12:00:00.5Z".into(),
            actor: ActivityKey {
                session: NativeActivityIdentity { id: "42".into(), generation: "backend-start".into() },
                request: Some(NativeActivityIdentity { id: "statement".into(), generation: "query-start".into() }),
                transaction: None,
            },
            statement: Some(NativeIdentity::new("native-statement")),
            database_oid: "16384".into(),
            relation_oid: "16420".into(),
            index_oid: Some("0".into()),
            command: MaintenanceCommand::CreateIndex,
            phase: "building index: scanning table".into(),
            work: MaintenanceProgressWork::PostgreSqlIndexBuild {
                progress: Box::new(PostgresIndexBuildProgress {
                    blocks_total: Some("9007199254740993".into()),
                    blocks_done: Some("0".into()),
                    current_locker_pid: Some("0".into()),
                    ..Default::default()
                }),
            },
        }],
    }
}

#[test]
fn non_index_work_preserves_command_specific_units_and_native_index_availability() {
    let variants = [
        (
            MaintenanceCommand::Vacuum,
            None,
            MaintenanceProgressWork::PostgreSqlVacuum {
                progress: Box::new(PostgresVacuumProgress {
                    heap_blks_scanned: Some("9007199254740993".into()),
                    max_dead_tuples: None,
                    max_dead_tuple_bytes: Some("18446744073709551615".into()),
                    dead_tuple_bytes: Some("0".into()),
                    cost_delay_ms: Some("0".into()),
                    ..Default::default()
                }),
            },
        ),
        (
            MaintenanceCommand::Analyze,
            None,
            MaintenanceProgressWork::PostgreSqlAnalyze {
                progress: Box::new(PostgresAnalyzeProgress {
                    sample_blks_scanned: Some("0".into()),
                    current_child_table_relid: Some("0".into()),
                    cost_delay_ms: Some("0.25".into()),
                    ..Default::default()
                }),
            },
        ),
        (
            MaintenanceCommand::Cluster,
            Some("0".into()),
            MaintenanceProgressWork::PostgreSqlCluster {
                progress: Box::new(PostgresClusterProgress {
                    heap_tuples_written: Some("9007199254740993".into()),
                    ..Default::default()
                }),
            },
        ),
    ];
    let mut keys = Vec::new();
    for (command, index_oid, work) in variants {
        let mut input = snapshot();
        let point = &mut input.observations[0];
        point.command = command;
        point.index_oid = index_oid;
        point.work = work;
        assert!(point.is_valid());
        keys.push(point.key().unwrap());
        let encoded = serde_json::to_vec(point).unwrap();
        let decoded: MaintenanceProgressObservation = serde_json::from_slice(&encoded).unwrap();
        assert_eq!(*point, decoded);
        assert!(input.is_valid());
        input.observations[0].command = MaintenanceCommand::CreateIndex;
        assert!(!input.is_valid(), "an index command cannot relabel a vacuum/analyze/rewrite population");
    }
    keys.sort();
    keys.dedup();
    assert_eq!(keys.len(), 3, "one request's VACUUM/ANALYZE phases do not collide");
}

#[test]
fn nonfinite_native_delay_and_non_oid_child_references_are_not_valid_progress_evidence() {
    for delay in ["NaN", "inf", "-1", "1e999", ""] {
        let mut input = snapshot();
        let point = &mut input.observations[0];
        point.command = MaintenanceCommand::Vacuum;
        point.index_oid = None;
        point.work = MaintenanceProgressWork::PostgreSqlVacuum {
            progress: Box::new(PostgresVacuumProgress { cost_delay_ms: Some(delay.into()), ..Default::default() }),
        };
        assert!(!input.is_valid(), "{delay}");
    }
    let mut input = snapshot();
    let point = &mut input.observations[0];
    point.command = MaintenanceCommand::Analyze;
    point.index_oid = None;
    point.work = MaintenanceProgressWork::PostgreSqlAnalyze {
        progress: Box::new(PostgresAnalyzeProgress {
            current_child_table_relid: Some("9007199254740993".into()),
            ..Default::default()
        }),
    };
    assert!(!input.is_valid(), "a large exact counter is not a PostgreSQL OID");
}

fn focus(snapshot: &MaintenanceProgressSnapshot) -> ActivityRequestFocus {
    let point = &snapshot.observations[0];
    let mut original = super::super::activity::tests::request("42");
    original.key = point.actor.clone();
    original.statement_identity = point.statement.clone();
    original.scope = ActivityScopeBasis::SessionDatabase;
    let source = CollectionDescription {
        target: snapshot.context.target.clone(),
        partition: snapshot.context.partition.clone(),
        source: "pg_stat_activity".into(),
        continuity: SourceContinuity::Unverified,
        observation: SourceObservation::Activity(ActivitySnapshotDescription {
            epoch: match &snapshot.context.partition {
                SourcePartition::InstanceLocal { epoch, .. } => epoch.clone(),
                _ => None,
            },
            database_writable: snapshot.context.database_writable,
            observed_from: "2020-01-01T11:59:59Z".into(),
            observed_until: "2020-01-01T12:00:00Z".into(),
            at: "2020-01-01T11:59:59.5Z".into(),
            scope: crate::MeasurementScope::Database,
            population: ActivityPopulation::ClientBackends,
            native_rows: 1,
            observations: 1,
            coverage: StateSourceCoverage::collected("pg_stat_activity".into(), 1),
        }),
    };
    let capture = CaptureIdentity {
        observation_id: "original-activity".into(),
        revision: Some(CaptureRevision {
            outbox_id: digest(b"maintenance fixture"),
            owner_epoch: "1".into(),
            sequence: "1".into(),
        }),
    };
    let exported = ExportedRecord::new(&source, &capture, original.clone().into()).unwrap();
    let parent = DiagnosticEvidenceRef::from_export(&source, &digest(b"original manifest"), &exported, 0).unwrap();
    ActivityRequestFocus {
        parent,
        target: source.target,
        partition: source.partition,
        database_writable: snapshot.context.database_writable,
        source: source.source,
        epoch: snapshot.context.partition.native_epoch().unwrap().into(),
        original_observed_at: "2020-01-01T11:59:59.5Z".into(),
        statement_identity: point.statement.clone().unwrap(),
        original,
    }
}

#[test]
fn exact_phase_counts_and_unknown_native_index_oid_roundtrip_without_percent_or_resource_ownership() {
    let snapshot = snapshot();
    assert!(snapshot.is_valid());
    let point = &snapshot.observations[0];
    assert!(point.relation_reference().is_some());
    assert!(point.index_reference().is_none());
    let encoded = serde_json::to_string(&snapshot).unwrap();
    assert!(encoded.contains("\"9007199254740993\""));
    for absent in ["percent", "allocated_bytes", "owner_sql", "completed_at"] {
        assert!(!encoded.contains(absent));
    }
    assert_eq!(serde_json::from_str::<MaintenanceProgressSnapshot>(&encoded).unwrap(), snapshot);
}

#[test]
fn actor_generation_and_native_objects_change_the_key_while_phase_work_does_not() {
    let point = snapshot().observations.remove(0);
    let key = point.key().unwrap();
    let mut changed = point.clone();
    changed.phase = "waiting for writers before build".into();
    assert_eq!(changed.key().unwrap(), key);
    changed.actor.request.as_mut().unwrap().generation = "later-statement".into();
    assert_ne!(changed.key().unwrap(), key);
    changed = point;
    changed.index_oid = Some("16500".into());
    assert_ne!(changed.key().unwrap(), key);
}

#[test]
fn missing_original_member_never_borrows_the_current_qualified_member() {
    let mut snapshot = snapshot();
    let mut original = focus(&snapshot);
    original.partition = SourcePartition::Target;
    snapshot.result.focus = Some(Box::new(original));
    snapshot.result.binding = FocusedRequestBinding::Matched;
    assert!(!snapshot.is_valid());
    snapshot.result.binding = FocusedRequestBinding::UnverifiedOriginalScope;
    assert!(snapshot.is_valid());
    assert_eq!(snapshot.result.original_reference(), snapshot.result.focus.as_ref().map(|focus| &focus.parent));
}

#[test]
fn exact_original_member_epoch_role_actor_and_statement_are_needed_for_a_matched_point() {
    let mut original = snapshot();
    original.result.focus = Some(Box::new(focus(&original)));
    original.result.binding = FocusedRequestBinding::Matched;
    assert!(original.is_valid());
    for field in ["member", "epoch", "role", "actor", "statement", "future"] {
        let mut changed = original.clone();
        let focus = changed.result.focus.as_mut().unwrap();
        match field {
            "member" => {
                let SourcePartition::InstanceLocal { member: Some(member), .. } = &mut focus.partition else {
                    unreachable!()
                };
                member.native.id = "another-native-member".into();
            }
            "epoch" => {
                let SourcePartition::InstanceLocal { epoch, .. } = &mut focus.partition else { unreachable!() };
                *epoch = Some("another-process".into());
                focus.epoch = "another-process".into();
            }
            "role" => focus.database_writable = Some(false),
            "actor" => changed.observations[0].actor.request.as_mut().unwrap().generation = "recycled".into(),
            "statement" => changed.observations[0].statement = Some(NativeIdentity::new("other-statement")),
            _ => focus.original_observed_at = "2020-01-01T12:00:02Z".into(),
        }
        assert!(!changed.is_valid(), "{field}");
    }
}

#[test]
fn disappearance_and_unavailable_progress_never_become_a_completion_verdict() {
    let mut snapshot = snapshot();
    snapshot.observations.clear();
    snapshot.coverage = StateSourceCoverage::observed(snapshot.source.clone(), 0, false);
    snapshot.result.membership = FocusedRequestMembership::NotObserved;
    assert!(snapshot.is_valid());
    snapshot.result.membership = FocusedRequestMembership::Observed;
    assert!(!snapshot.is_valid());
    snapshot.result.membership = FocusedRequestMembership::Unavailable;
    snapshot.coverage =
        StateSourceCoverage::unavailable(snapshot.source.clone(), crate::ErrorCode::PermissionDenied, None);
    assert!(snapshot.is_valid());
    assert!(!serde_json::to_string(&snapshot).unwrap().contains("completed"));
}

#[test]
fn ysql_backfill_estimates_are_distinct_from_postgresql_phase_heap_work() {
    let mut snapshot = snapshot();
    snapshot.observations[0].work = MaintenanceProgressWork::YsqlIndexBuild {
        progress: Box::new(YsqlIndexBuildProgress {
            tuples_done: Some("9007199254740993".into()),
            tuples_total_estimate: Some("42".into()),
        }),
    };
    assert!(!snapshot.is_valid());
    snapshot.context.target =
        crate::DatabaseIdentity::server(crate::Engine::Yugabyte, "sample", "owned.invalid", 5433, None).into();
    let SourcePartition::InstanceLocal { member: Some(member), .. } = &mut snapshot.context.partition else {
        unreachable!()
    };
    member.engine = crate::Engine::Yugabyte;
    member.kind = SharedResourceKind::Node;
    snapshot.context.database_writable = None;
    snapshot.result.database_writable = None;
    assert!(snapshot.is_valid(), "estimates may be lower than completed work; no fraction is inferred");
}

fn source(snapshot: &MaintenanceProgressSnapshot) -> CollectionDescription {
    CollectionDescription {
        target: snapshot.context.target.clone(),
        partition: snapshot.context.partition.clone(),
        source: "postgres-maintenance-progress".into(),
        continuity: SourceContinuity::Unverified,
        observation: SourceObservation::MaintenanceProgress {
            snapshot: StateSnapshotDescription {
                dataset: SnapshotDataset::MaintenanceProgress,
                observed_from: snapshot.context.observed_from.clone(),
                observed_until: snapshot.context.observed_until.clone(),
                records: snapshot.coverage.records,
                sources: vec![snapshot.coverage.clone()],
            },
            result: Box::new(snapshot.result.clone()),
        },
    }
}

#[test]
fn source_and_raw_carrier_require_the_same_local_owner_for_complete_positive_or_empty_coverage() {
    for records in [0, 1] {
        let mut point = snapshot();
        point.context.partition = SourcePartition::Target;
        if records == 0 {
            point.observations.clear();
            point.result.membership = FocusedRequestMembership::NotObserved;
        }
        point.coverage = StateSourceCoverage::collected(point.source.clone(), records);
        assert!(!point.is_valid());
        assert!(!source(&point).partition_is_consistent());
        point.coverage = StateSourceCoverage::observed(point.source.clone(), records, false);
        assert!(point.is_valid());
        assert!(source(&point).partition_is_consistent());
    }
}

#[test]
fn unverified_original_scope_never_bypasses_a_known_target_member_or_role_conflict() {
    let mut point = snapshot();
    point.result.focus = Some(Box::new(focus(&point)));
    point.result.binding = FocusedRequestBinding::UnverifiedOriginalScope;
    assert!(!point.is_valid(), "qualified original cannot be relabeled unverified");
    let original = point.result.focus.as_mut().unwrap();
    original.partition = SourcePartition::Target;
    assert!(point.is_valid());
    point.result.focus.as_mut().unwrap().database_writable = Some(false);
    assert!(!point.is_valid());
    point.result.focus.as_mut().unwrap().database_writable = Some(true);
    point.result.focus.as_mut().unwrap().target =
        crate::DatabaseIdentity::server(crate::Engine::Postgres, "other", "owned.invalid", 5432, None).into();
    assert!(!point.is_valid());
    point.result.focus = None;
    point.result.binding = FocusedRequestBinding::Independent;
    let SourcePartition::InstanceLocal { member: Some(member), .. } = &point.context.partition else { unreachable!() };
    point.context.target = CollectionTarget::Shared(member.clone());
    assert!(!point.is_valid(), "native index progress is database scoped");
}

#[test]
fn snapshot_sharing_redacts_native_owner_and_original_sql_without_breaking_binding_or_clocks() {
    let mut point = snapshot();
    point.result.focus = Some(Box::new(focus(&point)));
    point.result.binding = FocusedRequestBinding::Matched;
    let original = point.result.focus.as_mut().unwrap();
    original.original.statement = DefinitionPayload::Available {
        text: "CREATE INDEX ix ON sample(value) WHERE value = 'private-canary'".into(),
        sha256: digest(b"CREATE INDEX ix ON sample(value) WHERE value = 'private-canary'"),
        redaction_version: REDACTION_VERSION,
    };
    let mut snapshot: crate::Snapshot =
        serde_json::from_str(include_str!("../../../../../fixtures/postgres-demo.json")).unwrap();
    snapshot.operations.maintenance_progress = Some(point.clone());
    let shared = crate::redact::redact_snapshot(&snapshot);
    let retained = shared.operations.maintenance_progress.as_ref().unwrap();
    assert!(retained.is_valid());
    assert_eq!(retained.context.observed_from, point.context.observed_from);
    assert_eq!(retained.context.observed_until, point.context.observed_until);
    assert_eq!(retained.result.database_writable, point.result.database_writable);
    let body = serde_json::to_string(retained).unwrap();
    assert!(!body.contains("private-canary"));
    assert!(!body.contains("owned.invalid"));
    assert_eq!(
        serde_json::to_string(&crate::redact::redact_snapshot(&shared)).unwrap(),
        serde_json::to_string(&shared).unwrap()
    );
}
