//! Controlled immutable revision history; no native collection, chronology or cost qualification is claimed.
use super::*;

#[derive(Clone, Copy, Debug)]
pub enum IncidentHistoryCase {
    Ongoing,
    Resolved,
    Uncertain,
    Prefix,
    Many,
    TemporaryResources,
    TemporaryResourcesPartial,
}
impl IncidentHistoryCase {
    pub const ALL: &'static [Self] = &[
        Self::Ongoing,
        Self::Resolved,
        Self::Uncertain,
        Self::Prefix,
        Self::Many,
        Self::TemporaryResources,
        Self::TemporaryResourcesPartial,
    ];
}
pub fn incident_history_evidence(case: IncidentHistoryCase) -> (CollectionDescription, Vec<TelemetryRecord>) {
    if matches!(case, IncidentHistoryCase::TemporaryResources | IncidentHistoryCase::TemporaryResourcesPartial) {
        return super::temporary_resource_history(matches!(case, IncidentHistoryCase::TemporaryResourcesPartial));
    }
    let (mut source, mut records) = diagnostic_evidence_cases().remove(0);
    let TelemetryRecord::Operational(first) = records.remove(0) else { unreachable!() };
    let OperationalFact::DiagnosticRevision(first) = first.fact else { unreachable!() };
    let mut revisions = vec![*first];
    let count = if matches!(case, IncidentHistoryCase::Many) { 13 } else { 3 };
    for number in 2..=count {
        let previous = revisions.last().unwrap();
        let mut next = previous.episode.clone();
        next.lifecycle = if number == count
            && matches!(case, IncidentHistoryCase::Resolved | IncidentHistoryCase::Prefix | IncidentHistoryCase::Many)
        {
            next.recovery = Some(DiagnosticRecoveryProof::CompleteScope {
                capture: DiagnosticCaptureRef {
                    capture: CaptureIdentity {
                        observation_id: "controlled-recovery".into(),
                        revision: Some(CaptureRevision {
                            outbox_id: "owner".into(),
                            owner_epoch: "1".into(),
                            sequence: "2".into(),
                        }),
                    },
                    manifest_sha256: digest(b"controlled recovery manifest"),
                    source_scope_id: next.first.source_scope_id.clone(),
                },
            });
            DiagnosticLifecycle::Resolved
        } else if number % 2 == 0 {
            DiagnosticLifecycle::Uncertain
        } else {
            DiagnosticLifecycle::Active
        };
        next.analysis =
            if number == count { DiagnosticAnalysisStatus::Unresolved } else { DiagnosticAnalysisStatus::Deferred };
        next.issue = Some(if number == count {
            DiagnosticAnalysisIssue::EvidenceUnavailable
        } else {
            DiagnosticAnalysisIssue::CostUnqualified
        });
        let revision =
            DiagnosticRevision::between(Some(&previous.episode), next, &previous.analyzed_at).unwrap().unwrap();
        revisions.push(revision);
    }
    if matches!(case, IncidentHistoryCase::Ongoing) {
        revisions.truncate(1);
    }
    if matches!(case, IncidentHistoryCase::Uncertain) {
        revisions.truncate(2);
    }
    if matches!(case, IncidentHistoryCase::Prefix) {
        revisions.drain(..2);
    }
    let SourceObservation::Diagnostics(header) = &mut source.observation else { unreachable!() };
    header.records = revisions.len() as u32;
    let Some(TelemetryRecord::DiagnosticCheckpoint(checkpoint)) = records.first_mut() else { unreachable!() };
    checkpoint.episodes = revisions
        .last()
        .into_iter()
        .map(|revision| &revision.episode)
        .filter(|episode| episode.retained_in_checkpoint())
        .cloned()
        .collect();
    checkpoint.id = checkpoint.expected_id().unwrap();
    let mut rows = revisions
        .into_iter()
        .map(|revision| {
            TelemetryRecord::Operational(OperationalObservation {
                source: DIAGNOSTICS_SOURCE.into(),
                key: revision.id.clone(),
                fact: OperationalFact::DiagnosticRevision(Box::new(revision)),
            })
        })
        .collect::<Vec<_>>();
    rows.extend(records);
    assert!(rows.iter().all(TelemetryRecord::is_valid));
    (source, rows)
}

/// Store adapters must retain resolved revisions after the checkpoint retires them, with exact native references.
pub fn assert_incident_history_capture(
    capture: &crate::analytical::evidence::EvidenceCapture,
    source: &CollectionDescription,
    records: &[TelemetryRecord],
) {
    super::assert_diagnostic_capture(capture, source, records);
    for expected in records.iter().filter_map(|row| match row {
        TelemetryRecord::Operational(OperationalObservation {
            fact: OperationalFact::DiagnosticRevision(revision),
            ..
        }) => Some(revision.as_ref()),
        _ => None,
    }) {
        let actual = capture
            .records()
            .iter()
            .find_map(|row| match &row.row().record {
                TelemetryRecord::Operational(OperationalObservation {
                    fact: OperationalFact::DiagnosticRevision(revision),
                    ..
                }) if revision.id == expected.id => Some(revision.as_ref()),
                _ => None,
            })
            .unwrap();
        assert_eq!(actual, expected);
        assert_eq!(actual.episode.lifecycle, expected.episode.lifecycle);
        assert_eq!(actual.episode.recovery, expected.episode.recovery);
    }
}
