use super::*;
mod insert_hotspot;

fn reference(key: NativeRecordIdentity, stream: TelemetryStream, ordinal: u32) -> DiagnosticEvidenceRef {
    let capture = CaptureIdentity {
        observation_id: "original-writer-frame".into(),
        revision: Some(CaptureRevision {
            outbox_id: "writer-view-fixture".into(),
            owner_epoch: "1".into(),
            sequence: "1".into(),
        }),
    };
    DiagnosticEvidenceRef {
        batch_id: batch_id(&capture.observation_id, stream).unwrap(),
        capture,
        manifest_sha256: digest(b"manifest"),
        source_scope_id: digest(b"source-scope"),
        stream,
        native_key: key,
        record_id: digest(&ordinal.to_be_bytes()),
        payload_sha256: digest(b"payload"),
        representation_version: REPRESENTATION_VERSION,
        row_ordinal: ordinal,
    }
}
pub(super) fn fixture() -> (PageWaitWriterFocus, NativeReadContext, PageWaitWriterObservation) {
    let mut page = observation("61", WaitResourceKind::MemoryPage);
    page.request.statement_identity = page.statement_identity.clone();
    let object = page.target.as_mut().unwrap().object.as_mut().unwrap();
    object.native = object
        .native
        .clone()
        .qualified("created_without_offset", "2020-01-01T00:00:00.0000000")
        .qualified("modified_without_offset", "2020-01-01T00:00:00.0000000")
        .qualified("index_type", "1")
        .qualified("index_id", "1");
    let partition = SourcePartition::InstanceLocal {
        member: Some(SharedResourceIdentity {
            engine: Engine::Mssql,
            kind: SharedResourceKind::Instance,
            namespace: NativeIdentity::new("native-machine"),
            native: NativeIdentity::new("native-member"),
        }),
        epoch: Some("native-epoch".into()),
    };
    let participant = ActivityRequestFocus {
        parent: reference(
            NativeRecordIdentity::Activity(page.request.key.clone()),
            TelemetryStream::ActivityObservations,
            0,
        ),
        target: culprit_core::DatabaseIdentity::server(Engine::Mssql, "shop", "fixture.invalid", 1433, None).into(),
        partition: partition.clone(),
        database_writable: Some(true),
        source: culprit_core::source_id::SESSIONS.into(),
        epoch: "native-epoch".into(),
        original_observed_at: "2026-11-13T09:59:59Z".into(),
        original: page.request.clone(),
        statement_identity: page.statement_identity.clone().unwrap(),
    };
    let mut request = page.request.clone();
    request.key.session.id = "62".into();
    request.wait = None;
    request.command = Some("INSERT".into());
    request.statement =
        DefinitionPayload::sql("INSERT INTO app.orders(payload) VALUES ('private-writer-value')", Engine::Mssql);
    let selected = PageWaitWriterRequest {
        parent: reference(
            NativeRecordIdentity::Activity(request.key.clone()),
            TelemetryStream::ActivityObservations,
            1,
        ),
        key: request.key.clone(),
        statement_identity: request.statement_identity.clone().unwrap(),
    };
    let focus = PageWaitWriterFocus {
        waiter: ActivityWaitFocus { participant, resource: page.resource.clone() },
        page: reference(
            NativeRecordIdentity::Operational { source: page.source.clone(), key: page.key().unwrap() },
            TelemetryStream::OperationalSamples,
            0,
        ),
        address: page.observed_address.unwrap(),
        target: page.target.clone().unwrap(),
        candidates: vec![selected.clone()],
    };
    let context = NativeReadContext {
        target: focus.waiter.participant().target.clone(),
        partition,
        observed_from: "2026-11-13T10:00:00Z".into(),
        observed_until: "2026-11-13T10:00:01Z".into(),
        database_writable: Some(true),
    };
    let mut candidate = PageWaitWriterCandidate {
        parent: selected.parent,
        request_state: PageWaitRequestState::SameStatement,
        request: Some(request),
        plan_identity: Some(NativeIdentity::new("selected-plan")),
        plan: DefinitionPayload::plan("<ShowPlanXML/>", Engine::Mssql),
        relation: PageWriterRelation::TargetObjectWrite,
        binding: None,
    };
    candidate.binding = Some(
        MssqlPageWritePlanProof::new(
            &focus.target,
            &candidate,
            digest(b"raw-native-plan"),
            digest(b"raw-native-statement"),
            vec![MssqlPageWriteAccess { node_id: 1, operation: PageWriteOperation::Insert }],
        )
        .unwrap(),
    );
    let row = PageWaitWriterObservation {
        observed_from: context.observed_from.clone(),
        observed_until: context.observed_until.clone(),
        focus_key: focus.key().unwrap(),
        target_state: PageWriterTargetState::CurrentMapped,
        page: Some(Box::new(page)),
        candidates: vec![candidate],
    };
    assert!(focus.accepts_observation(&context, &row));
    (focus, context, row)
}

fn add_candidate(
    focus: &mut PageWaitWriterFocus,
    row: &mut PageWaitWriterObservation,
    session: &str,
    request_id: &str,
    sql: &str,
    relation: PageWriterRelation,
) {
    let mut original = focus.candidates[0].clone();
    original.key.session.id = session.into();
    original.key.request.as_mut().unwrap().id = request_id.into();
    original.statement_identity = NativeIdentity::new(format!("statement-{session}-{request_id}"));
    original.parent.native_key = NativeRecordIdentity::Activity(original.key.clone());
    original.parent.record_id = digest(format!("request-{session}-{request_id}").as_bytes());
    original.parent.row_ordinal = focus.candidates.len() as u32 + 1;
    let mut current = row.candidates[0].clone();
    current.parent = original.parent.clone();
    current.plan_identity = Some(NativeIdentity::new(format!("plan-{session}-{request_id}")));
    current.relation = relation;
    current.binding = None;
    let request = current.request.as_mut().unwrap();
    request.key = original.key.clone();
    request.statement_identity = Some(original.statement_identity.clone());
    request.statement = DefinitionPayload::sql(sql, Engine::Mssql);
    request.client.as_mut().unwrap().application = Some(format!("application-{session}-{request_id}"));
    if relation == PageWriterRelation::TargetObjectWrite {
        current.binding = Some(
            MssqlPageWritePlanProof::new(
                &focus.target,
                &current,
                digest(b"another-native-plan"),
                digest(sql.as_bytes()),
                vec![MssqlPageWriteAccess { node_id: 2, operation: PageWriteOperation::Insert }],
            )
            .unwrap(),
        );
    }
    focus.candidates.push(original);
    row.candidates.push(current);
    row.focus_key = focus.key().unwrap();
}

fn withdraw_generation(candidate: &mut PageWaitWriterCandidate) {
    candidate.request.as_mut().unwrap().session_context = Some(Box::new(ActivitySessionContext {
        lifecycle: SessionLifecycle::PoolResetPrelogin,
        login_age_ms: None,
        last_request_end_age_ms: None,
        client_library: None,
        client_process_id: None,
        native: None,
    }));
}

#[test]
fn target_writing_plan_is_actionable_context_without_latch_owner_or_cost_attribution() {
    let (focus, context, row) = fixture();
    let report = explain_writers(&focus, &context, Some(&row), Some(&source(SourceState::Partial))).unwrap();
    assert_eq!(report.entries[0].relation, PageWriterRelation::TargetObjectWrite);
    assert_eq!(report.entries[0].parent, focus.candidates[0].parent);
    assert_eq!(report.entries[0].request, focus.candidates[0].key);
    assert_eq!(report.original_target, focus.target);
    assert_eq!(report.entries[0].sql.as_ref(), row.candidates[0].request.as_ref().map(|request| &request.statement));
    assert_eq!(report.entries[0].client, row.candidates[0].request.as_ref().unwrap().client);
    assert_eq!(report.entries[0].explanation, t!("view.page_writers.relation.target"));
    assert_eq!(report.entries[0].investigation, t!("view.page_writers.investigate.target"));
    for limitation in [
        t!("view.page_writers.limit.roster"),
        t!("view.page_writers.limit.access"),
        t!("view.page_writers.limit.ownership"),
        t!("view.page_writers.limit.execution"),
    ] {
        assert!(report.limitations.contains(&limitation));
    }
    let public = serde_json::to_string(&report).unwrap();
    assert!(public.contains("INSERT INTO"), "the guarded query pattern is visible");
    assert!(public.contains("private-app-label"), "native client context remains labelled source evidence");
    for private in ["private-writer-value", "ShowPlanXML"] {
        assert!(!public.contains(private), "{private}");
    }
}

#[test]
fn archived_missing_row_retains_question_and_partial_coverage_without_a_native_zero() {
    let (focus, context, _) = fixture();
    let report = explain_writers(&focus, &context, None, Some(&source(SourceState::Collected))).unwrap();
    assert!(report.entries.is_empty());
    assert_eq!(report.source_state, Some(SourceState::Partial));
    assert!(report.target_state.is_none());
    assert_eq!(report.original_page, focus.page);
    assert_eq!(report.original_target, focus.target);
    assert_eq!(report.summary, t!("view.page_writers.summary.unavailable"));
    assert!(report.limitations.contains(&t!("view.page_writers.limit.read")));
}

#[test]
fn unavailable_plan_or_contemporary_maintenance_does_not_become_a_proven_writer() {
    let (focus, context, mut row) = fixture();
    let candidate = &mut row.candidates[0];
    candidate.binding = None;
    candidate.plan_identity = None;
    candidate.plan = DefinitionPayload::Unavailable { reason: PayloadUnavailable::Missing };
    candidate.relation = PageWriterRelation::Unavailable;
    let request = candidate.request.as_mut().unwrap();
    request.command = Some("ALTER INDEX".into());
    request.statement = DefinitionPayload::sql("ALTER INDEX ix_orders ON app.orders REBUILD", Engine::Mssql);
    let report = explain_writers(&focus, &context, Some(&row), Some(&source(SourceState::Partial))).unwrap();
    assert_eq!(report.entries[0].relation, PageWriterRelation::Unavailable);
    assert!(matches!(report.entries[0].maintenance, culprit_core::MaintenanceClassification::Maintenance { .. }));
    assert!(report.entries[0].explanation.contains(&t!("view.page_writers.maintenance")));
    assert_eq!(report.entries[0].investigation, t!("view.page_writers.investigate.unavailable"));
}

#[test]
fn wrong_source_changed_request_and_injected_payload_are_rejected() {
    let (focus, context, row) = fixture();
    let mut wrong = source(SourceState::Partial);
    wrong.source = "unrelated native source".into();
    assert!(explain_writers(&focus, &context, Some(&row), Some(&wrong)).is_err());
    let mut changed = row.clone();
    changed.candidates[0].request.as_mut().unwrap().key.request.as_mut().unwrap().generation = "next-request".into();
    assert!(explain_writers(&focus, &context, Some(&changed), Some(&source(SourceState::Partial))).is_err());
    let mut changed = row;
    changed.candidates[0].plan = DefinitionPayload::plan("<different/>", Engine::Mssql);
    assert!(explain_writers(&focus, &context, Some(&changed), Some(&source(SourceState::Partial))).is_err());
}

#[test]
fn distinct_write_patterns_and_an_unrelated_reader_keep_their_own_qualified_query_and_target_relationships() {
    let (mut focus, context, mut row) = fixture();
    add_candidate(
        &mut focus,
        &mut row,
        "63",
        "0",
        "INSERT INTO app.orders(payload, customer) VALUES ('private-second-write', 9264)",
        PageWriterRelation::TargetObjectWrite,
    );
    add_candidate(
        &mut focus,
        &mut row,
        "64",
        "0",
        "SELECT payload FROM app.audit WHERE customer=2718",
        PageWriterRelation::ReadOnlyPlan,
    );
    let before = row.clone();
    let report = explain_writers(&focus, &context, Some(&row), Some(&source(SourceState::Partial))).unwrap();
    assert_eq!(report.entries.len(), 3);
    assert_eq!(report.original_target, focus.target);
    assert_eq!(report.original_target.header.page_id, 4144);
    assert_eq!(report.original_target.object.as_ref().unwrap().index_name.as_deref(), Some("ix_orders"));
    for (entry, current) in report.entries.iter().zip(&row.candidates) {
        let request = current.request.as_ref().unwrap();
        assert_eq!(entry.request, request.key);
        assert_eq!(entry.statement, *request.statement_identity.as_ref().unwrap());
        assert_eq!(entry.sql.as_ref(), Some(&request.statement));
        assert_eq!(entry.client, request.client);
        assert_eq!(entry.relation, current.relation);
        assert_eq!(entry.plan_identity, current.plan_identity);
    }
    assert_ne!(report.entries[0].sql, report.entries[1].sql);
    assert_eq!(report.entries[0].accesses[0].node_id, 1);
    assert_eq!(report.entries[1].accesses[0].node_id, 2);
    assert!(report.entries[2].accesses.is_empty());
    assert_eq!(report.entries[2].explanation, t!("view.page_writers.relation.read_only"));
    assert_eq!(report.entries[2].investigation, t!("view.page_writers.investigate.other"));
    assert!(report.limitations.contains(&t!("view.page_writers.limit.ownership")));
    let public = serde_json::to_string(&report).unwrap();
    for value in ["private-second-write", "9264", "2718", "ShowPlanXML"] {
        assert!(!public.contains(value), "{value}");
    }
    assert_eq!(row, before, "presentation cannot rewrite original query or plan evidence");
}

#[test]
fn a_negative_mars_peer_withdraws_query_and_operator_links_for_only_its_exact_session_generation() {
    for peer_session in ["62", "63"] {
        let (mut focus, context, mut row) = fixture();
        add_candidate(
            &mut focus,
            &mut row,
            peer_session,
            "1",
            "SELECT payload FROM app.audit",
            PageWriterRelation::ReadOnlyPlan,
        );
        withdraw_generation(&mut row.candidates[1]);
        let original = row.clone();
        let report = explain_writers(&focus, &context, Some(&row), Some(&source(SourceState::Partial))).unwrap();
        assert!(report.entries[1].sql.is_none());
        assert!(report.entries[1].client.is_none());
        let entry = &report.entries[0];
        if peer_session == "62" {
            assert_eq!(entry.request_state, PageWaitRequestState::Unverified);
            assert_eq!(entry.relation, PageWriterRelation::Unavailable);
            assert!(entry.sql.is_none() && entry.client.is_none() && entry.plan_identity.is_none());
            assert!(entry.accesses.is_empty());
            assert_eq!(entry.explanation, t!("view.page_writers.request.unverified"));
            assert_eq!(entry.investigation, t!("view.page_writers.investigate.unavailable"));
        } else {
            assert_eq!(entry.request_state, PageWaitRequestState::SameStatement);
            assert_eq!(entry.relation, PageWriterRelation::TargetObjectWrite);
            assert!(entry.sql.is_some() && entry.client.is_some() && entry.plan_identity.is_some());
        }
        assert_eq!(row, original, "the full supplied population remains intact");
    }
}

#[test]
fn missing_requests_and_native_sql_unavailability_remain_distinct_for_partial_captures() {
    for reason in [PayloadUnavailable::Restricted, PayloadUnavailable::Oversized, PayloadUnavailable::SourceUnavailable]
    {
        let (focus, context, mut row) = fixture();
        let candidate = &mut row.candidates[0];
        candidate.binding = None;
        candidate.plan_identity = None;
        candidate.plan = DefinitionPayload::Unavailable { reason };
        candidate.relation = PageWriterRelation::Unavailable;
        candidate.request.as_mut().unwrap().statement = DefinitionPayload::Unavailable { reason };
        let report = explain_writers(&focus, &context, Some(&row), Some(&source(SourceState::Partial))).unwrap();
        assert_eq!(report.entries[0].sql, Some(DefinitionPayload::Unavailable { reason }));
        assert!(report.entries[0].client.is_some());
        assert!(report.limitations.contains(&t!("view.page_writers.limit.partial")));
        row.candidates[0].request_state = PageWaitRequestState::Missing;
        row.candidates[0].request = None;
        let missing = explain_writers(&focus, &context, Some(&row), Some(&source(SourceState::Partial))).unwrap();
        assert!(missing.entries[0].sql.is_none() && missing.entries[0].client.is_none());
        assert_eq!(missing.entries[0].request_state, PageWaitRequestState::Missing);
        assert_eq!(missing.original_target, focus.target);
    }
}

#[test]
fn changed_page_mapping_keeps_the_original_target_without_attaching_current_write_access_to_it() {
    let (focus, context, mut row) = fixture();
    let mut reused = row.clone();
    reused
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
        .insert("modified_without_offset".into(), "2020-01-02T00:00:00.0000000".into());
    assert!(explain_writers(&focus, &context, Some(&reused), Some(&source(SourceState::Partial))).is_err());
    row.target_state = PageWriterTargetState::ChangedTarget;
    row.page = None;
    row.candidates[0].relation = PageWriterRelation::Unavailable;
    row.candidates[0].binding = None;
    let report = explain_writers(&focus, &context, Some(&row), Some(&source(SourceState::Partial))).unwrap();
    assert_eq!(report.original_target, focus.target);
    assert_eq!(report.target_state, Some(PageWriterTargetState::ChangedTarget));
    assert_eq!(report.entries[0].relation, PageWriterRelation::Unavailable);
    assert!(report.entries[0].accesses.is_empty());
    assert!(report.entries[0].sql.is_some(), "independent qualified query context remains historical evidence");
    assert!(report.summary.starts_with(&t!("view.page_writers.target.changed")));
}

#[test]
fn guarded_sql_is_redacted_for_presentation_without_rewriting_its_native_proof() {
    let (focus, context, mut row) = fixture();
    let text = "INSERT INTO app.orders(payload) VALUES ('private-unredacted-writer-value')";
    let candidate = &mut row.candidates[0];
    candidate.request.as_mut().unwrap().statement = DefinitionPayload::Available {
        text: text.into(),
        sha256: digest(text.as_bytes()),
        redaction_version: REDACTION_VERSION,
    };
    candidate.binding = Some(
        MssqlPageWritePlanProof::new(
            &focus.target,
            candidate,
            digest(b"raw-native-plan"),
            digest(text.as_bytes()),
            vec![MssqlPageWriteAccess { node_id: 1, operation: PageWriteOperation::Insert }],
        )
        .unwrap(),
    );
    let original = row.clone();
    let report = explain_writers(&focus, &context, Some(&row), Some(&source(SourceState::Partial))).unwrap();
    assert_eq!(report.entries[0].sql, Some(DefinitionPayload::sql(text, Engine::Mssql)));
    assert!(!serde_json::to_string(&report).unwrap().contains("private-unredacted-writer-value"));
    assert_eq!(row, original);
}
