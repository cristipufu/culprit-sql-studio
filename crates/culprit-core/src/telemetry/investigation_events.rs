//! Immutable owner events describe admission and attempt history; they never approve native source work.
use super::diagnostics::{digest_value, positive, valid_capture};
use super::*;

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct DiagnosticAttemptReceipt {
    pub capture: CaptureIdentity,
    /// Absent when exact manifest bytes could not be attested.
    pub manifest_sha256: Option<String>,
}

impl DiagnosticAttemptReceipt {
    pub fn is_valid(&self) -> bool {
        valid_capture(&self.capture) && self.manifest_sha256.as_deref().is_none_or(valid_digest)
    }
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
#[ts(export)]
pub enum DiagnosticInvestigationOutcome {
    Admitted,
    Stopped { issue: DiagnosticAnalysisIssue },
    Captured { capture: DiagnosticCaptureRef },
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct DiagnosticInvestigationEvent {
    pub intent: Box<DiagnosticInvestigationIntent>,
    pub attempt: Option<DiagnosticAttemptReceipt>,
    /// Explicit owner settlement. Neither elapsed time nor an error category establishes native quiescence.
    pub source_work: SourceWorkState,
    pub outcome: DiagnosticInvestigationOutcome,
}

impl DiagnosticInvestigationEvent {
    pub fn is_valid(&self) -> bool {
        if !self.intent.is_valid() || self.attempt.as_ref().is_some_and(|attempt| !attempt.is_valid()) {
            return false;
        }
        match &self.outcome {
            DiagnosticInvestigationOutcome::Admitted => {
                self.attempt.is_none() && self.source_work == SourceWorkState::NotStarted
            }
            DiagnosticInvestigationOutcome::Stopped { .. } => match self.source_work {
                SourceWorkState::NotStarted | SourceWorkState::Unconfirmed => true,
                SourceWorkState::Quiescent => self.attempt.is_some(),
                SourceWorkState::Active => false,
            },
            DiagnosticInvestigationOutcome::Captured { capture } => {
                self.source_work == SourceWorkState::Quiescent
                    && capture.is_valid()
                    && self.attempt.as_ref().is_some_and(|attempt| {
                        attempt.capture == capture.capture
                            && attempt.manifest_sha256.as_deref() == Some(capture.manifest_sha256.as_str())
                    })
            }
        }
    }

    /// Exact immutable identity, including native accounting, outcome, attempt and the complete admitted intent.
    pub fn key(&self) -> Result<String> {
        if !self.is_valid() {
            return Err(CollectionStop::InvalidPage.error());
        }
        digest_value(&("diagnostic-investigation-event-v1", self))
    }

    /// A newer revision can still owe an earlier question, including after native symptom recovery.
    /// The caller independently authenticates the original row and the selected source's cost artifact.
    pub fn matches_episode(&self, episode: &DiagnosticEpisode) -> bool {
        self.is_valid()
            && episode.is_valid()
            && episode.id == self.intent.episode_id
            && positive(&episode.revision) >= positive(&self.intent.episode_revision)
            && episode.matches_focus(&self.intent.focus)
            && episode.scope.key().ok() == self.intent.qualification.scope.key().ok()
    }
}
