use super::*;

struct OriginalFrame {
    source: CollectionDescription,
    rows: Vec<ActivityObservation>,
    references: Vec<DiagnosticEvidenceRef>,
    original: ActivityRequestFocus,
}

impl OriginalFrame {
    fn new(page: &mut PageWaitObservation) -> Self {
        page.request.statement_identity = page.statement_identity.clone();
        page.request.wait.as_mut().unwrap().owners =
            vec![ActivityWaitOwner::Session { id: "57".into(), generation: Some("original-login".into()) }];
        let waiter = page.request.clone();
        let owners = (0..2).map(|index| {
            let mut owner = waiter.clone();
            owner.key.session = NativeActivityIdentity { id: "57".into(), generation: "original-login".into() };
            owner.key.request.as_mut().unwrap().id = index.to_string();
            owner.state = "running".into();
            owner.command = Some(if index == 0 { "INSERT" } else { "SELECT" }.into());
            owner.wait = None;
            owner.statement_identity = Some(NativeIdentity::new(format!("original-owner-statement-{index}")));
            owner.statement = DefinitionPayload::sql("private owner SQL WHERE secret=8821", Engine::Mssql);
            owner.request_counters.as_mut().unwrap().cpu_ms = Some("9007199254740993".into());
            owner
        });
        let rows = std::iter::once(waiter.clone()).chain(owners).collect::<Vec<_>>();
        let source = CollectionDescription {
            target: culprit_core::DatabaseIdentity::server(Engine::Mssql, "shop", "fixture.invalid", 1433, None).into(),
            partition: SourcePartition::InstanceLocal {
                member: Some(SharedResourceIdentity {
                    engine: Engine::Mssql,
                    kind: SharedResourceKind::Instance,
                    namespace: NativeIdentity::new("native-machine"),
                    native: NativeIdentity::new("native-member"),
                }),
                epoch: Some("original-epoch".into()),
            },
            source: "original-activity".into(),
            continuity: SourceContinuity::Unverified,
            observation: SourceObservation::Activity(ActivitySnapshotDescription {
                epoch: Some("original-epoch".into()),
                database_writable: Some(true),
                observed_from: "2026-11-13T10:00:00Z".into(),
                observed_until: "2026-11-13T10:00:00.05Z".into(),
                at: "2026-11-13T10:00:00.025Z".into(),
                scope: culprit_core::MeasurementScope::Database,
                population: ActivityPopulation::ActiveRequestsOpenTransactionsAndBlockers,
                native_rows: rows.len() as u32,
                observations: rows.len() as u32,
                coverage: StateSourceCoverage::collected("original-activity".into(), rows.len() as u32),
            }),
        };
        let capture = CaptureIdentity {
            observation_id: "original-activity".into(),
            revision: Some(CaptureRevision {
                outbox_id: digest(b"outbox"),
                owner_epoch: "1".into(),
                sequence: "1".into(),
            }),
        };
        let references = rows
            .iter()
            .enumerate()
            .map(|(ordinal, row)| {
                let exported = ExportedRecord::new(&source, &capture, TelemetryRecord::from(row.clone())).unwrap();
                DiagnosticEvidenceRef::from_export(&source, &digest(b"manifest"), &exported, ordinal as u32).unwrap()
            })
            .collect::<Vec<_>>();
        let original = ActivityRequestFocus {
            parent: references[0].clone(),
            target: source.target.clone(),
            partition: source.partition.clone(),
            database_writable: Some(true),
            source: source.source.clone(),
            epoch: "original-epoch".into(),
            original_observed_at: "2026-11-13T10:00:00.025Z".into(),
            statement_identity: waiter.statement_identity.clone().unwrap(),
            original: waiter,
        };
        Self { source, rows, references, original }
    }

    fn borrowed(&self) -> Vec<ActivityFrameRow<'_>> {
        self.rows
            .iter()
            .zip(&self.references)
            .map(|(observation, reference)| ActivityFrameRow { observation, reference: Some(reference) })
            .collect()
    }

    fn page_source(&self, page: &PageWaitObservation) -> CollectionDescription {
        CollectionDescription {
            target: self.source.target.clone(),
            partition: self.source.partition.clone(),
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
                database_writable: None,
                original_activity: None,
            },
        }
    }

    fn rebind(&mut self) {
        let capture = self.references[0].capture.clone();
        let manifest = self.references[0].manifest_sha256.clone();
        self.references = self
            .rows
            .iter()
            .enumerate()
            .map(|(ordinal, row)| {
                let exported = ExportedRecord::new(&self.source, &capture, TelemetryRecord::from(row.clone())).unwrap();
                DiagnosticEvidenceRef::from_export(&self.source, &manifest, &exported, ordinal as u32).unwrap()
            })
            .collect();
        self.original.parent = self.references[0].clone();
        self.original.original = self.rows[0].clone();
        self.original.statement_identity = self.rows[0].statement_identity.clone().unwrap();
    }

    fn empty_focus(
        &self,
        page: &PageWaitObservation,
        request: PageWaitRequestState,
        wait: PageWaitRelation,
    ) -> CollectionDescription {
        let mut source = self.page_source(page);
        let SourceObservation::PageWaitTargets { mut snapshot, .. } = source.observation else { unreachable!() };
        snapshot.records = 0;
        snapshot.sources[0] = StateSourceCoverage::observed(page.source.clone(), 0, false);
        source.observation = SourceObservation::FocusedPageWait {
            result: Box::new(ActivityWaitFocusResult {
                focus: ActivityWaitFocus { participant: self.original.clone(), resource: page.resource.clone() },
                observed_from: snapshot.observed_from.clone(),
                observed_until: snapshot.observed_until.clone(),
                source: FocusSourceState::Matched,
                request,
                wait,
            }),
            snapshot,
        };
        source
    }

    fn empty_report(
        &self,
        page_source: &CollectionDescription,
        rows: &[ActivityFrameRow<'_>],
        complete: bool,
    ) -> Result<PageWaitReport> {
        let frame = ActivityFrameEvidence::new(&self.source, rows, complete)?;
        let SourceObservation::FocusedPageWait { result, .. } = &page_source.observation else { unreachable!() };
        explain(
            &[],
            Some(&source(SourceState::Partial)),
            Some(PageWaitActivityEvidence::Focused {
                source: page_source,
                original: result.focus.participant(),
                frame: &frame,
            }),
        )
    }
}

#[test]
fn authenticated_original_frame_exposes_all_mars_commands_as_historical_session_context_without_sql_or_costs() {
    let mut page = observation("61", WaitResourceKind::MemoryPage);
    let original = OriginalFrame::new(&mut page);
    let frame = ActivityFrameEvidence::new(&original.source, &original.borrowed(), true).unwrap();
    let page_source = original.page_source(&page);
    let report = explain(
        &[&page],
        Some(&source(SourceState::Partial)),
        Some(PageWaitActivityEvidence::Focused { source: &page_source, original: &original.original, frame: &frame }),
    )
    .unwrap();
    let entry = &report.entries[0];
    assert_eq!(entry.blocking.contexts.len(), 2);
    assert_eq!(entry.blocking.references().cloned().collect::<Vec<_>>(), original.references[1..]);
    assert_eq!(entry.blocking.contexts[0].command.as_deref(), Some("INSERT"));
    assert_eq!(entry.blocking.contexts[1].command.as_deref(), Some("SELECT"));
    assert!(entry.explanation.contains(&t!("view.page_waits.blocking.historical")));
    assert!(entry.explanation.contains(&t!("view.page_waits.blocking.multiple_requests")));
    let encoded = serde_json::to_string(&report).unwrap();
    for raw in ["private owner SQL", "secret=8821", "9007199254740993", "private-app-label", "SELECT payload"] {
        assert!(!encoded.contains(raw), "{raw}");
    }
    assert_eq!(entry.blocking.basis.as_ref().unwrap().waiter.evidence.as_ref(), Some(&original.original.parent));
    assert_eq!(report.source_state, Some(SourceState::Partial));
}

#[test]
fn direct_page_sample_without_original_activity_does_not_reconstruct_blocker_context_from_waiter_sql() {
    let mut page = observation("61", WaitResourceKind::MemoryPage);
    let _original = OriginalFrame::new(&mut page);
    let report = explain(&[&page], Some(&source(SourceState::Partial)), None).unwrap();
    let entry = &report.entries[0];
    assert!(entry.blocking.contexts.is_empty());
    assert!(entry.blocking.basis.is_none());
    assert_eq!(entry.blocking.issues, [PageWaitBlockingIssue::FrameUnavailable]);
    assert!(entry.explanation.contains(&t!("view.page_waits.blocking.frame_unavailable")));
    assert_eq!(entry.request_key, page.request.key);
    assert!(report.recorded_incident.is_none());
}

#[test]
fn changed_and_ended_focus_keeps_only_the_authenticated_original_waiter_and_recorded_sql() {
    let mut page = observation("61", WaitResourceKind::MemoryPage);
    let original = OriginalFrame::new(&mut page);
    let retained = original.rows.clone();
    page.request.statement = DefinitionPayload::sql("SELECT replacement_current_statement_canary", Engine::Mssql);
    for (request, wait) in [
        (PageWaitRequestState::Missing, PageWaitRelation::Unqualified),
        (PageWaitRequestState::Changed, PageWaitRelation::Unqualified),
        (PageWaitRequestState::SameStatement, PageWaitRelation::NoLongerObserved),
    ] {
        let source = original.empty_focus(&page, request, wait);
        let report = original.empty_report(&source, &original.borrowed(), true).unwrap();
        let incident = report.recorded_incident.as_ref().unwrap();
        assert_eq!(incident.original, original.references[0]);
        assert_eq!(incident.observed_at, original.original.original_observed_at);
        assert_eq!(incident.request, original.rows[0].key);
        assert_eq!(incident.resource, page.resource);
        assert_eq!(incident.sql, original.rows[0].statement);
        assert_eq!(incident.client, original.rows[0].client);
        assert!(report.entries.is_empty());
        assert_eq!(report.summary, t!("view.page_waits.summary.empty"));
        assert!(report.limitations.contains(&t!("view.page_waits.limit.ownership")));
        let encoded = serde_json::to_string(&report).unwrap();
        assert!(!encoded.contains("replacement_current_statement_canary"));
        assert!(!encoded.contains("private owner SQL"));
    }
    assert_eq!(original.rows, retained);
}

#[test]
fn recorded_incident_preserves_each_original_sql_unavailability_reason() {
    for reason in [
        PayloadUnavailable::NotCollected,
        PayloadUnavailable::Missing,
        PayloadUnavailable::Restricted,
        PayloadUnavailable::Encrypted,
        PayloadUnavailable::Oversized,
        PayloadUnavailable::RedactionFailed,
        PayloadUnavailable::SourceUnavailable,
    ] {
        let mut page = observation("61", WaitResourceKind::MemoryPage);
        let mut original = OriginalFrame::new(&mut page);
        original.rows[0].statement = DefinitionPayload::Unavailable { reason };
        original.rebind();
        let source = original.empty_focus(&page, PageWaitRequestState::Missing, PageWaitRelation::Unqualified);
        let report = original.empty_report(&source, &original.borrowed(), true).unwrap();
        assert_eq!(report.recorded_incident.unwrap().sql, DefinitionPayload::Unavailable { reason });
    }
}

#[test]
fn recorded_sql_rejects_a_noncanonical_body_and_preserves_the_authenticated_redaction_and_receipt() {
    let mut page = observation("61", WaitResourceKind::MemoryPage);
    let mut original = OriginalFrame::new(&mut page);
    let text = "SELECT payload FROM app.orders WHERE customer=N'private-recorded-value-canary'";
    original.rows[0].statement = DefinitionPayload::Available {
        text: text.into(),
        sha256: digest(text.as_bytes()),
        redaction_version: REDACTION_VERSION,
    };
    original.rebind();
    let source = original.empty_focus(&page, PageWaitRequestState::Missing, PageWaitRelation::Unqualified);
    assert!(original.empty_report(&source, &original.borrowed(), true).is_err());
    let exported = ExportedRecord::new(
        &original.source,
        &original.references[0].capture,
        TelemetryRecord::from(original.rows[0].clone()),
    )
    .unwrap();
    let TelemetryRecord::Activity(canonical) = exported.record else { unreachable!() };
    original.rows[0] = *canonical;
    original.rebind();
    let body = original.rows[0].clone();
    let receipt = original.references[0].clone();
    let source = original.empty_focus(&page, PageWaitRequestState::Missing, PageWaitRelation::Unqualified);
    let report = original.empty_report(&source, &original.borrowed(), true).unwrap();
    let incident = report.recorded_incident.unwrap();
    assert_eq!(incident.sql, DefinitionPayload::sql(text, Engine::Mssql));
    assert!(!serde_json::to_string(&incident).unwrap().contains("private-recorded-value-canary"));
    assert_eq!(incident.original, receipt);
    assert_eq!(original.rows[0], body);
}

#[test]
fn incomplete_original_bodies_and_partial_native_populations_do_not_supply_recorded_sql() {
    let mut page = observation("61", WaitResourceKind::MemoryPage);
    let mut original = OriginalFrame::new(&mut page);
    let source = original.empty_focus(&page, PageWaitRequestState::Missing, PageWaitRelation::Unqualified);
    let rows = original.borrowed();
    for retained in [&rows[..], &rows[1..], &rows[0..0]] {
        let report = original.empty_report(&source, retained, false).unwrap();
        assert!(report.recorded_incident.is_none());
        assert!(report.entries.is_empty());
    }
    let SourceObservation::Activity(native) = &mut original.source.observation else { unreachable!() };
    native.coverage.state = SourceState::Partial;
    native.coverage.complete_scope = false;
    original.rebind();
    let source = original.empty_focus(&page, PageWaitRequestState::Missing, PageWaitRelation::Unqualified);
    assert!(original.empty_report(&source, &original.borrowed(), true).unwrap().recorded_incident.is_none());
}

#[test]
fn a_negative_mars_peer_withdraws_recorded_sql_only_for_its_exact_original_session_generation() {
    for same_session in [true, false] {
        let mut page = observation("61", WaitResourceKind::MemoryPage);
        let mut original = OriginalFrame::new(&mut page);
        let mut peer = original.rows[0].clone();
        peer.key.request.as_mut().unwrap().id = "99".into();
        if !same_session {
            peer.key.session.id = "99".into();
        }
        peer.session_context = Some(Box::new(ActivitySessionContext {
            lifecycle: SessionLifecycle::PoolResetPrelogin,
            login_age_ms: None,
            last_request_end_age_ms: None,
            client_library: None,
            client_process_id: None,
            native: None,
        }));
        original.rows[1] = peer;
        original.rebind();
        let source = original.empty_focus(&page, PageWaitRequestState::Missing, PageWaitRelation::Unqualified);
        let report = original.empty_report(&source, &original.borrowed(), true).unwrap();
        assert_eq!(report.recorded_incident.is_none(), same_session);
    }
}

#[test]
fn copied_focus_and_foreign_receipts_cannot_replace_the_actual_original_waiter() {
    let mut page = observation("61", WaitResourceKind::MemoryPage);
    let original = OriginalFrame::new(&mut page);
    let source = original.empty_focus(&page, PageWaitRequestState::Missing, PageWaitRelation::Unqualified);
    for field in ["capture", "manifest", "scope", "request", "statement", "sql"] {
        let mut changed = source.clone();
        let SourceObservation::FocusedPageWait { result, .. } = &mut changed.observation else { unreachable!() };
        let focus = result.focus.participant_mut();
        match field {
            "capture" => focus.parent.capture.observation_id = "foreign-original-capture".into(),
            "manifest" => focus.parent.manifest_sha256 = digest(b"foreign manifest"),
            "scope" => focus.parent.source_scope_id = digest(b"foreign source scope"),
            "request" => focus.original.key.request.as_mut().unwrap().generation = "reused-native-request".into(),
            "statement" => focus.statement_identity = NativeIdentity::new("different-native-statement"),
            _ => focus.original.statement = DefinitionPayload::sql("SELECT forged_focus_sql", Engine::Mssql),
        }
        assert!(original.empty_report(&changed, &original.borrowed(), true).is_err(), "{field}");
    }
}

fn paired_task_context() -> (OriginalFrame, CollectionDescription, PageWaitObservation) {
    let mut page = observation("61", WaitResourceKind::MemoryPage);
    let mut original = OriginalFrame::new(&mut page);
    original.rows[0].statement =
        DefinitionPayload::sql("SELECT earlier_recorded_request WHERE account=9264", Engine::Mssql);
    let wait = original.rows[0].wait.as_mut().unwrap();
    wait.native_type = Some("LCK_M_X".into());
    wait.identified_resources = vec![culprit_core::ObservedWaitResource {
        kind: WaitResourceKind::DataLock,
        native: NativeIdentity::new("earlier-lock-resource"),
    }];
    original.rebind();
    page.resource.native =
        page.resource.native.clone().qualified("source", "sys.dm_os_waiting_tasks.resource_description");
    page.task_wait = Some(MssqlPageTaskWait {
        observed_at: page.observed_from.clone(),
        session_id: 61,
        request_id: 0,
        execution_context_id: 1,
        task_address: "00000000000000AB".into(),
        wait_type: "PAGELATCH_EX".into(),
        wait_duration_ms: Some("0".into()),
        resource: page.resource.clone(),
        resource_address: None,
        blocking_task_address: None,
        blocking_session_id: None,
        blocking_execution_context_id: None,
    });
    page.statement_identity = None;
    page.request.statement = DefinitionPayload::Unavailable { reason: PayloadUnavailable::Missing };
    page.after_check.as_mut().unwrap().request = PageWaitRequestState::Changed;
    page.after_check.as_mut().unwrap().wait = PageWaitRelation::Unqualified;
    page.resolution = PageWaitResolution::Unavailable;
    page.target = None;
    let mut source = original.page_source(&page);
    let SourceObservation::PageWaitTargets { original_activity, database_writable, .. } = &mut source.observation
    else {
        unreachable!()
    };
    *original_activity = Some(original.references[0].capture_ref());
    *database_writable = Some(true);
    assert!(page.fits_source_scope(&source));
    (original, source, page)
}

fn explain_tasks(
    original: &OriginalFrame,
    source_description: &CollectionDescription,
    pages: &[&PageWaitObservation],
    rows: &[ActivityFrameRow<'_>],
    complete: bool,
) -> Result<PageWaitReport> {
    let frame = ActivityFrameEvidence::new(&original.source, rows, complete)?;
    explain(
        pages,
        Some(&source(SourceState::Partial)),
        Some(PageWaitActivityEvidence::Tasks { source: source_description, frame: &frame }),
    )
}

#[test]
fn native_task_context_retains_earlier_lock_wait_sql_without_changing_task_time_resource_or_ownership() {
    for request_state in [PageWaitRequestState::Changed, PageWaitRequestState::Missing] {
        let (original, source_description, mut page) = paired_task_context();
        page.after_check.as_mut().unwrap().request = request_state;
        let report = explain_tasks(&original, &source_description, &[&page], &original.borrowed(), true).unwrap();
        assert!(report.recorded_incident.is_none());
        let entry = &report.entries[0];
        let recorded = entry.recorded_request.as_ref().unwrap();
        assert_eq!(recorded.observed_at, original.source.observation.activity().unwrap().at);
        assert_ne!(recorded.observed_at, page.task_wait.as_ref().unwrap().observed_at);
        assert_eq!(recorded.original, original.references[0]);
        assert_eq!(recorded.sql, original.rows[0].statement);
        assert_eq!(recorded.client, original.rows[0].client);
        assert_eq!(entry.resource, page.resource);
        assert!(page.statement_identity.is_none() && page.target.is_none());
        assert!(entry.blocking.basis.is_none());
        assert!(entry.blocking.reported_owners.is_empty() && entry.blocking.contexts.is_empty());
        assert_eq!(entry.blocking.issues, [PageWaitBlockingIssue::ParallelTaskUnresolved]);
        let encoded = serde_json::to_string(&entry).unwrap();
        for unrelated in ["earlier-lock-resource", "LCK_M_X", "private owner SQL", "request_counters"] {
            assert!(!encoded.contains(unrelated), "{unrelated}");
        }
        assert!(!encoded.contains("9264"));
    }
}

#[test]
fn parallel_task_contexts_bind_their_own_request_receipts_in_one_original_frame() {
    let (original, mut source_description, page) = paired_task_context();
    let mut parallel = page.clone();
    parallel.task_wait.as_mut().unwrap().execution_context_id = 2;
    parallel.task_wait.as_mut().unwrap().task_address = "00000000000000AC".into();
    let mut peer = page.clone();
    peer.request = original.rows[1].clone();
    peer.task_wait.as_mut().unwrap().session_id = peer.request.key.session.id.parse().unwrap();
    peer.task_wait.as_mut().unwrap().request_id = peer.request.key.request.as_ref().unwrap().id.parse().unwrap();
    peer.task_wait.as_mut().unwrap().task_address = "00000000000000AD".into();
    let SourceObservation::PageWaitTargets { snapshot, .. } = &mut source_description.observation else {
        unreachable!()
    };
    snapshot.records = 3;
    snapshot.sources[0].records = 3;
    let report =
        explain_tasks(&original, &source_description, &[&page, &parallel, &peer], &original.borrowed(), true).unwrap();
    assert_eq!(report.entries.len(), 3);
    for entry in report.entries {
        let recorded = entry.recorded_request.unwrap();
        let index = if entry.request_key == page.request.key { 0 } else { 1 };
        assert_eq!(recorded.original, original.references[index]);
        assert_eq!(recorded.sql, original.rows[index].statement);
        assert_eq!(recorded.request, entry.request_key);
    }
}

#[test]
fn task_recorded_sql_preserves_unavailable_reasons_and_never_uses_later_requester_sql() {
    for reason in [PayloadUnavailable::NotCollected, PayloadUnavailable::Missing, PayloadUnavailable::Oversized] {
        let (mut original, source_description, mut page) = paired_task_context();
        original.rows[0].statement = DefinitionPayload::Unavailable { reason };
        original.rebind();
        page.request.statement =
            DefinitionPayload::sql("SELECT later_requester_sql_must_not_be_borrowed", Engine::Mssql);
        let report = explain_tasks(&original, &source_description, &[&page], &original.borrowed(), true).unwrap();
        assert_eq!(report.entries[0].recorded_request.as_ref().unwrap().sql, DefinitionPayload::Unavailable { reason });
        assert!(!serde_json::to_string(&report).unwrap().contains("later_requester_sql_must_not_be_borrowed"));
    }
}

#[test]
fn task_context_requires_the_actual_complete_original_body_and_never_creates_an_incident_for_no_tasks() {
    let (original, mut source_description, page) = paired_task_context();
    for rows in [original.borrowed()[..1].to_vec(), original.borrowed()[1..].to_vec(), vec![]] {
        let report = explain_tasks(&original, &source_description, &[&page], &rows, false).unwrap();
        assert_eq!(report.entries.len(), 1);
        assert!(report.entries[0].recorded_request.is_none());
    }
    let (mut limited, mut limited_source, limited_page) = paired_task_context();
    let SourceObservation::Activity(native) = &mut limited.source.observation else { unreachable!() };
    native.coverage = StateSourceCoverage::observed(limited.source.source.clone(), limited.rows.len() as u32, false);
    limited.rebind();
    let SourceObservation::PageWaitTargets { original_activity, .. } = &mut limited_source.observation else {
        unreachable!()
    };
    *original_activity = Some(limited.references[0].capture_ref());
    let limited_report = explain_tasks(&limited, &limited_source, &[&limited_page], &limited.borrowed(), true).unwrap();
    assert!(limited_report.entries[0].recorded_request.is_none());
    let SourceObservation::PageWaitTargets { snapshot, .. } = &mut source_description.observation else {
        unreachable!()
    };
    snapshot.records = 0;
    snapshot.sources[0].records = 0;
    let report = explain_tasks(&original, &source_description, &[], &original.borrowed(), true).unwrap();
    assert!(report.entries.is_empty() && report.recorded_incident.is_none());
}

#[test]
fn negative_later_task_peers_withdraw_only_their_observed_session_generation_without_merging_workers() {
    for peer_context in ["same-negative", "other-generation-negative", "nonnegative-worker"] {
        let (original, mut source_description, page) = paired_task_context();
        let mut parallel = page.clone();
        parallel.task_wait.as_mut().unwrap().task_address = "00000000000000AC".into();
        parallel.task_wait.as_mut().unwrap().execution_context_id = 2;
        let mut peer = page.clone();
        peer.task_wait.as_mut().unwrap().task_address = "00000000000000AD".into();
        peer.task_wait.as_mut().unwrap().request_id = 1;
        peer.request.key.request.as_mut().unwrap().id = "1".into();
        if peer_context != "nonnegative-worker" {
            peer.request.session_context = Some(Box::new(ActivitySessionContext {
                lifecycle: SessionLifecycle::Connecting,
                login_age_ms: None,
                last_request_end_age_ms: None,
                client_library: None,
                client_process_id: None,
                native: None,
            }));
        }
        if peer_context == "other-generation-negative" {
            peer.request.key.session.generation.push_str("-other-generation");
        }
        let SourceObservation::PageWaitTargets { snapshot, .. } = &mut source_description.observation else {
            unreachable!()
        };
        snapshot.records = 3;
        snapshot.sources[0].records = 3;
        let pages = [&page, &parallel, &peer];
        let report = explain_tasks(&original, &source_description, &pages, &original.borrowed(), true).unwrap();
        assert_eq!(report.entries.len(), 3);
        for entry in &report.entries {
            let retained = pages.iter().find(|page| page.key().unwrap() == entry.observation_key).unwrap();
            assert_eq!(entry.request_key, retained.request.key);
            assert_eq!(entry.resource, retained.resource);
            assert_eq!(entry.task.as_ref().unwrap().task_address, retained.task_wait.as_ref().unwrap().task_address);
            assert!(entry.blocking.reported_owners.is_empty() && entry.blocking.contexts.is_empty());
            if entry.request_key == page.request.key {
                assert_eq!(entry.recorded_request.is_some(), peer_context != "same-negative", "{peer_context}");
            } else {
                assert!(entry.recorded_request.is_none());
            }
        }
    }
}
