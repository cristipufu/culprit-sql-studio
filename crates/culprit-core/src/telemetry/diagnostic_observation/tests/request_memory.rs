use super::*;

fn memory_actor() -> ActivityObservation {
    let mut row = actor();
    row.blocking_status = ActivityBlockingStatus::Unknown;
    let wait = row.wait.as_mut().unwrap();
    wait.class = Some(ActivityWaitClass::ExecutionGrant);
    wait.native_type = Some("source-owned-execution-grant-wait".into());
    wait.identified_resources.clear();
    row
}

pub(super) fn memory_frame(
    sequence: u64,
    rows: Vec<ActivityObservation>,
    complete: bool,
) -> (CollectionDescription, DiagnosticCaptureRef, Vec<(DiagnosticEvidenceRef, ActivityObservation)>) {
    let (mut source, mut input, rows) = super::frame(sequence, rows, complete);
    source.partition = SourcePartition::InstanceLocal {
        member: Some(SharedResourceIdentity {
            engine: source.target.engine(),
            kind: SharedResourceKind::Node,
            namespace: NativeIdentity::new("synthetic-logical-database"),
            native: NativeIdentity::new("synthetic-native-member"),
        }),
        epoch: source.observation.activity().unwrap().epoch.clone(),
    };
    input.source_scope_id = source.captured_range().unwrap().scope_id;
    let rows = rows
        .into_iter()
        .enumerate()
        .map(|(ordinal, (_, observation))| {
            let row = ExportedRecord::new(&source, &input.capture, TelemetryRecord::from(observation.clone())).unwrap();
            (
                DiagnosticEvidenceRef::from_export(&source, &input.manifest_sha256, &row, ordinal as u32).unwrap(),
                observation,
            )
        })
        .collect();
    (source, input, rows)
}

fn focus(update: &DiagnosticObservationUpdate) -> ActivityRequestFocus {
    let DiagnosticFocus::RequestMemory { focus } = &update.focuses[0].1 else { panic!() };
    focus.as_ref().clone()
}

fn grant(focus: &ActivityRequestFocus) -> ExecutionGrantObservation {
    ExecutionGrantObservation {
        source: crate::source_id::CURRENT_REQUEST_MEMORY.into(),
        identity: NativeIdentity::new("native-grant").qualified("request-time", "2020-01-01T00:01:01"),
        observed_at: "2020-01-01T00:02:00Z".into(),
        state: ExecutionGrantState::Waiting,
        request_association: RequestGrantAssociation::GenerationChecked,
        statement_relationship: GrantStatementRelationship::Unresolved,
        request: Some(focus.original.clone()),
        scope: Some(MeasurementScope::Database),
        requested_without_offset: "2020-01-01T00:01:01".into(),
        granted_without_offset: None,
        requested_kibibytes: Some("9007199254740993".into()),
        required_kibibytes: Some("64".into()),
        granted_kibibytes: None,
        used_kibibytes: None,
        maximum_used_kibibytes: None,
        ideal_kibibytes: None,
        wait_ms: Some("40000".into()),
        timeout_seconds: Some("25".into()),
        degree_of_parallelism: None,
        reserved_workers: None,
        used_workers: None,
        maximum_used_workers: None,
        resource: None,
        resource_observation: None,
        workload_group: None,
        queue: ExecutionGrantQueue::default(),
        native_properties: Default::default(),
        available_columns: vec![],
        native_null_columns: vec![],
    }
}

fn captured(
    focus: ActivityRequestFocus,
    membership: RequestMemoryMembership,
) -> (CollectionDescription, DiagnosticCaptureRef, Vec<(DiagnosticEvidenceRef, ExecutionGrantObservation)>) {
    let value = grant(&focus);
    let count = u32::from(membership == RequestMemoryMembership::Observed);
    let source = CollectionDescription {
        target: focus.target.clone(),
        partition: focus.partition.clone(),
        source: value.source.clone(),
        continuity: SourceContinuity::Unverified,
        observation: SourceObservation::FocusedRequestMemory {
            snapshot: StateSnapshotDescription {
                dataset: SnapshotDataset::CurrentRequestMemory,
                observed_from: value.observed_at.clone(),
                observed_until: value.observed_at.clone(),
                records: count,
                sources: vec![StateSourceCoverage::observed(value.source.clone(), count, true)],
            },
            result: Box::new(ActivityRequestMemoryFocusResult {
                focus,
                observed_from: value.observed_at.clone(),
                observed_until: value.observed_at.clone(),
                source: FocusSourceState::Matched,
                membership,
                subject: (membership == RequestMemoryMembership::Observed).then(|| value.identity.clone()),
                resource: None,
                peers: vec![],
            }),
        },
    };
    let input = DiagnosticCaptureRef {
        capture: CaptureIdentity {
            observation_id: "grant-result".into(),
            revision: Some(CaptureRevision {
                outbox_id: "owned-outbox".into(),
                owner_epoch: "1".into(),
                sequence: "20".into(),
            }),
        },
        manifest_sha256: digest(b"sealed grant result"),
        source_scope_id: source.captured_range().unwrap().scope_id,
    };
    let rows = references(&source, &input, if count == 0 { vec![] } else { vec![value] });
    (source, input, rows)
}

fn references(
    source: &CollectionDescription,
    input: &DiagnosticCaptureRef,
    observations: Vec<ExecutionGrantObservation>,
) -> Vec<(DiagnosticEvidenceRef, ExecutionGrantObservation)> {
    observations
        .into_iter()
        .enumerate()
        .map(|(ordinal, value)| {
            let row = ExportedRecord::new(
                source,
                &input.capture,
                TelemetryRecord::Operational(OperationalObservation {
                    source: value.source.clone(),
                    key: value.key().unwrap(),
                    fact: OperationalFact::ExecutionGrant(Box::new(value.clone())),
                }),
            )
            .unwrap();
            (DiagnosticEvidenceRef::from_export(source, &input.manifest_sha256, &row, ordinal as u32).unwrap(), value)
        })
        .collect()
}

fn with_peers(
    original: ActivityRequestFocus,
    peers: usize,
) -> (CollectionDescription, DiagnosticCaptureRef, Vec<(DiagnosticEvidenceRef, ExecutionGrantObservation)>) {
    let (mut source, mut input, rows) = captured(original, RequestMemoryMembership::Observed);
    let mut subject = rows[0].1.clone();
    subject.resource = Some(NativeIdentity::new("native-resource").qualified("semaphore", "0"));
    let mut observations = (0..peers)
        .map(|index| {
            let mut peer = subject.clone();
            peer.identity = NativeIdentity::new(format!("grant-peer-{index}"));
            let request = peer.request.as_mut().unwrap();
            request.key.session.id = format!("peer-{index}");
            request.statement_identity = Some(NativeIdentity::new(format!("native-statement-{index}")));
            peer.state = ExecutionGrantState::Granted;
            peer.granted_without_offset = Some("2020-01-01T00:01:01.003".into());
            peer.granted_kibibytes = Some("64".into());
            peer
        })
        .collect::<Vec<_>>();
    let SourceObservation::FocusedRequestMemory { result, snapshot } = &mut source.observation else { panic!() };
    result.resource = subject.resource.clone();
    result.peers = observations.iter().map(|row| row.identity.clone()).collect();
    observations.push(subject);
    snapshot.records = observations.len() as u32;
    snapshot.sources[0] = StateSourceCoverage::observed(source.source.clone(), snapshot.records, true);
    input.source_scope_id = source.captured_range().unwrap().scope_id;
    let rows = references(&source, &input, observations);
    (source, input, rows)
}

#[test]
fn a_typed_execution_grant_wait_creates_a_question_without_parsing_native_wait_names_or_request_age() {
    let baseline = memory_frame(1, vec![memory_actor()], false);
    let observed = observe(&baseline, None);
    assert_eq!(observed.checkpoint.episodes.len(), 1);
    assert!(matches!(observed.checkpoint.episodes[0].signal, DiagnosticSignal::RequestMemory { .. }));
    assert_eq!(focus(&observed).original, baseline.2[0].1);
    let mut unclassified = memory_actor();
    let wait = unclassified.wait.as_mut().unwrap();
    wait.class = None;
    wait.native_type = Some("RESOURCE_SEMAPHORE".into());
    assert!(observe(&memory_frame(1, vec![unclassified], true), None).checkpoint.episodes.is_empty());
}

#[test]
fn an_exact_waiting_or_granted_point_explains_the_original_request_without_claiming_recovery_or_an_owner() {
    let observed = observe(&memory_frame(1, vec![memory_actor()], true), None);
    for granted in [false, true] {
        let (source, input, mut rows) = captured(focus(&observed), RequestMemoryMembership::Observed);
        if granted {
            // This is a separately attested later native state, not a reason inferred for the prior wait.
            rows[0].1.state = ExecutionGrantState::Granted;
            rows[0].1.granted_without_offset = Some("2020-01-01T00:01:02".into());
            let exported = ExportedRecord::new(
                &source,
                &input.capture,
                TelemetryRecord::Operational(OperationalObservation {
                    source: rows[0].1.source.clone(),
                    key: rows[0].1.key().unwrap(),
                    fact: OperationalFact::ExecutionGrant(Box::new(rows[0].1.clone())),
                }),
            )
            .unwrap();
            rows[0].0 = DiagnosticEvidenceRef::from_export(&source, &input.manifest_sha256, &exported, 0).unwrap();
        }
        let native = rows
            .iter()
            .map(|(reference, observation)| DiagnosticMemoryRow { reference, observation })
            .collect::<Vec<_>>();
        let update =
            analyze_diagnostic_memory(&source, &input, &native, &observed.checkpoint, "2020-01-01T01:00:00Z", None)
                .unwrap();
        let episode = &update.revisions[0].episode;
        assert_eq!(episode.lifecycle, DiagnosticLifecycle::Active);
        assert_eq!(episode.first, observed.checkpoint.episodes[0].first);
        assert_eq!(episode.latest, observed.checkpoint.episodes[0].latest);
        assert_eq!(episode.analysis, DiagnosticAnalysisStatus::Analyzed);
        assert!(matches!(episode.report, Some(DiagnosticReport::RequestMemory { .. })));
        assert!(episode.recovery.is_none());
        assert!(update.revisions[0].inputs.contains(&focus(&observed).parent));
    }
}

#[test]
fn no_matching_or_unverified_grant_never_proves_the_original_request_disappeared() {
    let observed = observe(&memory_frame(1, vec![memory_actor()], true), None);
    for membership in [RequestMemoryMembership::NotObserved, RequestMemoryMembership::Unverified] {
        let (source, input, _) = captured(focus(&observed), membership);
        let update =
            analyze_diagnostic_memory(&source, &input, &[], &observed.checkpoint, "2020-01-01T01:00:00Z", None)
                .unwrap();
        let episode = &update.revisions[0].episode;
        assert_eq!(episode.lifecycle, DiagnosticLifecycle::Active);
        assert_eq!(episode.analysis, DiagnosticAnalysisStatus::Unresolved);
        assert_eq!(episode.issue, Some(DiagnosticAnalysisIssue::EvidenceUnavailable));
        assert!(episode.recovery.is_none() && episode.report.is_none());
    }
}

#[test]
fn the_original_subject_is_found_by_native_identity_and_every_peer_receipt_enters_history() {
    let baseline = observe(&memory_frame(1, vec![memory_actor()], true), None);
    let (source, input, rows) = with_peers(focus(&baseline), 199);
    let native =
        rows.iter().map(|(reference, observation)| DiagnosticMemoryRow { reference, observation }).collect::<Vec<_>>();
    let update =
        analyze_diagnostic_memory(&source, &input, &native, &baseline.checkpoint, "2020-01-01T01:00:00Z", None)
            .unwrap();
    let revision = &update.revisions[0];
    let DiagnosticReport::RequestMemory { observation, peers } = revision.episode.report.as_ref().unwrap() else {
        panic!()
    };
    assert_eq!(*observation, rows.last().unwrap().0, "the first row is a peer, not a representative original");
    assert_eq!(peers.len(), 199);
    assert!(
        revision.episode.report.as_ref().unwrap().references().all(|reference| revision.inputs.contains(reference))
    );
    assert!(revision.inputs.len() <= MAX_DIAGNOSTIC_INPUTS);
    assert!(revision.is_valid());
    assert!(serde_json::to_vec(&source).unwrap().len() < 128 * 1024);
    assert_eq!(revision.episode.lifecycle, baseline.checkpoint.episodes[0].lifecycle);
    assert_eq!(revision.episode.latest, baseline.checkpoint.episodes[0].latest);
}

#[test]
fn a_larger_plugin_roster_preserves_raw_evidence_and_records_explicit_analysis_budget_exhaustion() {
    let baseline = observe(&memory_frame(1, vec![memory_actor()], true), None);
    let (source, input, rows) = with_peers(focus(&baseline), MAX_DIAGNOSTIC_INPUTS);
    let raw = rows.clone();
    let native =
        rows.iter().map(|(reference, observation)| DiagnosticMemoryRow { reference, observation }).collect::<Vec<_>>();
    let update =
        analyze_diagnostic_memory(&source, &input, &native, &baseline.checkpoint, "2020-01-01T01:00:00Z", None)
            .unwrap();
    let revision = &update.revisions[0];
    assert_eq!(revision.episode.analysis, DiagnosticAnalysisStatus::Unresolved);
    assert_eq!(revision.episode.issue, Some(DiagnosticAnalysisIssue::BudgetExhausted));
    assert!(revision.episode.report.is_none());
    assert_eq!(rows, raw);
    assert!(revision.is_valid());
    assert_eq!(revision.episode.first, baseline.checkpoint.episodes[0].first);
}

#[test]
fn peer_only_missing_duplicate_foreign_clock_source_or_resource_cannot_close_a_focused_input() {
    let baseline = observe(&memory_frame(1, vec![memory_actor()], true), None);
    let (source, _, rows) = with_peers(focus(&baseline), 1);
    let SourceObservation::FocusedRequestMemory { result, .. } = &source.observation else { panic!() };
    assert!(result.matches_observations(rows.iter().map(|(_, row)| row)));
    assert!(!result.matches_observations([&rows[0].1]));
    assert!(!result.matches_observations([&rows[1].1]));
    assert!(!result.matches_observations([&rows[0].1, &rows[0].1, &rows[1].1]));
    for mismatch in ["resource", "clock", "source", "database", "generation", "token"] {
        let mut peer = rows[0].1.clone();
        match mismatch {
            "resource" => peer.resource = Some(NativeIdentity::new("other-resource")),
            "clock" => peer.observed_at = "2020-01-01T00:02:01Z".into(),
            "source" => peer.source = "other-native-source".into(),
            "database" => peer.request.as_mut().unwrap().database = Some("other-database".into()),
            "generation" => {
                peer.request_association = RequestGrantAssociation::ChangedRequest;
                peer.request = None;
                peer.scope = None;
            }
            "token" => peer.request.as_mut().unwrap().statement_identity = None,
            _ => unreachable!(),
        }
        assert!(peer.is_valid(), "{mismatch}");
        assert!(!result.matches_observations([&peer, &rows[1].1]), "{mismatch}");
    }
}

#[test]
fn a_restored_resolved_but_owed_episode_can_reconstruct_its_exact_middle_parent() {
    let first = observe(&memory_frame(1, vec![memory_actor()], true), None);
    let middle = memory_frame(2, vec![memory_actor()], false);
    let second = observe(&middle, Some(&first.checkpoint));
    let third = observe(&memory_frame(3, vec![memory_actor()], true), Some(&second.checkpoint));
    let recovered = observe(&memory_frame(4, vec![], true), Some(&third.checkpoint));
    let restored: DiagnosticCheckpoint =
        serde_json::from_slice(&serde_json::to_vec(&recovered.checkpoint).unwrap()).unwrap();
    let episode = &restored.episodes[0];
    assert_eq!(episode.lifecycle, DiagnosticLifecycle::Resolved);
    let DiagnosticFocusPlan::Ready(question) = episode.plan_focus(&middle.0, &middle.2[0].1, &middle.2[0].0).unwrap()
    else {
        panic!()
    };
    assert_eq!(question.activity_participant().unwrap().parent, middle.2[0].0);
    let (source, input, _) =
        captured(question.activity_participant().unwrap().clone(), RequestMemoryMembership::NotObserved);
    let update = analyze_diagnostic_memory(&source, &input, &[], &restored, "2020-01-01T01:00:00Z", None).unwrap();
    assert_eq!(update.revisions[0].episode.lifecycle, DiagnosticLifecycle::Resolved);
    assert_eq!(update.revisions[0].episode.recovery, episode.recovery);
}

#[test]
fn a_grant_with_another_generation_or_statement_cannot_bind_even_with_the_same_session_request_numbers() {
    let observed = observe(&memory_frame(1, vec![memory_actor()], true), None);
    let (source, input, rows) = captured(focus(&observed), RequestMemoryMembership::Observed);
    let SourceObservation::FocusedRequestMemory { result, .. } = &source.observation else { panic!() };
    for changed in ["session", "request", "statement", "database", "scope", "clock"] {
        let mut value = rows[0].1.clone();
        match changed {
            "session" => value.request.as_mut().unwrap().key.session.generation = "recycled".into(),
            "request" => {
                value.request.as_mut().unwrap().key.request.as_mut().unwrap().generation = "new-request".into()
            }
            "statement" => {
                value.request.as_mut().unwrap().statement_identity = Some(NativeIdentity::new("another-statement"))
            }
            "database" => value.request.as_mut().unwrap().database = Some("foreign-database".into()),
            "scope" => {
                value.request_association = RequestGrantAssociation::InsufficientEvidence;
                value.request = None;
                value.scope = None;
            }
            "clock" => value.observed_at = "2020-01-01T00:03:00Z".into(),
            _ => unreachable!(),
        }
        assert!(!result.accepts_observation(&value), "{changed}");
        assert!(
            ExportedRecord::new(
                &source,
                &input.capture,
                TelemetryRecord::Operational(OperationalObservation {
                    source: value.source.clone(),
                    key: value.key().unwrap(),
                    fact: OperationalFact::ExecutionGrant(Box::new(value))
                })
            )
            .is_err()
        );
    }
    let mut unknown_label = rows[0].1.clone();
    unknown_label.request.as_mut().unwrap().database = None;
    assert!(
        result.accepts_observation(&unknown_label),
        "the native request-database scope remains usable without a display label"
    );
    assert!(
        ExportedRecord::new(
            &source,
            &input.capture,
            TelemetryRecord::Operational(OperationalObservation {
                source: unknown_label.source.clone(),
                key: unknown_label.key().unwrap(),
                fact: OperationalFact::ExecutionGrant(Box::new(unknown_label)),
            })
        )
        .is_ok()
    );
}

#[test]
fn a_logical_target_without_a_native_member_cannot_authorize_a_memory_followup() {
    let unqualified = super::frame(1, vec![memory_actor()], true);
    let update = observe(&unqualified, None);
    assert!(update.focuses.is_empty());
    let episode = &update.checkpoint.episodes[0];
    assert_eq!(episode.issue, Some(DiagnosticAnalysisIssue::MissingNativeIdentity));
    assert_eq!(episode.analysis, DiagnosticAnalysisStatus::Unresolved);
    assert!(matches!(
        episode.plan_focus(&unqualified.0, &unqualified.2[0].1, &unqualified.2[0].0).unwrap(),
        DiagnosticFocusPlan::NotRequired
    ));
    let pending = DiagnosticEpisode::start(
        episode.scope.clone(),
        episode.signal.clone(),
        episode.first_seen_at.clone(),
        episode.first.clone(),
    )
    .unwrap();
    assert!(matches!(
        pending.plan_focus(&unqualified.0, &unqualified.2[0].1, &unqualified.2[0].0).unwrap(),
        DiagnosticFocusPlan::Unavailable(DiagnosticAnalysisIssue::MissingNativeIdentity)
    ));
}
