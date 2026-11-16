use super::*;

type Baseline = (CollectionDescription, DiagnosticCaptureRef, Vec<(DiagnosticEvidenceRef, ActivityObservation)>);

struct Scenario {
    baseline: Baseline,
    original: ActivityWaitFocus,
    before: DiagnosticCheckpoint,
    mapped: DiagnosticFocusUpdate,
    page_source: CollectionDescription,
    page: PageWaitObservation,
    page_ref: DiagnosticEvidenceRef,
}

fn scenario(peers: usize) -> Scenario {
    scenario_with_role(peers, Some(true))
}

fn scenario_with_role(peers: usize, role: Option<bool>) -> Scenario {
    let mut waiter = actor();
    waiter.command = None;
    waiter.wait.as_mut().unwrap().owners =
        vec![ActivityWaitOwner::Session { id: "52".into(), generation: Some("session-generation-52".into()) }];
    let rows = std::iter::once(waiter).chain((0..peers).map(|index| {
        let id = (52 + index).to_string();
        let mut row = actor();
        row.command = None;
        row.key.session = NativeActivityIdentity { id: id.clone(), generation: format!("session-generation-{id}") };
        row.key.request.as_mut().unwrap().generation = format!("request-generation-{id}");
        row.statement_identity = Some(NativeIdentity::new(format!("statement-{id}")));
        row.wait = None;
        row
    }));
    let mut baseline = frame(1, rows.collect(), true);
    baseline.0.partition = SourcePartition::InstanceLocal {
        member: Some(SharedResourceIdentity {
            engine: Engine::Mssql,
            kind: SharedResourceKind::Instance,
            namespace: NativeIdentity::new("fixture-host"),
            native: NativeIdentity::new("fixture-instance"),
        }),
        epoch: Some("native-epoch".into()),
    };
    let SourceObservation::Activity(native) = &mut baseline.0.observation else { panic!() };
    native.database_writable = role;
    rebind(&mut baseline);
    let observed = observe(&baseline, None);
    let original = observed.focuses[0].1.page_wait().unwrap().clone();
    let page = mapped_page(&original);
    let focused = focus_frame(
        original.clone(),
        PageWaitRequestState::SameStatement,
        PageWaitRelation::OriginalObserved,
        vec![page],
    );
    let rows = [DiagnosticFocusRow { reference: &focused.2[0].0, observation: &focused.2[0].1 }];
    let mapped =
        analyze_diagnostic_focus(&focused.0, &focused.1, &rows, &observed.checkpoint, "2020-01-01T00:02:03Z").unwrap();
    Scenario {
        baseline,
        original,
        before: observed.checkpoint,
        mapped,
        page_source: focused.0,
        page_ref: focused.2[0].0.clone(),
        page: focused.2[0].1.clone(),
    }
}

fn rebind(baseline: &mut Baseline) {
    baseline.1.source_scope_id = baseline.0.captured_range().unwrap().scope_id;
    for (ordinal, (reference, row)) in baseline.2.iter_mut().enumerate() {
        let exported =
            ExportedRecord::new(&baseline.0, &baseline.1.capture, TelemetryRecord::from(row.clone())).unwrap();
        *reference =
            DiagnosticEvidenceRef::from_export(&baseline.0, &baseline.1.manifest_sha256, &exported, ordinal as u32)
                .unwrap();
    }
}

fn mapped_page(original: &ActivityWaitFocus) -> PageWaitObservation {
    PageWaitObservation {
        source: "page-wait-targets".into(),
        observed_from: "2020-01-01T00:02:00Z".into(),
        observed_until: "2020-01-01T00:02:01Z".into(),
        request: original.participant().original.clone(),
        task_wait: None,
        before_check: Some(PageWaitRecheck {
            observed_at: "2020-01-01T00:02:00Z".into(),
            request: PageWaitRequestState::SameStatement,
            wait: PageWaitRelation::OriginalObserved,
        }),
        after_check: Some(PageWaitRecheck {
            observed_at: "2020-01-01T00:02:01Z".into(),
            request: PageWaitRequestState::SameStatement,
            wait: PageWaitRelation::OriginalObserved,
        }),
        statement_identity: Some(original.participant().statement_identity.clone()),
        plan_identity: Some(NativeIdentity::new("selected-native-plan")),
        resource: original.resource.clone(),
        observed_address: Some(PageWaitAddress { database_id: 5, file_id: 1, page_id: 99 }),
        resolution: PageWaitResolution::ObservedAtBothBounds,
        target: Some(Box::new(MappedPageTarget {
            scope: PageTargetScope::RequestDatabase,
            identity_basis: PageTargetIdentityBasis::LocalDatabaseFileEpoch,
            database: NativeIdentity::new("5").qualified("database_guid", "native-db"),
            file: NativeIdentity::new("1").qualified("file_guid", "native-file"),
            header: PageHeaderFacts {
                database_id: 5,
                file_id: 1,
                page_id: 99,
                header_version: Some(1),
                page_type: Some(2),
                level: Some(0),
                object_id: Some("128".into()),
                index_id: Some("1".into()),
                partition_id: None,
                allocation_unit_id: Some("72057594000000001".into()),
                lsn: None,
                next_file_id: None,
                next_page_id: None,
            },
            kind: PageWaitTargetKind::IndexLeaf,
            object: Some(PageWaitObject {
                native: NativeIdentity::new("128")
                    .qualified("created_without_offset", "2019-01-01T00:00:00")
                    .qualified("modified_without_offset", "2019-01-02T00:00:00")
                    .qualified("index_type", "1")
                    .qualified("index_id", "1"),
                schema: "dbo".into(),
                name: "orders".into(),
                index_name: Some("orders_id".into()),
            }),
        })),
        index_operations: None,
    }
}

fn frame_evidence(baseline: &Baseline, read_complete: bool) -> ActivityFrameEvidence<'_> {
    let rows = baseline
        .2
        .iter()
        .map(|(reference, observation)| ActivityFrameRow { observation, reference: Some(reference) })
        .collect::<Vec<_>>();
    ActivityFrameEvidence::new(&baseline.0, &rows, read_complete).unwrap()
}

fn writer_focus(scenario: &Scenario) -> PageWaitWriterFocus {
    let frame = frame_evidence(&scenario.baseline, true);
    let DiagnosticFocusPlan::Ready(focus) = scenario.mapped.checkpoint.episodes[0]
        .plan_page_wait_writers(&scenario.page_source, &scenario.page, &scenario.page_ref, &scenario.original, &frame)
        .unwrap()
    else {
        panic!("a mapped qualified rowstore page owes a distinct writer question")
    };
    let DiagnosticFocus::PageWaitWriters { focus } = *focus else { panic!() };
    *focus
}

pub(super) fn investigation_case() -> (DiagnosticEpisode, PageWaitWriterFocus) {
    let scenario = scenario(2);
    (scenario.mapped.checkpoint.episodes[0].clone(), writer_focus(&scenario))
}

fn writer_read(
    scenario: &Scenario,
    target_state: PageWriterTargetState,
    complete: bool,
) -> (CollectionDescription, DiagnosticCaptureRef, DiagnosticEvidenceRef, OperationalObservation) {
    let focus = writer_focus(scenario);
    let mut page = scenario.page.clone();
    page.observed_from = "2020-01-01T00:03:00Z".into();
    page.observed_until = "2020-01-01T00:03:01Z".into();
    page.before_check.as_mut().unwrap().observed_at = page.observed_from.clone();
    page.after_check.as_mut().unwrap().observed_at = page.observed_until.clone();
    let point = PageWaitWriterObservation {
        observed_from: page.observed_from.clone(),
        observed_until: page.observed_until.clone(),
        focus_key: focus.key().unwrap(),
        target_state,
        candidates: focus
            .candidates
            .iter()
            .map(|candidate| {
                let row = scenario.baseline.2.iter().find(|(_, row)| row.key == candidate.key).unwrap();
                let current = target_state == PageWriterTargetState::CurrentMapped;
                PageWaitWriterCandidate {
                    parent: candidate.parent.clone(),
                    request_state: if current {
                        PageWaitRequestState::SameStatement
                    } else {
                        PageWaitRequestState::Missing
                    },
                    request: current.then(|| {
                        let mut request = row.1.clone();
                        request.command = Some("SELECT".into());
                        request.statement = DefinitionPayload::sql("SELECT value FROM dbo.orders", Engine::Mssql);
                        request
                    }),
                    plan_identity: current.then(|| NativeIdentity::new("same-selected-plan")),
                    plan: if current {
                        DefinitionPayload::plan("<ShowPlanXML/>", Engine::Mssql)
                    } else {
                        DefinitionPayload::Unavailable { reason: PayloadUnavailable::Missing }
                    },
                    relation: if current { PageWriterRelation::ReadOnlyPlan } else { PageWriterRelation::Unavailable },
                    binding: None,
                }
            })
            .collect(),
        page: (target_state == PageWriterTargetState::CurrentMapped).then(|| Box::new(page)),
    };
    for candidate in &point.candidates {
        assert!(candidate.is_valid(), "controlled native candidate: {candidate:?}");
    }
    assert!(point.is_valid(), "controlled writer point keeps each independently guarded candidate");
    let source = CollectionDescription {
        target: scenario.page_source.target.clone(),
        partition: scenario.page_source.partition.clone(),
        source: scenario.page_source.source.clone(),
        continuity: SourceContinuity::Unverified,
        observation: SourceObservation::PageWaitWriters {
            snapshot: StateSnapshotDescription {
                dataset: SnapshotDataset::PageWaitWriters,
                observed_from: point.observed_from.clone(),
                observed_until: point.observed_until.clone(),
                records: 1,
                sources: vec![StateSourceCoverage::observed(scenario.page_source.source.clone(), 1, complete)],
            },
            focus: Box::new(focus),
            database_writable: Some(true),
        },
    };
    let capture = CaptureIdentity {
        observation_id: "writer-result".into(),
        revision: Some(CaptureRevision {
            outbox_id: "writer-outbox".into(),
            owner_epoch: "1".into(),
            sequence: "3".into(),
        }),
    };
    let row = OperationalObservation {
        source: source.source.clone(),
        key: point.key().unwrap(),
        fact: OperationalFact::PageWaitWriters(Box::new(point)),
    };
    let exported = ExportedRecord::new(&source, &capture, TelemetryRecord::Operational(row.clone())).unwrap();
    let reference = DiagnosticEvidenceRef::from_export(&source, &digest(b"writer-manifest"), &exported, 0).unwrap();
    (source, reference.capture_ref(), reference, row)
}

#[test]
fn a_mapped_rowstore_page_advances_the_same_incident_to_a_bounded_original_writer_roster() {
    let scenario = scenario(12);
    let episode = &scenario.mapped.checkpoint.episodes[0];
    assert_eq!(episode.id, scenario.before.episodes[0].id);
    assert_eq!(episode.analysis, DiagnosticAnalysisStatus::Partial);
    assert!(episode.analysis.needs_work());
    let focus = writer_focus(&scenario);
    assert_eq!(focus.candidates.len(), MAX_PAGE_WAIT_WRITER_CANDIDATES);
    assert_eq!(focus.candidates[0].parent, scenario.original.participant().parent);
    assert_eq!(focus.candidates[1].key.session.id, "52", "exact native owner context is selected before other peers");
    assert!(focus.candidates.iter().all(|candidate| candidate.parent.capture_ref() == scenario.baseline.1));
    assert!(focus.matches_page(&scenario.page_source, &scenario.page, &scenario.page_ref));
    let question = DiagnosticFocus::PageWaitWriters { focus: Box::new(focus) };
    assert_eq!(question.selector(), RecipeSelector::FocusedPageWaitWriters);
    assert_eq!(question.references().count(), MAX_PAGE_WAIT_WRITER_CANDIDATES + 1);
}

#[test]
fn unknown_original_updateability_cannot_authorize_or_advertise_a_writer_question() {
    let scenario = scenario_with_role(2, None);
    let frame = frame_evidence(&scenario.baseline, true);
    assert!(scenario.original.is_valid(), "ordinary page evidence does not acquire stronger writer prerequisites");
    assert!(!PageWaitWriterFocus::accepts_waiter(&scenario.original));
    assert!(!supports_page_wait_writers(&scenario.page_source, &scenario.page, &scenario.original));
    assert_eq!(
        plan_page_wait_writer_focus(
            &scenario.page_source,
            &scenario.page,
            &scenario.page_ref,
            &scenario.original,
            &frame,
        )
        .unwrap(),
        DiagnosticFocusPlan::Unavailable(DiagnosticAnalysisIssue::EvidenceUnavailable),
    );
    let mut known = writer_focus(&scenario_with_role(2, Some(true)));
    known.waiter.participant.database_writable = None;
    assert!(!known.is_valid(), "direct reconstructed focuses obey the same original-role contract");
    assert!(
        writer_focus(&scenario_with_role(2, Some(false))).is_valid(),
        "known read-only remains eligible for bounded read-only evidence"
    );
}

#[test]
fn unsupported_outside_io_and_allocation_questions_do_not_become_missing_role_writer_questions() {
    for kind in ["outside", "io", "allocation"] {
        let scenario = scenario_with_role(2, None);
        let mut baseline = scenario.baseline;
        if kind == "io" {
            let wait = baseline.2[0].1.wait.as_mut().unwrap();
            wait.native_type = Some("PAGEIOLATCH_SH".into());
            wait.identified_resources[0].kind = WaitResourceKind::PageIo;
            rebind(&mut baseline);
        }
        let observed = observe(&baseline, None);
        let original = observed.focuses.iter().find_map(|(_, focus)| focus.page_wait()).unwrap();
        assert!(original.participant.database_writable.is_none());
        let mut page = mapped_page(original);
        match kind {
            "outside" => {
                page.resolution = PageWaitResolution::OutsideDatabase;
                page.target = None;
                page.statement_identity = None;
                page.plan_identity = None;
            }
            "allocation" => {
                let target = page.target.as_mut().unwrap();
                target.kind = PageWaitTargetKind::Pfs;
                target.header.page_type = Some(11);
                target.header.object_id = None;
                target.header.index_id = None;
                target.header.partition_id = None;
                target.object = None;
            }
            _ => (),
        }
        let focused = focus_frame(
            original.clone(),
            PageWaitRequestState::SameStatement,
            PageWaitRelation::OriginalObserved,
            vec![page],
        );
        let frame = frame_evidence(&baseline, true);
        let (reference, page) = &focused.2[0];
        assert_eq!(
            plan_page_wait_writer_focus(&focused.0, page, reference, original, &frame).unwrap(),
            DiagnosticFocusPlan::Unavailable(DiagnosticAnalysisIssue::Unsupported),
            "{kind} retains its independent original question, without acquiring writer scope or a role",
        );
        assert!(!supports_page_wait_writers(&focused.0, page, original));
    }
}

#[test]
fn another_page_receipt_or_modified_original_frame_cannot_authorize_a_writer_question() {
    let scenario = scenario(2);
    let frame = frame_evidence(&scenario.baseline, true);
    let mut receipt = scenario.page_ref.clone();
    receipt.payload_sha256 = digest(b"changed mapped page");
    assert!(
        plan_page_wait_writer_focus(&scenario.page_source, &scenario.page, &receipt, &scenario.original, &frame)
            .is_err()
    );
    let mut other = scenario.baseline.clone();
    other.2[0].1.statement_identity = Some(NativeIdentity::new("replacement-statement"));
    rebind(&mut other);
    let other_frame = frame_evidence(&other, true);
    assert!(
        plan_page_wait_writer_focus(
            &scenario.page_source,
            &scenario.page,
            &scenario.page_ref,
            &scenario.original,
            &other_frame,
        )
        .is_err()
    );
    let mut other_page = scenario.page.clone();
    other_page.target.as_mut().unwrap().header.page_id += 1;
    assert!(plan_page_wait_writer_focus(
        &scenario.page_source, &other_page, &scenario.page_ref, &scenario.original, &frame,
    ).is_err());
}

#[test]
fn a_missing_original_on_an_incomplete_archive_is_unavailable_without_substituting_a_current_row() {
    let scenario = scenario(2);
    let mut incomplete = scenario.baseline.clone();
    incomplete.2.remove(0);
    let frame = frame_evidence(&incomplete, false);
    assert_eq!(
        plan_page_wait_writer_focus(
            &scenario.page_source,
            &scenario.page,
            &scenario.page_ref,
            &scenario.original,
            &frame
        )
        .unwrap(),
        DiagnosticFocusPlan::Unavailable(DiagnosticAnalysisIssue::EvidenceUnavailable),
    );
    assert_eq!(scenario.mapped.checkpoint.episodes[0].lifecycle, DiagnosticLifecycle::Active);
}

#[test]
fn writer_results_retain_page_candidates_and_overlapping_blocker_receipts_once_without_resolving_the_wait() {
    let mut scenario = scenario(2);
    let blocker = scenario.baseline.2[1].0.clone();
    let Some(DiagnosticReport::PageWait { blocking_contexts, .. }) = &mut scenario.mapped.checkpoint.episodes[0].report
    else {
        panic!()
    };
    blocking_contexts.push(blocker.clone());
    let (source, input, reference, row) = writer_read(&scenario, PageWriterTargetState::CurrentMapped, true);
    let rows = [DiagnosticPageWriterRow { reference: &reference, observation: &row }];
    let update = analyze_diagnostic_page_writers(
        &source,
        &input,
        &rows,
        &scenario.mapped.checkpoint,
        "2020-01-01T00:04:00Z",
        None,
    )
    .unwrap();
    let episode = &update.checkpoint.episodes[0];
    assert_eq!(episode.analysis, DiagnosticAnalysisStatus::Analyzed);
    assert_eq!(episode.lifecycle, DiagnosticLifecycle::Active);
    assert_eq!(episode.first, scenario.before.episodes[0].first);
    assert_eq!(episode.latest, scenario.before.episodes[0].latest);
    assert_eq!(episode.last_seen_at, scenario.before.episodes[0].last_seen_at);
    assert!(episode.recovery.is_none());
    let report = episode.report.as_ref().unwrap();
    assert!(report.matches_writer_input(&source, [(&reference, &row)]));
    assert_eq!(report.references().filter(|candidate| **candidate == blocker).count(), 1);
    assert!(report.references().any(|candidate| *candidate == scenario.page_ref));
    assert!(report.references().all(|candidate| update.revisions[0].inputs.contains(candidate)));
    let restored: DiagnosticCheckpoint =
        serde_json::from_slice(&serde_json::to_vec(&update.checkpoint).unwrap()).unwrap();
    assert_eq!(restored, update.checkpoint);
}

#[test]
fn missing_current_requests_changed_mapping_and_partial_visibility_never_supply_recovery() {
    let scenario = scenario(2);
    for (state, complete, analysis) in [
        (PageWriterTargetState::NoLongerObserved, true, DiagnosticAnalysisStatus::Unresolved),
        (PageWriterTargetState::ChangedTarget, true, DiagnosticAnalysisStatus::Unresolved),
        (PageWriterTargetState::Unavailable, false, DiagnosticAnalysisStatus::Unresolved),
        (PageWriterTargetState::CurrentMapped, false, DiagnosticAnalysisStatus::Partial),
    ] {
        let (source, input, reference, row) = writer_read(&scenario, state, complete);
        let rows = [DiagnosticPageWriterRow { reference: &reference, observation: &row }];
        let update = analyze_diagnostic_page_writers(
            &source,
            &input,
            &rows,
            &scenario.mapped.checkpoint,
            "2020-01-01T00:04:00Z",
            None,
        )
        .unwrap();
        let episode = &update.checkpoint.episodes[0];
        assert_eq!(episode.analysis, analysis, "{state:?}");
        assert_eq!(episode.lifecycle, DiagnosticLifecycle::Active, "{state:?}");
        assert!(episode.recovery.is_none());
        assert_eq!(episode.last_seen_at, scenario.before.episodes[0].last_seen_at);
    }
}

#[test]
fn writer_input_validation_rejects_a_replaced_roster_or_an_unsealed_raw_payload() {
    let scenario = scenario(2);
    let (source, input, reference, row) = writer_read(&scenario, PageWriterTargetState::CurrentMapped, true);
    let mut changed = row.clone();
    let OperationalFact::PageWaitWriters(point) = &mut changed.fact else { panic!() };
    point.candidates.swap(0, 1);
    assert!(
        analyze_diagnostic_page_writers(
            &source,
            &input,
            &[DiagnosticPageWriterRow { reference: &reference, observation: &changed }],
            &scenario.mapped.checkpoint,
            "2020-01-01T00:04:00Z",
            None,
        )
        .is_err()
    );
    let mut forged = reference.clone();
    forged.payload_sha256 = digest(b"forged writer payload");
    assert!(
        analyze_diagnostic_page_writers(
            &source,
            &input,
            &[DiagnosticPageWriterRow { reference: &forged, observation: &row }],
            &scenario.mapped.checkpoint,
            "2020-01-01T00:04:00Z",
            None,
        )
        .is_err()
    );
}

#[test]
fn a_bound_object_writer_plan_explains_the_original_incident_without_becoming_a_latch_owner() {
    let scenario = scenario(2);
    let (source, input, _, mut row) = writer_read(&scenario, PageWriterTargetState::CurrentMapped, true);
    let SourceObservation::PageWaitWriters { focus, .. } = &source.observation else { panic!() };
    let OperationalFact::PageWaitWriters(point) = &mut row.fact else { panic!() };
    let candidate = &mut point.candidates[1];
    candidate.request.as_mut().unwrap().command = Some("INSERT".into());
    candidate.request.as_mut().unwrap().statement =
        DefinitionPayload::sql("INSERT INTO dbo.orders VALUES (9264)", Engine::Mssql);
    candidate.relation = PageWriterRelation::TargetObjectWrite;
    candidate.binding = Some(
        MssqlPageWritePlanProof::new(
            &focus.target,
            candidate,
            digest(b"original native selected plan"),
            digest(b"original native selected statement"),
            vec![MssqlPageWriteAccess { node_id: 4, operation: PageWriteOperation::Insert }],
        )
        .unwrap(),
    );
    let exported = ExportedRecord::new(&source, &input.capture, TelemetryRecord::Operational(row.clone())).unwrap();
    let reference = DiagnosticEvidenceRef::from_export(&source, &input.manifest_sha256, &exported, 0).unwrap();
    let update = analyze_diagnostic_page_writers(
        &source,
        &input,
        &[DiagnosticPageWriterRow { reference: &reference, observation: &row }],
        &scenario.mapped.checkpoint,
        "2020-01-01T00:04:00Z",
        None,
    )
    .unwrap();
    let episode = &update.checkpoint.episodes[0];
    assert_eq!(episode.id, scenario.before.episodes[0].id);
    assert_eq!(episode.analysis, DiagnosticAnalysisStatus::Analyzed);
    assert_eq!(episode.lifecycle, DiagnosticLifecycle::Active);
    assert_eq!(episode.latest, scenario.before.episodes[0].latest);
    assert!(episode.recovery.is_none());
    assert!(episode.report.as_ref().unwrap().matches_writer_input(&source, [(&reference, &row)]));
}

#[test]
fn oversized_context_roles_are_rejected_even_when_the_distinct_total_fits_the_input_policy() {
    let scenario = scenario(crate::page_wait_report::MAX_PAGE_WAIT_BLOCKING_CONTEXTS + 1);
    let (source, _, reference, row) = writer_read(&scenario, PageWriterTargetState::CurrentMapped, true);
    let SourceObservation::PageWaitWriters { focus, .. } = &source.observation else { panic!() };
    let mut report = DiagnosticReport::PageWaitWriters {
        observation: reference.clone(),
        page: Box::new(scenario.page_ref.clone()),
        candidates: focus.candidates.iter().map(|candidate| candidate.parent.clone()).collect(),
        blocking_contexts: scenario.baseline.2[1..].iter().map(|(reference, _)| reference.clone()).collect(),
    };
    assert!(report.references().count() < MAX_DIAGNOSTIC_INPUTS);
    assert!(!report.is_valid(), "the separate historical-context role remains bounded");
    let DiagnosticReport::PageWaitWriters { blocking_contexts, .. } = &mut report else { panic!() };
    blocking_contexts.pop();
    assert!(report.is_valid());
    assert!(report.matches_writer_input(&source, [(&reference, &row)]));
}

#[test]
fn an_earlier_native_recovery_stays_intact_when_historical_writer_analysis_finishes() {
    let scenario = scenario(2);
    let mut empty = frame(4, vec![], true);
    empty.0.partition = scenario.baseline.0.partition.clone();
    let SourceObservation::Activity(native) = &mut empty.0.observation else { panic!() };
    native.database_writable = Some(true);
    rebind(&mut empty);
    let recovered = observe(&empty, Some(&scenario.mapped.checkpoint));
    let original = recovered.checkpoint.episodes[0].clone();
    assert_eq!(original.lifecycle, DiagnosticLifecycle::Resolved);
    assert!(original.analysis.needs_work());
    let (source, input, reference, row) = writer_read(&scenario, PageWriterTargetState::CurrentMapped, true);
    let update = analyze_diagnostic_page_writers(
        &source,
        &input,
        &[DiagnosticPageWriterRow { reference: &reference, observation: &row }],
        &recovered.checkpoint,
        "2020-01-01T00:05:00Z",
        None,
    )
    .unwrap();
    assert!(update.checkpoint.episodes.is_empty(), "completed history stays in the immutable revision");
    let finished = &update.revisions[0].episode;
    assert_eq!(finished.analysis, DiagnosticAnalysisStatus::Analyzed);
    assert_eq!(finished.lifecycle, DiagnosticLifecycle::Resolved);
    assert_eq!(finished.recovery, original.recovery);
    assert_eq!(finished.first, original.first);
    assert_eq!(finished.last_seen_at, original.last_seen_at);
    assert!(
        finished.report.as_ref().unwrap().references().all(|reference| update.revisions[0].inputs.contains(reference))
    );
}
