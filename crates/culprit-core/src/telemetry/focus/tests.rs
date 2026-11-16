use super::*;

fn focus() -> ActivityWaitFocus {
    let mut original = super::super::activity::tests::request("42");
    let resource = ObservedWaitResource { kind: WaitResourceKind::MemoryPage, native: NativeIdentity::new("7:1:4144") };
    let statement_identity = NativeIdentity::new("native-statement").qualified("offset", "2");
    original.statement_identity = Some(statement_identity.clone());
    original.wait = Some(ActivityWait {
        class: None,
        native_type: Some("PAGELATCH_EX".into()),
        identified_resources: vec![resource.clone()],
        elapsed_ms: Some("1".into()),
        owners: vec![],
        resources: None,
    });
    let capture = CaptureIdentity {
        observation_id: "baseline".into(),
        revision: Some(CaptureRevision {
            outbox_id: "focus-fixture".into(),
            owner_epoch: "1".into(),
            sequence: "1".into(),
        }),
    };
    ActivityWaitFocus {
        participant: ActivityRequestFocus {
            database_writable: None,
            partition: crate::telemetry::SourcePartition::Target,
            parent: DiagnosticEvidenceRef {
                batch_id: batch_id(&capture.observation_id, TelemetryStream::ActivityObservations).unwrap(),
                capture,
                manifest_sha256: digest(b"manifest"),
                source_scope_id: digest(b"scope"),
                stream: TelemetryStream::ActivityObservations,
                native_key: NativeRecordIdentity::Activity(original.key.clone()),
                record_id: digest(b"record"),
                payload_sha256: digest(b"payload"),
                representation_version: REPRESENTATION_VERSION,
                row_ordinal: 0,
            },
            target: crate::DatabaseIdentity::server(crate::Engine::Mssql, "sample", "fixture.invalid", 1433, None)
                .into(),
            source: "native-activity".into(),
            epoch: "native-epoch".into(),
            original_observed_at: "2020-01-01T00:00:00Z".into(),
            original,
            statement_identity,
        },
        resource,
    }
}

#[test]
fn one_original_row_and_native_token_are_required_without_interpreting_provider_components() {
    let focus = focus();
    assert!(focus.is_valid());
    assert_eq!(serde_json::from_str::<ActivityWaitFocus>(&serde_json::to_string(&focus).unwrap()).unwrap(), focus);
    for field in ["parent-key", "statement", "wait", "resource", "scope", "database", "request"] {
        let mut changed = focus.clone();
        match field {
            "parent-key" => changed.participant.original.key.session.generation = "reused".into(),
            "statement" => changed.participant.statement_identity.id = "different-token".into(),
            "wait" => changed.participant.original.wait.as_mut().unwrap().native_type = None,
            "resource" => changed.resource.native.id = "7:1:4145".into(),
            "scope" => changed.participant.original.scope = ActivityScopeBasis::OtherDatabase,
            "database" => changed.participant.original.database = Some("another-db".into()),
            _ => {
                changed.participant.original.key.request = None;
                changed.participant.original.request_counters = None;
            }
        }
        assert!(!changed.is_valid(), "{field}");
    }
}

#[test]
fn source_declared_session_database_context_does_not_authorize_request_database_questions() {
    let mut point = focus();
    point.participant.original.scope = ActivityScopeBasis::SessionDatabase;
    assert!(point.participant.is_valid());
    assert!(!point.is_valid(), "page questions retain their native request-database requirement");
    let mut original = point.participant;
    original.partition = SourcePartition::InstanceLocal {
        member: Some(SharedResourceIdentity {
            engine: crate::Engine::Mssql,
            kind: SharedResourceKind::Instance,
            namespace: NativeIdentity::new("native-machine"),
            native: NativeIdentity::new("native-member"),
        }),
        epoch: Some(original.epoch.clone()),
    };
    original.original.wait.as_mut().unwrap().class = Some(ActivityWaitClass::ExecutionGrant);
    assert!(original.is_valid());
    assert!(!original.is_request_memory_focus());
    original.original.wait.as_mut().unwrap().class = Some(ActivityWaitClass::CompilationAdmission);
    assert!(!original.is_compilation_admission_focus());
}

#[test]
fn compiler_capture_namespaces_bind_the_exact_original_question_and_ignore_followup_read_clocks() {
    let mut original = focus().participant;
    original.partition = SourcePartition::InstanceLocal {
        member: Some(SharedResourceIdentity {
            engine: crate::Engine::Mssql,
            kind: SharedResourceKind::Instance,
            namespace: NativeIdentity::new("native-machine"),
            native: NativeIdentity::new("native-member"),
        }),
        epoch: Some(original.epoch.clone()),
    };
    original.original.wait.as_mut().unwrap().class = Some(ActivityWaitClass::CompilationAdmission);
    let mut source = CollectionDescription {
        target: original.target.clone(),
        partition: original.partition.clone(),
        source: crate::source_id::COMPILATION_GATEWAYS.into(),
        continuity: SourceContinuity::Unverified,
        observation: SourceObservation::FocusedCompilationAdmission {
            snapshot: StateSnapshotDescription {
                dataset: SnapshotDataset::CompilerAdmission,
                observed_from: "2020-01-01T00:00:01Z".into(),
                observed_until: "2020-01-01T00:00:02Z".into(),
                records: 0,
                sources: vec![StateSourceCoverage::collected(crate::source_id::COMPILATION_GATEWAYS.into(), 0)],
            },
            result: Box::new(ActivityCompilationAdmissionFocusResult {
                focus: original,
                observed_from: "2020-01-01T00:00:01Z".into(),
                observed_until: "2020-01-01T00:00:02Z".into(),
                source: FocusSourceState::Matched,
                membership: CompilationAdmissionMembership::NotObserved,
                gateways: vec![],
            }),
        },
    };
    let scope = source.captured_range().unwrap().scope_id;
    let record = source.scoped_id("native-observation", &"native-row").unwrap();
    let SourceObservation::FocusedCompilationAdmission { snapshot, result } = &mut source.observation else {
        unreachable!()
    };
    snapshot.observed_until = "2020-01-01T00:00:03Z".into();
    result.observed_until = snapshot.observed_until.clone();
    assert_eq!(source.captured_range().unwrap().scope_id, scope);
    assert_eq!(source.scoped_id("native-observation", &"native-row").unwrap(), record);
    let SourceObservation::FocusedCompilationAdmission { result, .. } = &mut source.observation else { unreachable!() };
    result.focus.parent.manifest_sha256 = digest(b"another original capture");
    assert!(result.is_valid());
    assert_ne!(source.captured_range().unwrap().scope_id, scope);
    assert_ne!(source.scoped_id("native-observation", &"native-row").unwrap(), record);
}

#[test]
fn a_shared_followup_keeps_the_database_parent_and_verifies_a_distinct_native_resource_epoch() {
    let resource = SharedResourceIdentity {
        engine: crate::Engine::Mssql,
        kind: SharedResourceKind::Instance,
        namespace: NativeIdentity::new("native-machine"),
        native: NativeIdentity::new("native-member"),
    };
    let mut original = focus();
    original.participant.partition = SourcePartition::InstanceLocal {
        member: Some(resource.clone()),
        epoch: Some(original.participant.epoch.clone()),
    };
    let request = CollectionRequest {
        subject: CollectionSubject::Shared { resource: resource.clone() },
        selection: CollectionSelection::SharedPageWaitFocus {
            focus: Box::new(original.clone()),
            partition: SourcePartition::InstanceLocal {
                member: Some(resource.clone()),
                epoch: Some("native-shared-process-epoch".into()),
            },
        },
        budget: CollectionBudget::default(),
    };
    request.validate().unwrap();
    let snapshot = StateSnapshotDescription {
        dataset: SnapshotDataset::PageWaitTargets,
        observed_from: "2020-01-01T00:00:01Z".into(),
        observed_until: "2020-01-01T00:00:02Z".into(),
        records: 0,
        sources: vec![StateSourceCoverage::observed("page-targets".into(), 0, false)],
    };
    let mut source = CollectionDescription {
        target: CollectionTarget::Shared(resource),
        partition: SourcePartition::InstanceLocal {
            member: match &request.subject {
                CollectionSubject::Shared { resource } => Some(resource.clone()),
                _ => unreachable!(),
            },
            epoch: Some("native-shared-process-epoch".into()),
        },
        source: "page-targets".into(),
        continuity: SourceContinuity::Unverified,
        observation: SourceObservation::FocusedPageWait {
            result: Box::new(ActivityWaitFocusResult {
                focus: original.clone(),
                observed_from: snapshot.observed_from.clone(),
                observed_until: snapshot.observed_until.clone(),
                source: FocusSourceState::Matched,
                request: PageWaitRequestState::Missing,
                wait: PageWaitRelation::Unqualified,
            }),
            snapshot,
        },
    };
    assert!(source.matches_request(&request));
    assert!(original.participant.target.database().is_some(), "the original waiter stays in its application database");
    let mut wrong_subject = request.clone();
    let CollectionSubject::Shared { resource } = &mut wrong_subject.subject else { panic!() };
    resource.native.id = "another-native-member".into();
    assert!(wrong_subject.validate().is_err());
    assert!(!source.matches_request(&wrong_subject));
    source.partition = SourcePartition::InstanceLocal { member: None, epoch: None };
    assert!(!original.matches_capture_scope(&source));
    assert!(!source.matches_request(&request));
}

#[test]
fn focus_identity_binds_original_receipt_and_client_without_a_warehouse_range_requirement() {
    let first = focus();
    let key = first.key().unwrap();
    for field in ["ordinal", "manifest", "source", "epoch", "clock", "sql"] {
        let mut changed = first.clone();
        match field {
            "ordinal" => changed.participant.parent.row_ordinal += 1,
            "manifest" => changed.participant.parent.manifest_sha256 = digest(b"another manifest"),
            "source" => changed.participant.source = "another native source".into(),
            "epoch" => changed.participant.epoch = "another source incarnation".into(),
            "clock" => changed.participant.original_observed_at = "2020-01-01T00:00:01Z".into(),
            _ => changed.participant.original.statement = DefinitionPayload::sql("SELECT 1", crate::Engine::Mssql),
        }
        assert!(changed.is_valid(), "structural validity cannot authenticate modified {field}");
        assert_ne!(changed.key().unwrap(), key, "exact parent resolution must bind {field}");
    }
}

#[test]
fn source_mismatch_or_disappeared_wait_never_authorizes_page_lookup() {
    let mut result = ActivityWaitFocusResult {
        focus: focus(),
        observed_from: "2020-01-01T00:00:01Z".into(),
        observed_until: "2020-01-01T00:00:02Z".into(),
        source: FocusSourceState::Matched,
        request: PageWaitRequestState::SameStatement,
        wait: PageWaitRelation::OriginalObserved,
    };
    assert!(result.permits_page_lookup());
    for wait in [PageWaitRelation::NoLongerObserved, PageWaitRelation::Different, PageWaitRelation::Unqualified] {
        result.wait = wait;
        assert!(result.is_valid());
        assert!(!result.permits_page_lookup());
    }
    for source in [FocusSourceState::Changed, FocusSourceState::Unverified] {
        result.source = source;
        result.request = PageWaitRequestState::Unverified;
        result.wait = PageWaitRelation::Unqualified;
        assert!(result.is_valid());
        assert!(!result.permits_page_lookup());
        result.request = PageWaitRequestState::SameStatement;
        assert!(!result.is_valid());
    }
}

#[test]
fn later_lookup_bounds_cannot_move_the_original_native_observation_forward() {
    let mut result = ActivityWaitFocusResult {
        focus: focus(),
        observed_from: "2019-12-31T23:59:59Z".into(),
        observed_until: "2020-01-01T00:00:01Z".into(),
        source: FocusSourceState::Matched,
        request: PageWaitRequestState::Missing,
        wait: PageWaitRelation::Unqualified,
    };
    assert!(!result.is_valid());
    result.observed_from = "2020-01-01T00:00:00Z".into();
    assert!(result.is_valid());
    assert_eq!(result.focus.participant().original_observed_at, "2020-01-01T00:00:00Z");
    result.observed_until = "2019-12-31T23:59:59Z".into();
    assert!(!result.is_valid());
}

#[test]
fn coordinator_focus_cannot_accept_a_parallel_task_with_the_same_request_and_page() {
    let mut focus = focus();
    focus.resource.native =
        focus.resource.native.clone().qualified("source", "sys.dm_os_waiting_tasks.resource_description");
    focus.participant_mut().original.wait.as_mut().unwrap().identified_resources = vec![focus.resource.clone()];
    let result = ActivityWaitFocusResult {
        focus,
        observed_from: "2020-01-01T00:00:01Z".into(),
        observed_until: "2020-01-01T00:00:02Z".into(),
        source: FocusSourceState::Matched,
        request: PageWaitRequestState::SameStatement,
        wait: PageWaitRelation::OriginalObserved,
    };
    let mut page = PageWaitObservation {
        source: "focused-page-fixture".into(),
        observed_from: result.observed_from.clone(),
        observed_until: result.observed_until.clone(),
        request: result.focus.participant().original.clone(),
        task_wait: None,
        before_check: None,
        after_check: None,
        statement_identity: None,
        plan_identity: None,
        resource: result.focus.resource.clone(),
        observed_address: None,
        resolution: PageWaitResolution::UnverifiedSource,
        target: None,
        index_operations: None,
    };
    assert!(page.is_valid());
    assert!(result.accepts_observation(&page));
    page.task_wait = Some(MssqlPageTaskWait {
        observed_at: page.observed_from.clone(),
        session_id: page.request.key.session.id.parse().unwrap(),
        request_id: page.request.key.request.as_ref().unwrap().id.parse().unwrap(),
        execution_context_id: 1,
        task_address: "0000000000000001".into(),
        wait_type: "PAGELATCH_EX".into(),
        wait_duration_ms: Some("1".into()),
        resource: page.resource.clone(),
        resource_address: None,
        blocking_task_address: None,
        blocking_session_id: Some(-5),
        blocking_execution_context_id: None,
    });
    assert!(page.is_valid(), "a structurally valid task still belongs to a different evidence population");
    assert!(!result.accepts_observation(&page));
}
