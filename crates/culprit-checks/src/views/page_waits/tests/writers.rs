use super::*;

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
fn fixture() -> (PageWaitWriterFocus, NativeReadContext, PageWaitWriterObservation) {
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

#[test]
fn target_writing_plan_is_actionable_context_without_latch_owner_or_cost_attribution() {
    let (focus, context, row) = fixture();
    let report = explain_writers(&focus, &context, Some(&row), Some(&source(SourceState::Partial))).unwrap();
    assert_eq!(report.entries[0].relation, PageWriterRelation::TargetObjectWrite);
    assert_eq!(report.entries[0].parent, focus.candidates[0].parent);
    assert_eq!(report.entries[0].request, focus.candidates[0].key);
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
    for private in ["INSERT INTO", "private-writer-value", "private-app-label", "9007199254740993", "ShowPlanXML"] {
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
