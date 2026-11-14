use super::*;
use crate::{DatabaseIdentity, Engine, MeasurementScope, ObservedWaitResource};

mod planning;

fn actor() -> ActivityObservation {
    ActivityObservation {
        blocking_status: ActivityBlockingStatus::Known,
        waiting_on_lock: None,
        key: ActivityKey {
            session: NativeActivityIdentity { id: "51".into(), generation: "session-generation".into() },
            request: Some(NativeActivityIdentity { id: "0".into(), generation: "request-generation".into() }),
            transaction: None,
        },
        idle: false,
        scope: ActivityScopeBasis::RequestDatabase,
        state: "suspended".into(),
        database: Some("app".into()),
        command: Some("INSERT".into()),
        last_request_age_ms: None,
        open_transactions: Some(0),
        elapsed_ms: Some("1".into()),
        transaction_age_ms: None,
        session_counters: ActivityCounters::default(),
        request_counters: Some(ActivityCounters::default()),
        wait: Some(ActivityWait {
            identified_resources: vec![ObservedWaitResource {
                kind: WaitResourceKind::MemoryPage,
                native: NativeIdentity::new("5:1:99"),
            }],
            native_type: Some("PAGELATCH_EX".into()),
            elapsed_ms: Some("1".into()),
            owners: vec![],
            resources: None,
        }),
        query_hash: None,
        statement_identity: Some(NativeIdentity::new("native-statement")),
        client: None,
        statement: DefinitionPayload::Unavailable { reason: PayloadUnavailable::NotCollected },
    }
}

fn frame(
    sequence: u64,
    rows: Vec<ActivityObservation>,
    complete: bool,
) -> (CollectionDescription, DiagnosticCaptureRef, Vec<(DiagnosticEvidenceRef, ActivityObservation)>) {
    let at = format!("2020-01-01T00:{:02}:00Z", sequence);
    let source = CollectionDescription {
        target: DatabaseIdentity::server(Engine::Mssql, "app", "fixture.invalid", 1433, Some("native-db".into()))
            .into(),
        source: "native-activity".into(),
        continuity: SourceContinuity::Unverified,
        observation: SourceObservation::Activity(ActivitySnapshotDescription {
            epoch: Some("native-epoch".into()),
            observed_from: at.clone(),
            observed_until: at.clone(),
            at,
            scope: MeasurementScope::Database,
            population: ActivityPopulation::ActiveRequestsOpenTransactionsAndBlockers,
            native_rows: rows.len() as u32,
            observations: rows.len() as u32,
            coverage: StateSourceCoverage::observed("native-activity".into(), rows.len() as u32, complete),
        }),
    };
    let capture = CaptureIdentity {
        observation_id: format!("capture-{sequence}"),
        revision: Some(CaptureRevision {
            outbox_id: "owned-outbox".into(),
            owner_epoch: "1".into(),
            sequence: sequence.to_string(),
        }),
    };
    let input = DiagnosticCaptureRef {
        capture: capture.clone(),
        manifest_sha256: digest(format!("manifest-{sequence}").as_bytes()),
        source_scope_id: source.captured_range().unwrap().scope_id,
    };
    let rows = rows
        .into_iter()
        .enumerate()
        .map(|(ordinal, row)| {
            let exported = ExportedRecord::new(&source, &capture, TelemetryRecord::from(row.clone())).unwrap();
            let reference =
                DiagnosticEvidenceRef::from_export(&source, &input.manifest_sha256, &exported, ordinal as u32).unwrap();
            (reference, row)
        })
        .collect();
    (source, input, rows)
}

fn observe(
    frame: &(CollectionDescription, DiagnosticCaptureRef, Vec<(DiagnosticEvidenceRef, ActivityObservation)>),
    before: Option<&DiagnosticCheckpoint>,
) -> DiagnosticObservationUpdate {
    let rows = frame
        .2
        .iter()
        .map(|(reference, observation)| DiagnosticActivityRow { reference, observation })
        .collect::<Vec<_>>();
    observe_diagnostic_activity(&frame.0, &frame.1, &rows, before, &ActivityPolicy::default(), "2020-01-01T01:00:00Z")
        .unwrap()
}

#[test]
fn a_short_native_page_wait_is_retained_without_claiming_an_owner_or_a_peak() {
    let first = observe(&frame(1, vec![actor()], true), None);
    assert_eq!(first.revisions.len(), 1);
    let episode = &first.checkpoint.episodes[0];
    assert_eq!(episode.lifecycle, DiagnosticLifecycle::Active);
    assert_eq!(episode.analysis, DiagnosticAnalysisStatus::Pending);
    assert!(episode.peak.is_none() && episode.report.is_none());
    assert_eq!(first.focuses.len(), 1);
    assert_eq!(first.focuses[0].1.original.elapsed_ms.as_deref(), Some("1"));
}

fn lock_wait_actor() -> ActivityObservation {
    let mut row = actor();
    row.waiting_on_lock = Some(true);
    row.blocking_status = ActivityBlockingStatus::Unknown;
    row.elapsed_ms = Some("40000".into());
    let wait = row.wait.as_mut().unwrap();
    wait.native_type = Some("native-lock".into());
    wait.identified_resources.clear();
    wait.elapsed_ms = None;
    row
}

#[test]
fn a_native_lock_point_does_not_require_an_owner_or_borrow_the_requests_age_as_wait_duration() {
    let first = observe(&frame(1, vec![lock_wait_actor()], true), None);
    assert_eq!(first.checkpoint.episodes.len(), 1);
    let episode = &first.checkpoint.episodes[0];
    assert!(matches!(episode.signal, DiagnosticSignal::LockWait { .. }));
    assert_eq!(episode.lifecycle, DiagnosticLifecycle::Active);
    assert_eq!(episode.analysis, DiagnosticAnalysisStatus::Pending);
    assert!(episode.report.is_none() && episode.peak.is_none());
    assert!(first.focuses.is_empty());
}

#[test]
fn unknown_lock_visibility_keeps_the_last_native_point_without_freshening_or_resolving_it() {
    let first = observe(&frame(1, vec![lock_wait_actor()], true), None);
    let mut unknown = lock_wait_actor();
    unknown.waiting_on_lock = None;
    let update = observe(&frame(2, vec![unknown], true), Some(&first.checkpoint));
    let episode = &update.checkpoint.episodes[0];
    assert_eq!(episode.lifecycle, DiagnosticLifecycle::Uncertain);
    assert_eq!(episode.latest, first.checkpoint.episodes[0].latest);
    assert_eq!(episode.last_seen_at, first.checkpoint.episodes[0].last_seen_at);
    assert!(episode.recovery.is_none());
}

#[test]
fn a_native_lock_point_is_independent_of_threshold_policy_changes() {
    let first = observe(&frame(1, vec![lock_wait_actor()], true), None);
    let later = frame(2, vec![lock_wait_actor()], true);
    let rows = later
        .2
        .iter()
        .map(|(reference, observation)| DiagnosticActivityRow { reference, observation })
        .collect::<Vec<_>>();
    let policy = ActivityPolicy { blocked_after_ms: 90000, running_after_ms: 180000 };
    let update = observe_diagnostic_activity(
        &later.0,
        &later.1,
        &rows,
        Some(&first.checkpoint),
        &policy,
        "2020-01-01T01:00:00Z",
    )
    .unwrap();
    assert_eq!(update.checkpoint.episodes.len(), 1);
    assert_eq!(update.checkpoint.episodes[0].id, first.checkpoint.episodes[0].id);
    assert_eq!(update.checkpoint.episodes[0].lifecycle, DiagnosticLifecycle::Active);
    assert!(update.checkpoint.episodes[0].issue.is_none());
}

#[test]
fn recovery_keeps_original_evidence_and_pending_analysis_and_recurrence_is_a_new_episode() {
    let first = observe(&frame(1, vec![actor()], true), None);
    let recovered = observe(&frame(2, vec![], true), Some(&first.checkpoint));
    let episode = &recovered.checkpoint.episodes[0];
    assert_eq!(episode.lifecycle, DiagnosticLifecycle::Resolved);
    assert_eq!(episode.analysis, DiagnosticAnalysisStatus::Pending);
    assert_eq!(episode.first, first.checkpoint.episodes[0].first);
    assert!(episode.recovery.is_some());
    let recurrence = observe(&frame(3, vec![actor()], true), Some(&recovered.checkpoint));
    assert_eq!(recurrence.checkpoint.episodes.len(), 2);
    let active =
        recurrence.checkpoint.episodes.iter().find(|episode| episode.lifecycle == DiagnosticLifecycle::Active).unwrap();
    assert_ne!(active.id, episode.id);
}

#[test]
fn partial_absence_never_resolves_and_later_qualified_evidence_can_finish_recovery() {
    let first = observe(&frame(1, vec![actor()], true), None);
    let partial = observe(&frame(2, vec![], false), Some(&first.checkpoint));
    let episode = &partial.checkpoint.episodes[0];
    assert_eq!(episode.lifecycle, DiagnosticLifecycle::Uncertain);
    assert!(episode.recovery.is_none());
    assert_eq!(episode.last_seen_at, first.checkpoint.episodes[0].last_seen_at);
    let recovered = observe(&frame(3, vec![], true), Some(&partial.checkpoint));
    assert_eq!(recovered.checkpoint.episodes[0].lifecycle, DiagnosticLifecycle::Resolved);
}

#[test]
fn a_visible_actor_with_unknown_lock_details_cannot_resolve_a_prior_blocking_incident() {
    let mut blocked = actor();
    blocked.wait.as_mut().unwrap().identified_resources.clear();
    blocked
        .wait
        .as_mut()
        .unwrap()
        .owners
        .push(ActivityWaitOwner::Session { id: "52".into(), generation: Some("owner-generation".into()) });
    blocked.wait.as_mut().unwrap().elapsed_ms = Some("120000".into());
    let first = observe(&frame(1, vec![blocked.clone()], true), None);
    assert!(first.checkpoint.episodes.iter().any(|episode| matches!(
        episode.signal,
        DiagnosticSignal::Activity { condition: ActivityCondition::Blocked, .. }
    )));
    blocked.blocking_status = ActivityBlockingStatus::Unknown;
    blocked.wait = None;
    let later = observe(&frame(2, vec![blocked], true), Some(&first.checkpoint));
    assert!(
        later
            .checkpoint
            .episodes
            .iter()
            .all(|episode| episode.lifecycle == DiagnosticLifecycle::Uncertain && episode.recovery.is_none())
    );
}

#[test]
fn missing_statement_binding_retains_the_signal_without_authorizing_a_later_native_lookup() {
    let mut row = actor();
    row.statement_identity = None;
    let first = observe(&frame(1, vec![row], true), None);
    assert!(first.focuses.is_empty());
    assert_eq!(first.checkpoint.episodes[0].analysis, DiagnosticAnalysisStatus::Unresolved);
    assert_eq!(first.checkpoint.episodes[0].issue, Some(DiagnosticAnalysisIssue::MissingNativeIdentity));
}

#[test]
fn a_recycled_native_epoch_cannot_clear_an_older_episode() {
    let first = observe(&frame(1, vec![actor()], true), None);
    let mut later = frame(2, vec![], true);
    let SourceObservation::Activity(native) = &mut later.0.observation else { panic!() };
    native.epoch = Some("another-native-epoch".into());
    later.1.source_scope_id = later.0.captured_range().unwrap().scope_id;
    let next = observe(&later, Some(&first.checkpoint));
    assert_eq!(next.checkpoint.episodes[0].lifecycle, DiagnosticLifecycle::Uncertain);
    assert!(next.checkpoint.episodes[0].recovery.is_none());
}

#[test]
fn a_different_native_population_or_measurement_scope_cannot_resolve_an_older_actor() {
    let first = observe(&frame(1, vec![actor()], true), None);
    for change_population in [true, false] {
        let mut later = frame(2, vec![], true);
        let SourceObservation::Activity(native) = &mut later.0.observation else { panic!() };
        if change_population {
            native.population = ActivityPopulation::DefaultDatabaseConnections;
        } else {
            native.scope = MeasurementScope::Node;
        }
        later.1.source_scope_id = later.0.captured_range().unwrap().scope_id;
        assert_ne!(later.1.source_scope_id, first.checkpoint.episodes[0].first.source_scope_id);
        let update = observe(&later, Some(&first.checkpoint));
        assert_eq!(update.checkpoint.episodes[0].lifecycle, DiagnosticLifecycle::Uncertain);
        assert!(update.checkpoint.episodes[0].recovery.is_none());
    }
}

#[test]
fn a_different_native_row_cannot_borrow_an_authenticated_receipt() {
    let original = frame(1, vec![actor()], true);
    let mut forged = original.2[0].1.clone();
    forged.key.session.id = "99".into();
    let rows = [DiagnosticActivityRow { reference: &original.2[0].0, observation: &forged }];
    assert!(
        observe_diagnostic_activity(
            &original.0,
            &original.1,
            &rows,
            None,
            &ActivityPolicy::default(),
            "2020-01-01T01:00:00Z"
        )
        .is_err()
    );
}

#[test]
fn changing_a_threshold_keeps_the_old_policy_and_cannot_claim_native_recovery() {
    let mut running = actor();
    running.wait = None;
    running.elapsed_ms = Some("90000".into());
    let first = observe(&frame(1, vec![running.clone()], true), None);
    running.elapsed_ms = Some("120000".into());
    let later = frame(2, vec![running], true);
    let rows = later
        .2
        .iter()
        .map(|(reference, observation)| DiagnosticActivityRow { reference, observation })
        .collect::<Vec<_>>();
    let policy = ActivityPolicy { running_after_ms: 180000, ..Default::default() };
    let update = observe_diagnostic_activity(
        &later.0,
        &later.1,
        &rows,
        Some(&first.checkpoint),
        &policy,
        "2020-01-01T01:00:00Z",
    )
    .unwrap();
    assert_eq!(update.checkpoint.episodes.len(), 1);
    let episode = &update.checkpoint.episodes[0];
    assert_eq!(episode.lifecycle, DiagnosticLifecycle::Uncertain);
    assert_eq!(episode.issue, Some(DiagnosticAnalysisIssue::PolicyChanged));
    assert!(
        matches!(&episode.signal, DiagnosticSignal::Activity { policy, .. } if *policy == ActivityPolicy::default())
    );
    assert!(episode.recovery.is_none());
}

fn focus_frame(
    focus: ActivityWaitFocus,
    request: PageWaitRequestState,
    wait: PageWaitRelation,
    pages: Vec<PageWaitObservation>,
) -> (CollectionDescription, DiagnosticCaptureRef, Vec<(DiagnosticEvidenceRef, PageWaitObservation)>) {
    let (mut source, mut input, _) = frame(2, vec![], true);
    source.source = "page-wait-targets".into();
    source.observation = SourceObservation::FocusedPageWait {
        snapshot: StateSnapshotDescription {
            dataset: SnapshotDataset::PageWaitTargets,
            observed_from: "2020-01-01T00:02:00Z".into(),
            observed_until: "2020-01-01T00:02:02Z".into(),
            records: pages.len() as u32,
            sources: vec![StateSourceCoverage::observed("page-wait-targets".into(), pages.len() as u32, false)],
        },
        result: Box::new(ActivityWaitFocusResult {
            focus,
            observed_from: "2020-01-01T00:02:00Z".into(),
            observed_until: "2020-01-01T00:02:02Z".into(),
            source: FocusSourceState::Matched,
            request,
            wait,
        }),
    };
    input.source_scope_id = source.captured_range().unwrap().scope_id;
    let rows = pages
        .into_iter()
        .enumerate()
        .map(|(ordinal, page)| {
            let record = TelemetryRecord::Operational(OperationalObservation {
                source: page.source.clone(),
                key: page.key().unwrap(),
                fact: OperationalFact::PageWait(Box::new(page.clone())),
            });
            let exported = ExportedRecord::new(&source, &input.capture, record).unwrap();
            (
                DiagnosticEvidenceRef::from_export(&source, &input.manifest_sha256, &exported, ordinal as u32).unwrap(),
                page,
            )
        })
        .collect();
    (source, input, rows)
}

#[test]
fn a_focused_missing_actor_preserves_the_original_incident_and_records_an_unresolved_history() {
    let first = observe(&frame(1, vec![actor()], true), None);
    let focused =
        focus_frame(first.focuses[0].1.clone(), PageWaitRequestState::Missing, PageWaitRelation::Unqualified, vec![]);
    let update =
        analyze_diagnostic_focus(&focused.0, &focused.1, &[], &first.checkpoint, "2020-01-01T01:00:00Z").unwrap();
    assert!(update.checkpoint.episodes.is_empty(), "terminal historical evidence remains in its immutable revision");
    assert_eq!(update.revisions.len(), 1);
    let episode = &update.revisions[0].episode;
    assert_eq!(episode.first, first.checkpoint.episodes[0].first);
    assert_eq!(episode.last_seen_at, first.checkpoint.episodes[0].last_seen_at);
    assert_eq!(episode.lifecycle, DiagnosticLifecycle::Resolved);
    assert_eq!(episode.analysis, DiagnosticAnalysisStatus::Unresolved);
    assert_eq!(episode.issue, Some(DiagnosticAnalysisIssue::NativeContextGone));
    assert!(episode.recovery.as_ref().unwrap().matches_source(episode, &focused.0, &focused.1));
    assert!(episode.report.is_none());
}

#[test]
fn a_later_focused_frame_keeps_its_own_counters_and_never_manufactures_an_owner() {
    let first = observe(&frame(1, vec![actor()], true), None);
    let focus = first.focuses[0].1.clone();
    let mut request = focus.original.clone();
    request.elapsed_ms = Some("121000".into());
    request.session_counters.logical_reads = Some("99".into());
    let page = PageWaitObservation {
        source: "page-wait-targets".into(),
        observed_from: "2020-01-01T00:02:00Z".into(),
        observed_until: "2020-01-01T00:02:01Z".into(),
        request,
        before_check: None,
        after_check: None,
        statement_identity: None,
        plan_identity: None,
        resource: focus.resource.clone(),
        observed_address: None,
        resolution: PageWaitResolution::Unavailable,
        target: None,
        index_operations: None,
    };
    let focused =
        focus_frame(focus.clone(), PageWaitRequestState::SameStatement, PageWaitRelation::OriginalObserved, vec![page]);
    let rows = focused
        .2
        .iter()
        .map(|(reference, observation)| DiagnosticFocusRow { reference, observation })
        .collect::<Vec<_>>();
    let update =
        analyze_diagnostic_focus(&focused.0, &focused.1, &rows, &first.checkpoint, "2020-01-01T01:00:00Z").unwrap();
    let episode = &update.checkpoint.episodes[0];
    assert_eq!(episode.analysis, DiagnosticAnalysisStatus::Partial);
    assert_eq!(episode.issue, Some(DiagnosticAnalysisIssue::EvidenceUnavailable));
    assert_eq!(episode.first, first.checkpoint.episodes[0].first);
    assert_eq!(focus.original.elapsed_ms.as_deref(), Some("1"));
    assert_eq!(episode.report.as_ref().unwrap().evidence(), &focused.2[0].0);
    let mut substituted = focused.2[0].1.clone();
    substituted.request.statement_identity = Some(NativeIdentity::new("different-statement"));
    let rows = [DiagnosticFocusRow { reference: &focused.2[0].0, observation: &substituted }];
    assert!(
        analyze_diagnostic_focus(&focused.0, &focused.1, &rows, &first.checkpoint, "2020-01-01T01:00:00Z").is_err()
    );
}
