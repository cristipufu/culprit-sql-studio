use super::*;
pub(super) fn first() -> DiagnosticRevision {
    let reference = DiagnosticEvidenceRef {
        capture: CaptureIdentity {
            observation_id: "native-first".into(),
            revision: Some(CaptureRevision {
                outbox_id: "owner".into(),
                owner_epoch: "1".into(),
                sequence: "1".into(),
            }),
        },
        batch_id: batch_id("native-first", TelemetryStream::ActivityObservations).unwrap(),
        manifest_sha256: digest(b"manifest"),
        source_scope_id: digest(b"native-scope"),
        stream: TelemetryStream::ActivityObservations,
        native_key: NativeRecordIdentity::Activity(ActivityKey {
            session: NativeActivityIdentity { id: "42".into(), generation: "login".into() },
            request: Some(NativeActivityIdentity { id: "0".into(), generation: "request".into() }),
            transaction: None,
        }),
        record_id: digest(b"row"),
        payload_sha256: digest(b"payload"),
        representation_version: REPRESENTATION_VERSION,
        row_ordinal: 0,
    };
    let NativeRecordIdentity::Activity(key) = &reference.native_key else { unreachable!() };
    let episode = DiagnosticEpisode::start(
        DiagnosticScope {
            target: culprit_core::DatabaseIdentity::server(
                culprit_core::Engine::Mssql,
                "private-db",
                "private-host",
                1433,
                None,
            )
            .into(),
            partition: SourcePartition::Target,
            source: "activity".into(),
            population: DiagnosticPopulation::Activity {
                population: ActivityPopulation::ActiveRequestsOpenTransactionsAndBlockers,
            },
            measurement_scope: culprit_core::MeasurementScope::Database,
            native_epoch: Some("native-process".into()),
        },
        DiagnosticSignal::Activity {
            key: key.clone(),
            condition: ActivityCondition::LongRunning,
            policy: ActivityPolicy::default(),
        },
        "2020-01-01T12:00:00Z".into(),
        reference,
    )
    .unwrap();
    DiagnosticRevision::between(None, episode, "2020-01-01T12:00:01Z").unwrap().unwrap()
}
fn imported(rows: &[DiagnosticRevision], complete: bool) -> Result<IncidentHistoryReport> {
    explain(
        &rows.iter().map(|revision| HistoryRevision { revision, original: None }).collect::<Vec<_>>(),
        IncidentHistoryProvenance::ImportedArtifact,
        complete,
    )
}
fn next(before: &DiagnosticRevision, resolved: bool) -> DiagnosticRevision {
    let mut episode = before.episode.clone();
    episode.analysis = DiagnosticAnalysisStatus::Unresolved;
    episode.issue = Some(DiagnosticAnalysisIssue::EvidenceUnavailable);
    episode.lifecycle = if resolved { DiagnosticLifecycle::Resolved } else { DiagnosticLifecycle::Uncertain };
    if resolved {
        episode.recovery = Some(DiagnosticRecoveryProof::CompleteScope { capture: episode.first.capture_ref() });
    }
    DiagnosticRevision::between(Some(&before.episode), episode, "2020-01-01T12:00:02Z").unwrap().unwrap()
}
#[test]
fn resolved_lifecycle_keeps_unresolved_analysis_original_clocks_and_references_without_current_action_authority() {
    let first = first();
    let resolved = next(&first, true);
    let report = imported(&[resolved.clone(), first.clone()], true).unwrap();
    assert_eq!(report.entries.len(), 2);
    assert_eq!(report.entries[0].revision, "1");
    let last = &report.entries[1];
    assert_eq!(last.lifecycle, DiagnosticLifecycle::Resolved);
    assert_eq!(last.analysis, DiagnosticAnalysisStatus::Unresolved);
    assert!(last.predecessor_selected);
    assert_eq!(last.first_seen_at, first.episode.first_seen_at);
    assert_eq!(last.recorded_at, resolved.analyzed_at);
    assert!(last.original.is_none());
    assert_eq!(last.evidence.len(), 3);
    assert!(
        last.evidence.iter().all(|evidence| evidence.availability == t!("view.incident_history.evidence.unverified"))
    );
    assert!(last.reasons.contains(&issue_message(&DiagnosticAnalysisIssue::EvidenceUnavailable)));
    let json = serde_json::to_string(&report).unwrap();
    assert!(!json.contains("private-host"));
    assert!(!json.contains("private-db"));
    assert!(report.limitations.contains(&t!("view.incident_history.limit.actions")));
}
#[test]
fn an_imported_prefix_never_becomes_authenticated_complete_controller_state_and_missing_predecessors_are_visible() {
    let resolved = next(&first(), true);
    let report = imported(&[resolved], false).unwrap();
    assert_eq!(report.source_state, None);
    assert!(!report.selected_read_complete);
    assert!(!report.entries[0].predecessor_selected);
    assert!(report.entries[0].reasons.contains(&t!("view.incident_history.limit.predecessor")));
    assert!(report.limitations.contains(&t!("view.incident_history.limit.partial")));
}
#[test]
fn repeats_are_idempotent_and_same_revision_contradictions_beyond_the_display_ceiling_are_rejected() {
    let first = first();
    assert_eq!(imported(&[first.clone(), first.clone()], true).unwrap().entries.len(), 1);
    let mut changed = first.clone();
    changed.analyzed_at = "2020-01-01T12:00:02Z".into();
    assert!(changed.is_valid());
    let mut rows = vec![first.clone(); MAX_INCIDENT_HISTORY_REVISIONS + 1];
    rows.push(changed);
    let error = imported(&rows, true).unwrap_err();
    assert_eq!(error.detail, Some(culprit_core::ErrorDetail::Delivery(DeliveryProblem::Conflict)));
}
#[test]
fn authenticated_revision_receipts_must_match_original_kind_identity_and_source() {
    let row = first();
    let mut reference = row.episode.first.clone();
    reference.stream = TelemetryStream::OperationalSamples;
    reference.batch_id = batch_id(&reference.capture.observation_id, reference.stream).unwrap();
    reference.native_key = NativeRecordIdentity::Operational { source: DIAGNOSTICS_SOURCE.into(), key: row.id.clone() };
    let explain_row = |reference: &DiagnosticEvidenceRef| {
        explain(
            &[HistoryRevision { revision: &row, original: Some(reference) }],
            IncidentHistoryProvenance::AuthenticatedCapture,
            true,
        )
    };
    let report = explain_row(&reference).unwrap();
    assert_eq!(report.source_state, Some(SourceState::Collected));
    assert_eq!(report.entries[0].original.as_ref(), Some(&reference));
    reference.native_key =
        NativeRecordIdentity::Operational { source: DIAGNOSTICS_SOURCE.into(), key: digest(b"different") };
    assert!(explain_row(&reference).is_err());
    assert!(
        explain(
            &[HistoryRevision { revision: &row, original: None }],
            IncidentHistoryProvenance::AuthenticatedCapture,
            true
        )
        .is_err()
    );
}
#[test]
fn invalid_revision_or_excess_distinct_history_refuses_instead_of_choosing_or_truncating() {
    let first = first();
    let mut invalid = first.clone();
    invalid.episode.revision = "9007199254740993".into();
    assert!(imported(&[invalid], true).is_err());
    let mut rows = vec![];
    for number in 1..=MAX_INCIDENT_HISTORY_REVISIONS + 1 {
        let mut row = first.clone();
        row.episode.revision = number.to_string();
        row.previous_revision = (number > 1).then(|| (number - 1).to_string());
        row.id = row.expected_id().unwrap();
        assert!(row.is_valid());
        rows.push(row);
    }
    assert_eq!(
        imported(&rows, true).unwrap_err().detail,
        Some(culprit_core::ErrorDetail::Delivery(DeliveryProblem::Capacity))
    );
}

#[test]
fn original_permission_refusal_and_missing_prior_attempt_are_preserved_without_synthetic_native_health() {
    let mut row = first();
    row.episode.analysis = DiagnosticAnalysisStatus::Unresolved;
    row.episode.issue = Some(DiagnosticAnalysisIssue::SourceUnavailable {
        code: Some(culprit_core::ErrorCode::PermissionDenied),
        stopped: None,
    });
    row.episode.investigation_event = Some(digest(b"prior owner event"));
    row.episode.revision = "2".into();
    row.previous_revision = Some("1".into());
    row.id = row.expected_id().unwrap();
    assert!(row.is_valid());
    let report = imported(&[row.clone()], true).unwrap();
    assert_eq!(report.entries[0].issue, row.episode.issue);
    assert!(report.entries[0].reasons.contains(&t!("view.incident_history.issue", reason = "denied")));
    assert!(report.entries[0].reasons.contains(&t!("view.incident_history.limit.prior_attempt")));
    assert_eq!(report.entries[0].followup_recipe, None);
    assert_eq!(report.source_state, None);
}

#[test]
fn different_recorded_controller_versions_are_distinct_histories_even_for_the_same_episode_number() {
    let first = first();
    let mut other = first.clone();
    other.controller_version += 1;
    other.id = other.expected_id().unwrap();
    assert!(other.is_valid());
    let report = imported(&[first.clone(), other.clone()], true).unwrap();
    assert_eq!(report.entries.len(), 2);
    assert_eq!(report.entries[0].controller_version, first.controller_version);
    assert_eq!(report.entries[1].controller_version, other.controller_version);
}

#[test]
fn conflicting_original_receipts_for_the_same_revision_refuse_instead_of_selecting_a_transport_survivor() {
    let revision = first();
    let mut first = revision.episode.first.clone();
    first.stream = TelemetryStream::OperationalSamples;
    first.batch_id = batch_id(&first.capture.observation_id, first.stream).unwrap();
    first.native_key =
        NativeRecordIdentity::Operational { source: DIAGNOSTICS_SOURCE.into(), key: revision.id.clone() };
    let mut second = first.clone();
    second.manifest_sha256 = digest(b"different original manifest");
    let rows = [
        HistoryRevision { revision: &revision, original: Some(&first) },
        HistoryRevision { revision: &revision, original: Some(&second) },
    ];
    assert_eq!(
        explain(&rows, IncidentHistoryProvenance::AuthenticatedCapture, true).unwrap_err().detail,
        Some(culprit_core::ErrorDetail::Delivery(DeliveryProblem::Conflict))
    );
}
