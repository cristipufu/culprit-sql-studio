//! Offline diagnostic work uses sealed outbox evidence and the same transactional commit owner as native captures.
mod focus_pins;
mod input;
pub(super) mod journal;
pub(super) mod retirement;
#[cfg(test)]
mod tests;
mod writer;

use super::{Files, artifacts::ValidatedCapture, problem, storage};
use culprit_core::{Result, telemetry::*};
use rusqlite::{OptionalExtension, Transaction, params};
use std::{
    collections::{BTreeMap, BTreeSet},
    io::{BufRead, BufReader, Read},
};

const MAX_CHECKPOINT_BYTES: usize = 4 * 1024 * 1024;
const MAX_RESTORED_INPUT_BYTES: usize = 4 * 1024 * 1024;
const MAX_INVESTIGATION_BYTES: usize = crate::collection_files::MAX_MANIFEST_BYTES;

#[derive(Clone, Debug)]
pub struct DiagnosticInputRow {
    pub reference: DiagnosticEvidenceRef,
    pub row: ExportedRecord,
}

/// Local queue knowledge only. Evidence cached from an archive does not establish committed analysis.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DiagnosticInputState {
    Missing,
    EvidenceOnly,
    Pending,
    Consumed,
}

/// Rows and their exact receipt references are already verified. Native source work has ended before this value exists.
#[derive(Clone, Debug)]
pub struct DiagnosticInput {
    pub parent: DiagnosticCaptureRef,
    pub source: CollectionDescription,
    pub investigation: Option<DiagnosticInvestigationLineage>,
    pub connection: String,
    pub rows: Vec<DiagnosticInputRow>,
    pub controller_version: u32,
    pub checkpoint: Option<DiagnosticCheckpoint>,
    expected_head: Option<String>,
}

impl DiagnosticInput {
    /// The original database owns the controller. Shared native evidence retains its separate source target.
    pub fn controller_target(&self) -> Result<&CollectionTarget> {
        controller_target(&self.source, self.investigation.as_ref())
    }

    pub fn investigation_intent(&self) -> Result<Option<DiagnosticInvestigationIntent>> {
        investigation_intent(&self.source, self.investigation.as_ref())
    }

    /// Reuse the original frame validator after the outbox has authenticated every immutable row receipt.
    pub fn activity_frame(&self) -> Result<ActivityFrameEvidence<'_>> {
        let rows = self
            .rows
            .iter()
            .map(|row| match &row.row.record {
                TelemetryRecord::Activity(observation) => {
                    Ok(ActivityFrameRow { observation, reference: Some(&row.reference) })
                }
                _ => Err(integrity()),
            })
            .collect::<Result<Vec<_>>>()?;
        ActivityFrameEvidence::new(&self.source, &rows, true).map_err(|_| integrity())
    }
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DiagnosticUpdate {
    pub step_id: String,
    pub analyzed_at: String,
    pub revisions: Vec<DiagnosticRevision>,
    pub checkpoint: DiagnosticCheckpoint,
}
impl DiagnosticUpdate {
    fn replay_digest(&self) -> Result<String> {
        if !valid_digest(&self.step_id)
            || !self.checkpoint.is_valid()
            || !self.revisions.iter().all(DiagnosticRevision::is_valid)
        {
            return Err(integrity());
        }
        let revision_ids = self.revisions.iter().map(|revision| &revision.id).collect::<Vec<_>>();
        Ok(digest(&serde_json::to_vec(&(&self.step_id, &self.checkpoint, revision_ids)).map_err(|_| integrity())?))
    }
}

#[derive(Clone, Debug)]
pub struct DiagnosticCheckpointPage {
    pub checkpoints: Vec<DiagnosticCheckpoint>,
    pub next_cursor: Option<String>,
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RestoredInput {
    pub store_id: String,
    pub parent: DiagnosticCaptureRef,
    pub source: CollectionDescription,
    pub investigation: Option<DiagnosticInvestigationLineage>,
    pub connection: String,
    pub rows: Vec<ExportedRecord>,
}

pub(crate) fn eligible_source(
    source: &CollectionDescription,
    investigation: Option<&DiagnosticInvestigationLineage>,
) -> Result<bool> {
    let intent = investigation_intent(source, investigation)?;
    Ok(match (&source.observation, &source.target) {
        (SourceObservation::Activity(_), CollectionTarget::Database(_)) => true,
        (SourceObservation::PageWaitTargets { original_activity: Some(_), .. }, CollectionTarget::Database(_)) => true,
        (SourceObservation::CurrentRequestWork { result, .. }, CollectionTarget::Database(_)) => {
            result.focus.as_deref().is_some_and(ActivityRequestFocus::is_current_request_work_focus)
        }
        (
            SourceObservation::FocusedPageWait { .. }
            | SourceObservation::FocusedBlocking { .. }
            | SourceObservation::PageWaitWriters { .. }
            | SourceObservation::PageTaskHolders { .. }
            | SourceObservation::FocusedRequestMemory { .. }
            | SourceObservation::FocusedCompilationAdmission { .. },
            CollectionTarget::Database(_),
        ) => true,
        (
            SourceObservation::FocusedPageWait { .. } | SourceObservation::FocusedCompilationAdmission { .. },
            CollectionTarget::Shared(_),
        ) => intent.is_some(),
        _ => false,
    })
}

/// Cached prerequisites retain immutable evidence only. They never recreate pending analysis obligations.
pub(crate) fn evidence_cache_source(
    source: &CollectionDescription,
    investigation: Option<&DiagnosticInvestigationLineage>,
) -> Result<bool> {
    if matches!(source.observation, SourceObservation::PageWaitTargets { .. }) {
        return Ok(source.partition_is_consistent()
            && source.observation.covers_request()
            && (investigation.is_none() || eligible_source(source, investigation)?));
    }
    if shared_owner_source(source) {
        return Ok(investigation.is_none());
    }
    eligible_source(source, investigation)
}

fn investigation_intent(
    source: &CollectionDescription,
    investigation: Option<&DiagnosticInvestigationLineage>,
) -> Result<Option<DiagnosticInvestigationIntent>> {
    let Some(investigation) = investigation else { return Ok(None) };
    investigation.intent_for_source(source).map(Some).map_err(|_| integrity())
}

fn controller_target<'a>(
    source: &'a CollectionDescription,
    investigation: Option<&DiagnosticInvestigationLineage>,
) -> Result<&'a CollectionTarget> {
    if !eligible_source(source, investigation)?
        && !matches!(source.observation, SourceObservation::PageWaitTargets { .. })
    {
        return Err(integrity());
    }
    Ok(match &source.observation {
        SourceObservation::FocusedBlocking { result, .. } => &result.focus.target,
        SourceObservation::FocusedPageWait { result, .. } => &result.focus.participant.target,
        SourceObservation::PageWaitWriters { focus, .. } => &focus.waiter.participant.target,
        SourceObservation::PageTaskHolders { focus, .. } => &focus.participant.target,
        SourceObservation::FocusedRequestMemory { result, .. } => &result.focus.target,
        SourceObservation::FocusedCompilationAdmission { result, .. } => &result.focus.target,
        _ => &source.target,
    })
}

pub(crate) fn shared_owner_source(source: &CollectionDescription) -> bool {
    matches!(
        (&source.target, &source.partition, &source.observation),
        (CollectionTarget::Shared(resource), SourcePartition::InstanceLocal { member: Some(member), epoch: Some(_) }, SourceObservation::Snapshot(StateSnapshotDescription { dataset: SnapshotDataset::Operational, .. }))
            if member == resource && source.partition.is_qualified()
    )
}

fn encode(value: &impl serde::Serialize) -> Result<String> {
    super::journal::encode(value)
}
fn decode<T: serde::de::DeserializeOwned>(text: &str) -> Result<T> {
    serde_json::from_str(text).map_err(|_| integrity())
}
fn encode_investigation(investigation: Option<&DiagnosticInvestigationLineage>) -> Result<Option<String>> {
    investigation
        .map(|investigation| {
            let encoded = encode(investigation)?;
            if encoded.len() > MAX_INVESTIGATION_BYTES {
                return Err(integrity());
            }
            Ok(encoded)
        })
        .transpose()
}
fn decode_investigation(encoded: Option<&str>) -> Result<Option<DiagnosticInvestigationLineage>> {
    encoded
        .map(|encoded| {
            if encoded.len() > MAX_INVESTIGATION_BYTES {
                return Err(integrity());
            }
            decode(encoded)
        })
        .transpose()
}
fn restored_checksum(
    parent: &DiagnosticCaptureRef,
    source: &CollectionDescription,
    investigation: Option<&DiagnosticInvestigationLineage>,
    rows: &str,
) -> Result<String> {
    Ok(digest(encode(&(parent, source, investigation, rows))?.as_bytes()))
}
fn integrity() -> culprit_core::Error {
    problem(OutboxProblem::Integrity)
}
fn controller_key(parent: &DiagnosticCaptureRef, target: &CollectionTarget, version: u32) -> Result<String> {
    DiagnosticCheckpoint {
        controller_version: version,
        target: target.clone(),
        input: parent.clone(),
        episodes: vec![],
    }
    .key()
}
fn row_references(
    parent: &DiagnosticCaptureRef,
    source: &CollectionDescription,
    rows: Vec<ExportedRecord>,
) -> Result<Vec<DiagnosticInputRow>> {
    if !parent.is_valid() || source.captured_range()?.scope_id != parent.source_scope_id {
        return Err(integrity());
    }
    let mut ordinals = BTreeMap::<String, u32>::new();
    rows.into_iter()
        .map(|row| {
            if row.capture != parent.capture {
                return Err(integrity());
            }
            let ordinal = ordinals.entry(row.batch_id.clone()).or_default();
            let reference = DiagnosticEvidenceRef::from_export(source, &parent.manifest_sha256, &row, *ordinal)?;
            *ordinal = ordinal.checked_add(1).ok_or_else(integrity)?;
            Ok(DiagnosticInputRow { reference, row })
        })
        .collect()
}
