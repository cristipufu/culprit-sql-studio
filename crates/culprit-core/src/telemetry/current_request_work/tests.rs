use super::*;

fn snapshot() -> CurrentRequestWorkSnapshot {
    let mut context = super::super::native_read_context::tests::read_context();
    context.target = DatabaseIdentity::server(Engine::Mysql, "sample", "owned.invalid", 3306, None).into();
    let SourcePartition::InstanceLocal { member: Some(member), .. } = &mut context.partition else { panic!() };
    member.engine = Engine::Mysql;
    let session = NativeActivityIdentity { id: "42".into(), generation: "73".into() };
    let request = NativeActivityIdentity { id: "91".into(), generation: "0".into() };
    let statement_identity = crate::engines::mysql_activity_statement_identity(&session, &request).unwrap();
    CurrentRequestWorkSnapshot {
        source: "mysql-current-request-work".into(),
        context,
        coverage: StateSourceCoverage::observed("mysql-current-request-work".into(), 1, false),
        result: FocusedRequestResult {
            focus: None,
            database_writable: Some(true),
            binding: FocusedRequestBinding::Independent,
            membership: FocusedRequestMembership::Observed,
        },
        observations: vec![CurrentRequestWorkObservation {
            observed_at: "2020-01-01T12:00:00.5Z".into(),
            actor: ActivityKey { session, request: Some(request), transaction: None },
            database: "sample".into(),
            scope: ActivityScopeBasis::RequestDatabase,
            statement_identity,
            statement: DefinitionPayload::sql("SELECT value FROM sample.items", Engine::Mysql),
            native: MysqlCurrentRequestWork {
                thread_id: "73".into(),
                statement_event_id: "91".into(),
                timer_start_ps: "0".into(),
                timer_wait_ps: Some("18446744073709551615".into()),
                rows_examined: Some("9007199254740993".into()),
                rows_sent: None,
                created_tmp_tables: Some("2".into()),
                created_tmp_disk_tables: Some("0".into()),
                sort_merge_passes: None,
                stage: MysqlStageObservation::Observed {
                    event: Box::new(MysqlStageEvent {
                        thread_id: "73".into(),
                        event_id: "92".into(),
                        nesting_event_id: "91".into(),
                        nesting_event_type: "STATEMENT".into(),
                        event_name: "stage/sql/Creating sort index".into(),
                        timer_start_ps: None,
                        timer_wait_ps: None,
                        work_completed: Some("0".into()),
                        work_estimated: Some("9007199254740993".into()),
                    }),
                },
            },
        }],
    }
}

fn source(input: &CurrentRequestWorkSnapshot) -> CollectionDescription {
    CollectionDescription {
        target: input.context.target.clone(),
        partition: input.context.partition.clone(),
        source: input.source.clone(),
        continuity: SourceContinuity::Unverified,
        observation: SourceObservation::CurrentRequestWork {
            snapshot: StateSnapshotDescription {
                dataset: SnapshotDataset::CurrentRequestWork,
                observed_from: input.context.observed_from.clone(),
                observed_until: input.context.observed_until.clone(),
                records: input.coverage.records,
                sources: vec![input.coverage.clone()],
            },
            result: Box::new(input.result.clone()),
        },
    }
}

fn focus(input: &CurrentRequestWorkSnapshot, partition: SourcePartition) -> ActivityRequestFocus {
    let point = &input.observations[0];
    let mut original = super::super::activity::tests::request("42");
    original.key = point.actor.clone();
    original.database = Some("sample".into());
    original.scope = point.scope;
    original.statement_identity = Some(point.statement_identity.clone());
    original.statement = point.statement.clone();
    let original_source = CollectionDescription {
        target: input.context.target.clone(),
        partition,
        source: "mysql-native-activity".into(),
        continuity: SourceContinuity::Unverified,
        observation: SourceObservation::Activity(ActivitySnapshotDescription {
            epoch: input.context.partition.native_epoch().map(str::to_string),
            database_writable: input.context.database_writable,
            observed_from: "2020-01-01T12:00:00Z".into(),
            observed_until: "2020-01-01T12:00:00Z".into(),
            at: "2020-01-01T12:00:00Z".into(),
            scope: crate::MeasurementScope::Database,
            population: ActivityPopulation::DefaultDatabaseConnections,
            native_rows: 1,
            observations: 1,
            coverage: StateSourceCoverage::observed("mysql-native-activity".into(), 1, false),
        }),
    };
    let capture = CaptureIdentity {
        observation_id: "original-activity".into(),
        revision: Some(CaptureRevision {
            outbox_id: digest(b"original current-work fixture"),
            owner_epoch: "1".into(),
            sequence: "1".into(),
        }),
    };
    let exported = ExportedRecord::new(&original_source, &capture, original.clone().into()).unwrap();
    let parent =
        DiagnosticEvidenceRef::from_export(&original_source, &digest(b"original manifest"), &exported, 0).unwrap();
    ActivityRequestFocus {
        parent,
        target: original_source.target,
        partition: original_source.partition,
        database_writable: input.context.database_writable,
        source: original_source.source,
        epoch: input.context.partition.native_epoch().unwrap().into(),
        original_observed_at: "2020-01-01T12:00:00Z".into(),
        statement_identity: point.statement_identity.clone(),
        original,
    }
}

#[test]
fn exact_request_counters_and_untimed_linked_stages_roundtrip_without_inventing_timing_or_work_units() {
    let input = snapshot();
    assert!(input.is_valid());
    let recovered: CurrentRequestWorkSnapshot = serde_json::from_slice(&serde_json::to_vec(&input).unwrap()).unwrap();
    assert_eq!(input, recovered);
    assert_eq!(recovered.observations[0].native.rows_examined.as_deref(), Some("9007199254740993"));
    let MysqlStageObservation::Observed { event } = &recovered.observations[0].native.stage else { panic!() };
    assert_eq!(event.timer_start_ps, None);
    assert_eq!(event.work_completed.as_deref(), Some("0"));
    assert_eq!(recovered.observations[0].native.created_tmp_disk_tables.as_deref(), Some("0"));
    assert!(input.observations[0].fits_source_scope(&source(&input)));
}

#[test]
fn thread_statement_event_timer_and_native_stage_parent_mismatches_never_attach_work() {
    for case in ["thread", "event", "start", "statement", "stage-thread", "stage-parent", "stage-type", "stage-event"] {
        let mut input = snapshot();
        let point = &mut input.observations[0];
        match case {
            "thread" => point.native.thread_id = "74".into(),
            "event" => point.native.statement_event_id = "92".into(),
            "start" => point.native.timer_start_ps = "1".into(),
            "statement" => point.statement_identity = NativeIdentity::new("another-statement"),
            _ => {
                let MysqlStageObservation::Observed { event } = &mut point.native.stage else { panic!() };
                match case {
                    "stage-thread" => event.thread_id = "74".into(),
                    "stage-parent" => event.nesting_event_id = "90".into(),
                    "stage-type" => event.nesting_event_type = "STAGE".into(),
                    _ => event.event_id = event.nesting_event_id.clone(),
                }
            }
        }
        assert!(!input.is_valid(), "{case}");
    }
}

#[test]
fn stage_absence_disabled_permissions_and_unlinked_parentage_remain_different_native_states() {
    let mut values = Vec::new();
    for stage in [
        MysqlStageObservation::NotObserved,
        MysqlStageObservation::Unlinked,
        MysqlStageObservation::Unavailable { reason: MysqlStageUnavailable::InstrumentationDisabled },
        MysqlStageObservation::Unavailable { reason: MysqlStageUnavailable::PermissionDenied },
        MysqlStageObservation::Unavailable { reason: MysqlStageUnavailable::Unsupported },
    ] {
        let mut input = snapshot();
        input.observations[0].native.stage = stage;
        assert!(input.is_valid());
        values.push(serde_json::to_string(&input.observations[0].native.stage).unwrap());
    }
    values.sort();
    values.dedup();
    assert_eq!(values.len(), 5);
}

#[test]
fn unknown_original_member_identity_never_becomes_matched_from_the_current_reader() {
    let mut input = snapshot();
    let SourcePartition::InstanceLocal { member: _, epoch } = &input.context.partition else { panic!() };
    input.result.focus =
        Some(Box::new(focus(&input, SourcePartition::InstanceLocal { member: None, epoch: epoch.clone() })));
    input.result.binding = FocusedRequestBinding::UnverifiedOriginalScope;
    assert!(!input.is_valid(), "an incomplete original local owner cannot supply a durable focus");
    input.result.focus = Some(Box::new(focus(&input, SourcePartition::Target)));
    assert!(input.is_valid(), "a logical original namespace stays explicitly unverified");
    input.result.binding = FocusedRequestBinding::Matched;
    assert!(!input.is_valid());
    input.result.focus = Some(Box::new(focus(&input, input.context.partition.clone())));
    assert!(input.is_valid());
    let SourcePartition::InstanceLocal { member: Some(member), .. } = &mut input.context.partition else { panic!() };
    member.native.id = "another-instance".into();
    input.result.binding = FocusedRequestBinding::UnverifiedOriginalScope;
    assert!(!input.is_valid(), "a changed qualified owner is not an unknown original owner");
    let SourcePartition::InstanceLocal { epoch, .. } = &mut input.context.partition else { panic!() };
    *epoch = None;
    assert!(!input.is_valid(), "a missing epoch cannot hide a known conflicting member");
}

#[test]
fn readback_gaps_preserve_the_native_membership_and_foreign_or_session_context_is_not_upgraded() {
    let mut input = snapshot();
    input.observations[0].scope = ActivityScopeBasis::SessionDatabase;
    assert!(input.is_valid());
    assert_eq!(input.observations[0].scope, ActivityScopeBasis::SessionDatabase);
    input.observations[0].database = "other".into();
    assert!(!input.is_valid());
    input = snapshot();
    input.observations.clear();
    assert!(!input.is_valid());
    assert!(input.is_valid_read(false));
    assert_eq!(input.result.membership, FocusedRequestMembership::Observed);
    assert_eq!(input.coverage.records, 1);
    input.coverage.records = 0;
    assert!(!input.is_valid_read(false));
}

#[test]
fn export_sanitizes_sql_without_changing_exact_work_or_native_event_identity() {
    let mut input = snapshot();
    let private = "SELECT 'private-literal' FROM sample.items";
    input.observations[0].statement = DefinitionPayload::Available {
        text: private.into(),
        sha256: digest(private.as_bytes()),
        redaction_version: REDACTION_VERSION,
    };
    let point = input.observations[0].clone();
    let exported = ExportedRecord::new(
        &source(&input),
        &CaptureIdentity { observation_id: "current-work".into(), revision: None },
        TelemetryRecord::Operational(OperationalObservation {
            source: input.source.clone(),
            key: point.key().unwrap(),
            fact: OperationalFact::CurrentRequestWork(Box::new(point.clone())),
        }),
    )
    .unwrap();
    let encoded = serde_json::to_string(&exported).unwrap();
    assert!(!encoded.contains("private-literal"));
    let TelemetryRecord::Operational(OperationalObservation {
        fact: OperationalFact::CurrentRequestWork(observed),
        ..
    }) = exported.record
    else {
        panic!()
    };
    assert_eq!(point.native, observed.native);
    assert_eq!(point.statement_identity, observed.statement_identity);
}
