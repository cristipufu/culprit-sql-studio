use super::*;

fn reference(id: &str) -> DiagnosticEvidenceRef {
    let capture = CaptureIdentity {
        observation_id: id.into(),
        revision: Some(CaptureRevision { outbox_id: "owner".into(), owner_epoch: "1".into(), sequence: "1".into() }),
    };
    DiagnosticEvidenceRef {
        batch_id: batch_id(id, TelemetryStream::ActivityObservations).unwrap(),
        capture,
        manifest_sha256: digest(b"original manifest"),
        source_scope_id: digest(b"native scope"),
        stream: TelemetryStream::ActivityObservations,
        native_key: NativeRecordIdentity::Activity(ActivityKey {
            session: NativeActivityIdentity { id: "42".into(), generation: "login".into() },
            request: Some(NativeActivityIdentity { id: "0".into(), generation: "request-start".into() }),
            transaction: None,
        }),
        record_id: digest(b"row"),
        payload_sha256: digest(b"payload"),
        representation_version: REPRESENTATION_VERSION,
        row_ordinal: 0,
    }
}
fn page_signal(reference: &DiagnosticEvidenceRef, resource: &str) -> DiagnosticSignal {
    let NativeRecordIdentity::Activity(key) = &reference.native_key else { panic!() };
    DiagnosticSignal::PageWait {
        key: key.clone(),
        statement_identity: None,
        resource: crate::ObservedWaitResource {
            kind: crate::WaitResourceKind::MemoryPage,
            native: NativeIdentity::new(resource),
        },
    }
}
fn episode() -> DiagnosticEpisode {
    DiagnosticEpisode::start(
        DiagnosticScope {
            target: crate::DatabaseIdentity::server(crate::Engine::Mssql, "db", "fixture.invalid", 1433, None).into(),
            source: "activity".into(),
            native_epoch: Some("native-start".into()),
            population: ActivityPopulation::ActiveRequestsOpenTransactionsAndBlockers,
            measurement_scope: crate::MeasurementScope::Database,
        },
        page_signal(&reference("baseline"), "5:1:42"),
        "2026-11-14T08:00:00Z".into(),
        reference("baseline"),
    )
    .unwrap()
}
fn revision() -> DiagnosticRevision {
    let episode = episode();
    let mut revision = DiagnosticRevision {
        id: String::new(),
        controller_version: 1,
        inputs: vec![episode.latest.clone()],
        episode,
        previous_revision: None,
        analyzed_at: "2026-11-14T08:01:00Z".into(),
    };
    revision.id = revision.expected_id().unwrap();
    revision
}
#[test]
fn row_receipt_identity_preserves_ordinal_generation_and_original_manifest() {
    let row = reference("baseline");
    assert!(row.is_valid());
    let key = row.key().unwrap();
    let mut changed = row.clone();
    changed.row_ordinal = 1;
    assert_ne!(changed.key().unwrap(), key);
    changed = row.clone();
    changed.manifest_sha256 = digest(b"different original manifest");
    assert_ne!(changed.key().unwrap(), key);
    changed = row.clone();
    changed.capture.revision.as_mut().unwrap().owner_epoch = "01".into();
    assert!(!changed.is_valid());
    changed = row.clone();
    changed.stream = TelemetryStream::OperationalSamples;
    assert!(!changed.is_valid());
    changed = row;
    changed.native_key = NativeRecordIdentity::Activity(ActivityKey {
        session: NativeActivityIdentity { id: "42".into(), generation: "".into() },
        request: None,
        transaction: None,
    });
    assert!(!changed.is_valid());
}
#[test]
fn resolution_and_analysis_are_independent_and_missing_scope_cannot_prove_recovery() {
    let mut episode = episode();
    assert_eq!(episode.analysis, DiagnosticAnalysisStatus::Pending);
    assert!(episode.retained_in_checkpoint());
    episode.lifecycle = DiagnosticLifecycle::Resolved;
    assert!(!episode.is_valid());
    episode.recovery =
        Some(DiagnosticRecoveryProof::CompleteScope { capture: reference("complete-empty").capture_ref() });
    assert!(episode.is_valid());
    assert!(episode.retained_in_checkpoint(), "recovered-before-analysis work remains durable");
    episode.analysis = DiagnosticAnalysisStatus::Unresolved;
    episode.issue = Some(DiagnosticAnalysisIssue::EvidenceUnavailable);
    assert!(!episode.retained_in_checkpoint());
    let scope = episode.scope.clone();
    episode.scope.native_epoch = None;
    assert!(!episode.is_valid());
    for state in [SourceState::Partial, SourceState::Unavailable, SourceState::NotApplicable] {
        assert!(!diagnostic_baseline_can_resolve(&scope, state, false));
    }
    assert!(diagnostic_baseline_can_resolve(&scope, SourceState::Empty, true));
    assert!(!diagnostic_baseline_can_resolve(&scope, SourceState::Collected, false));
}
#[test]
fn analysis_success_requires_a_durable_typed_result_and_contiguous_revision() {
    let mut row = revision();
    assert!(row.is_valid());
    row.episode.analysis = DiagnosticAnalysisStatus::Analyzed;
    row.id = row.expected_id().unwrap();
    assert!(!row.is_valid());
    let mut result = reference("detail");
    result.stream = TelemetryStream::OperationalSamples;
    result.batch_id = batch_id("detail", result.stream).unwrap();
    result.native_key = NativeRecordIdentity::Operational { source: "page wait targets".into(), key: digest(b"page") };
    row.episode.report = Some(DiagnosticReport::PageWait { observation: result.clone() });
    row.id = row.expected_id().unwrap();
    assert!(!row.is_valid(), "result must be one of the exact input receipts");
    row.inputs.push(result);
    row.id = row.expected_id().unwrap();
    assert!(row.is_valid());
    row.episode.revision = "3".into();
    row.previous_revision = Some("1".into());
    row.id = row.expected_id().unwrap();
    assert!(!row.is_valid());
    row.previous_revision = Some("2".into());
    row.id = row.expected_id().unwrap();
    assert!(row.is_valid());
}
#[test]
fn a_checkpoint_preserves_distinct_pending_episodes_and_rejects_other_targets_or_duplicates() {
    let first = episode();
    let mut second = first.clone();
    second.signal = page_signal(&second.first, "5:1:43");
    second.id = second.expected_id().unwrap();
    let mut checkpoint = DiagnosticCheckpoint {
        controller_version: 1,
        target: first.scope.target.clone(),
        input: first.first.capture_ref(),
        episodes: vec![first.clone(), second],
    };
    assert!(checkpoint.is_valid());
    assert_eq!(checkpoint.episodes.len(), 2);
    checkpoint.episodes.push(first);
    assert!(!checkpoint.is_valid());
    checkpoint.episodes.pop();
    checkpoint.target =
        crate::DatabaseIdentity::server(crate::Engine::Mssql, "other", "fixture.invalid", 1433, None).into();
    assert!(!checkpoint.is_valid());
}
#[test]
fn immutable_revision_identity_does_not_change_with_retry_clock_or_rendering_language() {
    let row = revision();
    let mut retried = row.clone();
    retried.analyzed_at = "2026-11-14T08:03:00Z".into();
    assert_eq!(retried.expected_id().unwrap(), row.id);
    assert!(retried.is_valid());
    let encoded = serde_json::to_string(&row).unwrap();
    assert_eq!(serde_json::from_str::<DiagnosticRevision>(&encoded).unwrap(), row);
    assert!(!encoded.contains("SELECT"));
}

#[test]
fn segmented_checkpoint_requires_every_part_exactly_once_and_never_drops_an_episode() {
    let first = episode();
    let mut other = first.clone();
    other.signal = page_signal(&other.first, "5:1:44");
    other.id = other.expected_id().unwrap();
    let mut first = DiagnosticCheckpointPart {
        id: String::new(),
        controller_version: 1,
        target: first.scope.target.clone(),
        input: first.first.capture_ref(),
        part: 0,
        parts: 2,
        episodes: vec![first],
    };
    first.id = first.expected_id().unwrap();
    let mut second = first.clone();
    second.part = 1;
    second.id = second.expected_id().unwrap();
    second.episodes = vec![other];
    assert_ne!(first.key().unwrap(), second.key().unwrap());
    assert_eq!(DiagnosticCheckpointPart::assemble(vec![second.clone(), first.clone()]).unwrap().episodes.len(), 2);
    assert!(DiagnosticCheckpointPart::assemble(vec![first.clone()]).is_err());
    assert!(DiagnosticCheckpointPart::assemble(vec![first.clone(), first.clone()]).is_err());
    second.input.manifest_sha256 = digest(b"another input");
    assert!(DiagnosticCheckpointPart::assemble(vec![first, second]).is_err());
}

#[test]
fn a_start_does_not_invent_a_peak_and_deferred_work_requires_a_machine_reason() {
    let mut pending = episode();
    assert!(pending.peak.is_none());
    pending.analysis = DiagnosticAnalysisStatus::Deferred;
    assert!(!pending.is_valid());
    pending.issue = Some(DiagnosticAnalysisIssue::CostUnqualified);
    assert!(pending.is_valid());
    pending.analysis = DiagnosticAnalysisStatus::Unresolved;
    pending.issue = None;
    assert!(!pending.is_valid());
    pending.issue = Some(DiagnosticAnalysisIssue::MissingNativeIdentity);
    assert!(pending.is_valid());
}

fn focused_episode() -> (DiagnosticEpisode, ActivityWaitFocus) {
    let mut episode = episode();
    let mut original = super::super::activity::tests::request("42");
    let NativeRecordIdentity::Activity(key) = &episode.first.native_key else { panic!() };
    original.key = key.clone();
    original.database = Some("db".into());
    let resource = crate::ObservedWaitResource {
        kind: crate::WaitResourceKind::MemoryPage,
        native: NativeIdentity::new("5:1:42"),
    };
    let token = NativeIdentity::new("exact-native-token");
    let DiagnosticSignal::PageWait { statement_identity, .. } = &mut episode.signal else { panic!() };
    *statement_identity = Some(token.clone());
    episode.id = episode.expected_id().unwrap();
    original.statement_identity = Some(token.clone());
    original.wait = Some(ActivityWait {
        identified_resources: vec![resource.clone()],
        native_type: Some("PAGELATCH_EX".into()),
        elapsed_ms: Some("1000".into()),
        owners: vec![],
        resources: None,
    });
    let focus = ActivityWaitFocus {
        parent: episode.first.clone(),
        target: episode.scope.target.clone(),
        source: episode.scope.source.clone(),
        epoch: episode.scope.native_epoch.clone().unwrap(),
        original_observed_at: episode.first_seen_at.clone(),
        original,
        statement_identity: token,
        resource,
    };
    (episode, focus)
}

#[test]
fn an_episode_focus_binding_requires_the_same_native_actor_statement_resource_scope_and_clock() {
    let (episode, focus) = focused_episode();
    assert!(episode.matches_focus(&focus));
    let mut other_actor = focus.clone();
    other_actor.original.key.session.id = "43".into();
    other_actor.parent.native_key = NativeRecordIdentity::Activity(other_actor.original.key.clone());
    let mut other_statement = focus.clone();
    other_statement.statement_identity = NativeIdentity::new("another-native-token");
    other_statement.original.statement_identity = Some(other_statement.statement_identity.clone());
    let mut other_resource = focus.clone();
    other_resource.resource.native = NativeIdentity::new("5:1:43");
    other_resource.original.wait.as_mut().unwrap().identified_resources = vec![other_resource.resource.clone()];
    let mut other_scope = focus.clone();
    other_scope.parent.source_scope_id = digest(b"another-native-scope");
    let mut other_epoch = focus.clone();
    other_epoch.epoch = "another-native-epoch".into();
    let mut other_source = focus.clone();
    other_source.source = "another-native-source".into();
    let mut outside_episode = focus;
    outside_episode.original_observed_at = "2026-11-14T08:00:01Z".into();
    for changed in
        [other_actor, other_statement, other_resource, other_scope, other_epoch, other_source, outside_episode]
    {
        assert!(changed.is_valid(), "the other native question remains independently valid");
        assert!(!episode.matches_focus(&changed));
    }
}

#[test]
fn only_matched_exact_focused_missing_can_resolve_a_participant_without_complete_inventory() {
    let (mut episode, focus) = focused_episode();
    let result = ActivityWaitFocusResult {
        lineage: None,
        focus,
        observed_from: "2026-11-14T08:01:00Z".into(),
        observed_until: "2026-11-14T08:01:01Z".into(),
        source: FocusSourceState::Matched,
        request: PageWaitRequestState::Missing,
        wait: PageWaitRelation::Unqualified,
    };
    let source = CollectionDescription {
        target: episode.scope.target.clone(),
        source: "focused-page".into(),
        continuity: SourceContinuity::Unverified,
        observation: SourceObservation::FocusedPageWait {
            snapshot: StateSnapshotDescription {
                dataset: SnapshotDataset::PageWaitTargets,
                observed_from: result.observed_from.clone(),
                observed_until: result.observed_until.clone(),
                records: 0,
                sources: vec![StateSourceCoverage::not_applicable("page wait targets".into())],
            },
            result: Box::new(result),
        },
    };
    let mut capture = reference("focused-gone").capture_ref();
    capture.source_scope_id = source.captured_range().unwrap().scope_id;
    let proof = DiagnosticRecoveryProof::FocusedParticipantGone {
        capture: capture.clone(),
        parent: Box::new(episode.first.clone()),
    };
    assert!(proof.matches_source(&episode, &source, &capture));
    episode.lifecycle = DiagnosticLifecycle::Resolved;
    episode.recovery = Some(proof.clone());
    assert!(episode.is_valid());
    for state in [FocusSourceState::Changed, FocusSourceState::Unverified] {
        let mut changed = source.clone();
        let SourceObservation::FocusedPageWait { result, .. } = &mut changed.observation else { panic!() };
        result.source = state;
        result.request = PageWaitRequestState::Unverified;
        assert!(!proof.matches_source(&episode, &changed, &capture));
    }
    let mut changed = source.clone();
    let SourceObservation::FocusedPageWait { result, .. } = &mut changed.observation else { panic!() };
    result.focus.epoch = "restarted".into();
    assert!(!proof.matches_source(&episode, &changed, &capture));
}
