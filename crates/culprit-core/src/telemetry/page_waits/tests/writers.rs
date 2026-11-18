use super::*;

/// Structurally valid controlled shared authority; no actual native read or production qualification is claimed.
pub(in crate::telemetry::page_waits) fn shared_target(
    target: &MappedPageTarget,
    from: &str,
    until: &str,
) -> MappedPageTarget {
    let resource = SharedResourceIdentity {
        engine: Engine::Mssql,
        kind: SharedResourceKind::Instance,
        namespace: NativeIdentity::new("private-unrelated-shared-machine"),
        native: NativeIdentity::new("private-unrelated-shared-instance"),
    };
    let scope = PageTargetScope::SharedResource {
        context: Box::new(NativeReadContext {
            target: CollectionTarget::Shared(resource.clone()),
            partition: SourcePartition::InstanceLocal {
                member: Some(resource),
                epoch: Some("controlled-shared-native-epoch".into()),
            },
            observed_from: from.into(),
            observed_until: until.into(),
            database_writable: None,
        }),
    };
    let (database, file) = scope.bind_identities(target.database.clone(), target.file.clone()).unwrap();
    let target = MappedPageTarget { scope, database, file, ..target.clone() };
    assert!(target.is_valid(), "the counterexample retains genuine bound shared context");
    target
}

fn reference(key: NativeRecordIdentity, stream: TelemetryStream, ordinal: u32) -> DiagnosticEvidenceRef {
    let capture = CaptureIdentity {
        observation_id: "original-roster".into(),
        revision: Some(CaptureRevision {
            outbox_id: "writer-model-fixture".into(),
            owner_epoch: "1".into(),
            sequence: "1".into(),
        }),
    };
    DiagnosticEvidenceRef {
        batch_id: batch_id(&capture.observation_id, stream).unwrap(),
        capture,
        manifest_sha256: digest(b"original-manifest"),
        source_scope_id: digest(b"original-scope"),
        stream,
        native_key: key,
        record_id: digest(&ordinal.to_be_bytes()),
        payload_sha256: digest(b"original-payload"),
        representation_version: REPRESENTATION_VERSION,
        row_ordinal: ordinal,
    }
}

pub(in crate::telemetry::page_waits) fn fixture() -> (PageWaitWriterFocus, NativeReadContext, PageWaitWriterObservation)
{
    let mut page = observation();
    let target = page.target.as_mut().unwrap();
    let object = target.object.as_mut().unwrap();
    object.native = object
        .native
        .clone()
        .qualified("created_without_offset", "2020-01-01T00:00:00.0000000")
        .qualified("modified_without_offset", "2020-01-01T00:00:00.0000000")
        .qualified("index_type", "1")
        .qualified("index_id", "1");
    page.request.statement_identity = page.statement_identity.clone();
    let parent =
        reference(NativeRecordIdentity::Activity(page.request.key.clone()), TelemetryStream::ActivityObservations, 0);
    let partition = SourcePartition::InstanceLocal {
        member: Some(SharedResourceIdentity {
            engine: Engine::Mssql,
            kind: SharedResourceKind::Instance,
            namespace: NativeIdentity::new("native-machine"),
            native: NativeIdentity::new("native-member"),
        }),
        epoch: Some("native-epoch".into()),
    };
    let waiter = ActivityWaitFocus {
        participant: ActivityRequestFocus {
            parent: parent.clone(),
            target: crate::DatabaseIdentity::server(Engine::Mssql, "sample", "fixture.invalid", 1433, None).into(),
            partition: partition.clone(),
            database_writable: Some(true),
            source: crate::source_id::SESSIONS.into(),
            epoch: "native-epoch".into(),
            original_observed_at: "2026-11-13T09:59:59Z".into(),
            original: page.request.clone(),
            statement_identity: page.statement_identity.clone().unwrap(),
        },
        resource: page.resource.clone(),
    };
    let mut request = page.request.clone();
    request.key.session.id = "43".into();
    request.wait = None;
    request.statement = DefinitionPayload::sql("INSERT INTO dbo.orders VALUES (1)", Engine::Mssql);
    request.command = Some("INSERT".into());
    let candidate = PageWaitWriterRequest {
        parent: reference(
            NativeRecordIdentity::Activity(request.key.clone()),
            TelemetryStream::ActivityObservations,
            1,
        ),
        key: request.key.clone(),
        statement_identity: request.statement_identity.clone().unwrap(),
    };
    let focus = PageWaitWriterFocus {
        waiter,
        page: reference(
            NativeRecordIdentity::Operational { source: SOURCE.into(), key: page.key().unwrap() },
            TelemetryStream::OperationalSamples,
            0,
        ),
        address: page.observed_address.unwrap(),
        target: page.target.clone().unwrap(),
        candidates: vec![candidate.clone()],
    };
    let context = NativeReadContext {
        target: focus.waiter.participant.target.clone(),
        partition,
        observed_from: "2026-11-13T10:00:00Z".into(),
        observed_until: "2026-11-13T10:00:01Z".into(),
        database_writable: Some(true),
    };
    let mut writer = PageWaitWriterCandidate {
        parent: candidate.parent,
        request_state: PageWaitRequestState::SameStatement,
        request: Some(request),
        plan_identity: Some(NativeIdentity::new("native-plan")),
        plan: DefinitionPayload::plan("<ShowPlanXML/>", Engine::Mssql),
        relation: PageWriterRelation::TargetObjectWrite,
        binding: None,
    };
    assert!(focus.is_valid(), "original selectors require complete durable receipts");
    writer.binding = Some(
        MssqlPageWritePlanProof::new(
            &focus.target,
            &writer,
            digest(b"native-plan"),
            digest(b"native-statement"),
            vec![MssqlPageWriteAccess { node_id: 7, operation: PageWriteOperation::Insert }],
        )
        .unwrap(),
    );
    let row = PageWaitWriterObservation {
        observed_from: context.observed_from.clone(),
        observed_until: context.observed_until.clone(),
        focus_key: focus.key().unwrap(),
        target_state: PageWriterTargetState::CurrentMapped,
        page: Some(Box::new(page)),
        candidates: vec![writer],
    };
    assert!(focus.is_valid());
    assert!(focus.accepts_observation(&context, &row));
    (focus, context, row)
}

#[test]
fn selected_object_writers_keep_original_receipts_and_independent_unknown_latch_owner() {
    let (focus, context, row) = fixture();
    let roundtrip: PageWaitWriterObservation = serde_json::from_slice(&serde_json::to_vec(&row).unwrap()).unwrap();
    assert!(focus.accepts_observation(&context, &roundtrip));
    assert_ne!(row.candidates[0].request.as_ref().unwrap().key, row.page.as_ref().unwrap().request.key);
    assert!(matches!(
        row.page.as_ref().unwrap().request.wait.as_ref().unwrap().owners.as_slice(),
        [ActivityWaitOwner::Unknown { reason: UnknownWaitOwner::UntrackedLatch, .. }]
    ));
    assert_eq!(row.candidates[0].binding.as_ref().unwrap().accesses[0].operation, PageWriteOperation::Insert);
}

#[test]
fn writer_session_authority_requires_both_waiter_and_every_candidate_generation() {
    use crate::telemetry::activity_frame::tests as frames;

    for selected in [0, 1] {
        for lifecycle in [SessionLifecycle::Unknown, SessionLifecycle::Connecting, SessionLifecycle::PoolResetPrelogin]
        {
            let (mut focus, _, observation) = fixture();
            let mut rows =
                vec![focus.waiter.participant.original.clone(), observation.candidates[0].request.clone().unwrap()];
            let mut peer = rows[selected].clone();
            peer.key.request.as_mut().unwrap().id = "1".into();
            peer.session_context = Some(Box::new(ActivitySessionContext {
                lifecycle,
                login_age_ms: None,
                last_request_end_age_ms: None,
                client_library: None,
                client_process_id: None,
                native: None,
            }));
            rows.push(peer);
            let original = &focus.waiter.participant;
            let mut source = frames::source(rows.len());
            source.target = original.target.clone();
            source.partition = original.partition.clone();
            source.source = original.source.clone();
            let SourceObservation::Activity(native) = &mut source.observation else { panic!("Activity") };
            native.epoch = Some(original.epoch.clone());
            native.database_writable = original.database_writable;
            native.at = original.original_observed_at.clone();
            native.observed_from = native.at.clone();
            native.observed_until = native.at.clone();
            native.coverage = StateSourceCoverage::collected(source.source.clone(), rows.len() as u32);
            let refs = frames::references(&source, &rows);
            focus.waiter.participant.parent = refs[0].clone();
            focus.candidates[0].parent = refs[1].clone();
            let borrowed = frames::borrowed(&rows, &refs);
            let frame = ActivityFrameEvidence::new(&source, &borrowed, true).unwrap();
            assert!(focus.is_valid());
            assert!(focus.waiter.participant.matches_parent(&source, &rows[0], &refs[0]));
            assert_eq!(frame.rows().count(), 3, "the conflicting canonical peer is never discarded");
            assert_eq!(focus.matches_activity_frame(&frame), lifecycle == SessionLifecycle::Unknown);
        }
    }
}

#[test]
fn writer_scope_request_and_native_binding_tampering_cannot_borrow_original_authority() {
    let (focus, context, row) = fixture();
    for field in ["member", "epoch", "role", "database"] {
        let mut changed = context.clone();
        match field {
            "member" => {
                if let SourcePartition::InstanceLocal { member: Some(member), .. } = &mut changed.partition {
                    member.native.id = "other-member".into()
                }
            }
            "epoch" => {
                if let SourcePartition::InstanceLocal { epoch, .. } = &mut changed.partition {
                    *epoch = Some("other-epoch".into())
                }
            }
            "role" => changed.database_writable = Some(false),
            _ => {
                changed.target =
                    crate::DatabaseIdentity::server(Engine::Mssql, "other", "fixture.invalid", 1433, None).into()
            }
        }
        assert!(!focus.accepts_observation(&changed, &row), "{field}");
    }
    for field in ["generation", "statement", "parent", "native-plan", "node", "payload", "target", "wait-gone"] {
        let mut changed = row.clone();
        match field {
            "generation" => {
                changed.candidates[0].request.as_mut().unwrap().key.request.as_mut().unwrap().generation =
                    "reused".into()
            }
            "statement" => {
                changed.candidates[0].request.as_mut().unwrap().statement_identity.as_mut().unwrap().id =
                    "different-statement".into()
            }
            "parent" => changed.candidates[0].parent.record_id = digest(b"other-parent"),
            "native-plan" => {
                changed.candidates[0].binding.as_mut().unwrap().native_plan_sha256 = digest(b"other-native-plan")
            }
            "node" => changed.candidates[0].binding.as_mut().unwrap().accesses[0].node_id = 8,
            "payload" => changed.candidates[0].plan = DefinitionPayload::plan("<other/>", Engine::Mssql),
            "target" => changed
                .page
                .as_mut()
                .unwrap()
                .target
                .as_mut()
                .unwrap()
                .object
                .as_mut()
                .unwrap()
                .native
                .qualifiers
                .insert("modified_without_offset".into(), "2021-01-01T00:00:00.0000000".into())
                .map(|_| ())
                .unwrap_or(()),
            _ => changed.page.as_mut().unwrap().after_check.as_mut().unwrap().wait = PageWaitRelation::NoLongerObserved,
        }
        assert!(!focus.accepts_observation(&context, &changed), "{field}");
    }
}

#[test]
fn mutable_page_contents_are_not_incarnation_but_allocation_io_and_shared_scopes_are_refused() {
    let (focus, context, _) = fixture();
    let expected = focus.target.writer_target_key().unwrap();
    let mut target = *focus.target.clone();
    target.header.lsn = Some("00000002:00000030:0002".into());
    target.header.next_file_id = Some(1);
    target.header.next_page_id = Some(43);
    assert_eq!(target.writer_target_key().unwrap(), expected);
    for kind in [
        PageWaitTargetKind::Pfs,
        PageWaitTargetKind::Gam,
        PageWaitTargetKind::Sgam,
        PageWaitTargetKind::Iam,
        PageWaitTargetKind::Unclassified,
    ] {
        target.kind = kind;
        assert!(target.writer_target_key().is_err(), "{kind:?}");
    }
    let mut io = focus.clone();
    io.waiter.resource.kind = WaitResourceKind::PageIo;
    assert!(!io.is_valid());
    let mut shared = focus;
    let mut resource_context = context;
    if let SourcePartition::InstanceLocal { member: Some(member), .. } = &resource_context.partition {
        resource_context.target = CollectionTarget::Shared(member.clone());
    }
    shared.target.scope = PageTargetScope::SharedResource { context: Box::new(resource_context) };
    assert!(!shared.is_valid());
}

#[test]
fn unavailable_payload_and_changed_targets_withhold_writer_verdicts_without_inventing_absence() {
    let (focus, context, mut row) = fixture();
    row.candidates[0].plan = DefinitionPayload::Unavailable { reason: PayloadUnavailable::Missing };
    assert!(!focus.accepts_observation(&context, &row));
    row.candidates[0].binding = None;
    row.candidates[0].plan_identity = None;
    row.candidates[0].relation = PageWriterRelation::Unavailable;
    assert!(focus.accepts_observation(&context, &row));
    row.target_state = PageWriterTargetState::ChangedTarget;
    row.page = None;
    assert!(focus.accepts_observation(&context, &row));
    row.candidates[0].relation = PageWriterRelation::ReadOnlyPlan;
    assert!(!focus.accepts_observation(&context, &row), "missing payload is not a read-only plan");
}

fn with_target_state(mut row: PageWaitWriterObservation, state: PageWriterTargetState) -> PageWaitWriterObservation {
    row.target_state = state;
    if state != PageWriterTargetState::CurrentMapped {
        for candidate in &mut row.candidates {
            candidate.relation = PageWriterRelation::Unavailable;
            candidate.binding = None;
            candidate.plan_identity = None;
            candidate.plan = DefinitionPayload::Unavailable { reason: PayloadUnavailable::SourceUnavailable };
        }
    }
    row
}

#[test]
fn every_writer_target_state_checks_the_retained_requests_database_without_inventing_unknown_labels() {
    let (focus, context, original) = fixture();
    for state in [
        PageWriterTargetState::CurrentMapped,
        PageWriterTargetState::NoLongerObserved,
        PageWriterTargetState::ChangedTarget,
        PageWriterTargetState::Unavailable,
    ] {
        let mut row = with_target_state(original.clone(), state);
        assert!(focus.accepts_observation(&context, &row), "{state:?}");
        row.page.as_mut().unwrap().request.database = None;
        assert!(focus.accepts_observation(&context, &row), "missing labels remain unknown: {state:?}");
        row.page.as_mut().unwrap().request.database = Some("foreign-database".into());
        assert!(row.is_valid(), "the point is structurally valid: {state:?}");
        assert!(!focus.accepts_observation(&context, &row), "{state:?}");
    }
}

#[test]
fn no_writer_target_state_can_borrow_a_valid_shared_mapping() {
    let (focus, context, original) = fixture();
    for state in [
        PageWriterTargetState::CurrentMapped,
        PageWriterTargetState::NoLongerObserved,
        PageWriterTargetState::ChangedTarget,
        PageWriterTargetState::Unavailable,
    ] {
        let mut row = with_target_state(original.clone(), state);
        assert!(focus.accepts_observation(&context, &row), "{state:?}");
        let page = row.page.as_mut().unwrap();
        page.target =
            Some(Box::new(shared_target(page.target.as_ref().unwrap(), &page.observed_from, &page.observed_until)));
        assert!(page.is_valid(), "the shared page is independently valid: {state:?}");
        assert!(row.is_valid(), "raw native fields alone do not establish the database lookup scope: {state:?}");
        assert!(!focus.accepts_observation(&context, &row), "{state:?}");
    }
}

#[test]
fn a_shared_page_keeps_its_independent_capture_scope_after_database_lookup_is_refused() {
    let (_, _, row) = fixture();
    let mut page = *row.page.unwrap();
    page.target =
        Some(Box::new(shared_target(page.target.as_ref().unwrap(), &page.observed_from, &page.observed_until)));
    let PageTargetScope::SharedResource { context } = &page.target.as_ref().unwrap().scope else { panic!() };
    let source = CollectionDescription {
        target: context.target.clone(),
        partition: context.partition.clone(),
        source: "independent-shared-page-recipe".into(),
        continuity: SourceContinuity::Unverified,
        observation: SourceObservation::PageWaitTargets {
            snapshot: StateSnapshotDescription {
                dataset: SnapshotDataset::PageWaitTargets,
                observed_from: context.observed_from.clone(),
                observed_until: context.observed_until.clone(),
                records: 1,
                sources: vec![StateSourceCoverage::observed(page.source.clone(), 1, false)],
            },
            database_writable: None,
            original_activity: None,
        },
    };
    assert!(page.fits_database_capture("sample"));
    assert!(!page.fits_database_lookup("sample"));
    assert!(page.fits_source_scope(&source));
    let record = TelemetryRecord::Operational(OperationalObservation {
        source: page.source.clone(),
        key: page.key().unwrap(),
        fact: OperationalFact::PageWait(Box::new(page)),
    });
    let capture = CaptureIdentity { observation_id: "independently-qualified-shared-page".into(), revision: None };
    assert!(ExportedRecord::new(&source, &capture, record).is_ok());
}

#[test]
fn proof_node_bound_and_exact_coverage_do_not_silently_drop_writers() {
    let (focus, context, row) = fixture();
    let accesses = (0..MAX_PAGE_WAIT_WRITE_ACCESSES as u32)
        .map(|node_id| MssqlPageWriteAccess { node_id, operation: PageWriteOperation::Update })
        .collect::<Vec<_>>();
    assert!(
        MssqlPageWritePlanProof::new(
            &focus.target,
            &row.candidates[0],
            digest(b"plan"),
            digest(b"statement"),
            accesses.clone()
        )
        .is_ok()
    );
    let mut overflow = accesses;
    overflow.push(MssqlPageWriteAccess {
        node_id: MAX_PAGE_WAIT_WRITE_ACCESSES as u32,
        operation: PageWriteOperation::Delete,
    });
    assert!(
        MssqlPageWritePlanProof::new(
            &focus.target,
            &row.candidates[0],
            digest(b"plan"),
            digest(b"statement"),
            overflow
        )
        .is_err()
    );
    let mut missing = row;
    missing.candidates.clear();
    assert!(!focus.accepts_observation(&context, &missing));
}

#[test]
fn a_parallel_task_page_cannot_be_reconstructed_as_a_coordinator_writer_question() {
    let (mut focus, context, row) = fixture();
    let mut page = *row.page.unwrap();
    page.resource.native.qualifiers.insert("source".into(), "sys.dm_os_waiting_tasks.resource_description".into());
    page.request.wait.as_mut().unwrap().identified_resources = vec![page.resource.clone()];
    focus.waiter.resource = page.resource.clone();
    focus.waiter.participant.original = page.request.clone();
    let source = CollectionDescription {
        target: context.target,
        partition: context.partition,
        source: page.source.clone(),
        continuity: SourceContinuity::Unverified,
        observation: SourceObservation::PageWaitTargets {
            snapshot: StateSnapshotDescription {
                dataset: SnapshotDataset::PageWaitTargets,
                observed_from: page.observed_from.clone(),
                observed_until: page.observed_until.clone(),
                records: 1,
                sources: vec![StateSourceCoverage::observed(page.source.clone(), 1, false)],
            },
            database_writable: context.database_writable,
            original_activity: None,
        },
    };
    focus.page.source_scope_id = source.captured_range().unwrap().scope_id;
    focus.page.native_key = NativeRecordIdentity::Operational { source: page.source.clone(), key: page.key().unwrap() };
    assert!(focus.matches_page(&source, &page, &focus.page));
    page.task_wait = Some(MssqlPageTaskWait {
        observed_at: page.observed_from.clone(),
        session_id: 42,
        request_id: 0,
        execution_context_id: 3,
        task_address: "00000000ABCDEF01".into(),
        wait_type: "PAGELATCH_EX".into(),
        wait_duration_ms: Some("1".into()),
        resource: page.resource.clone(),
        resource_address: None,
        blocking_task_address: None,
        blocking_session_id: Some(-5),
        blocking_execution_context_id: None,
    });
    assert!(page.is_valid());
    focus.page.native_key = NativeRecordIdentity::Operational { source: page.source.clone(), key: page.key().unwrap() };
    assert!(
        !focus.matches_page(&source, &page, &focus.page),
        "an independently identified parallel task is not the original coordinator question"
    );
}

#[test]
fn unknown_index_names_and_contradictory_native_catalog_ids_cannot_authorize_writer_binding() {
    let (focus, _, _) = fixture();
    for field in ["name", "type", "index-id", "missing-index-id"] {
        let mut target = *focus.target.clone();
        let object = target.object.as_mut().unwrap();
        match field {
            "name" => object.index_name = None,
            "type" => {
                object.native.qualifiers.insert("index_type".into(), "0".into());
            }
            "index-id" => {
                object.native.qualifiers.insert("index_id".into(), "2".into());
            }
            _ => {
                object.native.qualifiers.remove("index_id");
            }
        }
        assert!(target.writer_target_key().is_err(), "{field}");
    }
    let mut heap = *focus.target;
    heap.kind = PageWaitTargetKind::HeapData;
    heap.header.page_type = Some(1);
    heap.header.index_id = Some("0".into());
    let object = heap.object.as_mut().unwrap();
    object.index_name = None;
    object.native.qualifiers.insert("index_type".into(), "0".into());
    object.native.qualifiers.insert("index_id".into(), "0".into());
    assert!(heap.writer_target_key().is_ok(), "an observed heap0 has no index name by native definition");
    heap.object.as_mut().unwrap().index_name = Some("unexpected-index".into());
    assert!(heap.writer_target_key().is_err());
}
