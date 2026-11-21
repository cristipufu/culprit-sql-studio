//! Offline diagnostic work uses sealed outbox evidence and the same transactional commit owner as native captures.
mod focus_pins;
mod input;
pub(super) mod journal;
pub(super) mod retirement;
mod source_read;
#[cfg(test)]
mod tests;
mod writer;

use super::{Files, artifacts::ValidatedCapture, problem, storage};
use culprit_core::{Result, telemetry::*};
use rusqlite::{OptionalExtension, Transaction, params};
use std::collections::{BTreeMap, BTreeSet};

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

/// Diagnostic-role rows have verified original receipts. A typed SourceRead control input retains only its
/// authenticated capture/header/lineage; its history body is not materialized as a diagnostic row roster.
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
    pub rows: Vec<(u32, ExportedRecord)>,
}

pub(crate) fn eligible_source(
    source: &CollectionDescription,
    investigation: Option<&DiagnosticInvestigationLineage>,
) -> Result<bool> {
    let intent = investigation_intent(source, investigation)?;
    if diagnostic_original_source_kind(source).is_some() {
        return Ok(investigation.is_none());
    }
    Ok(match (&source.observation, &source.target) {
        (SourceObservation::Activity(_), CollectionTarget::Database(_)) => true,
        (SourceObservation::SourceRead { .. }, CollectionTarget::Database(_)) => {
            source.observation.covers_request()
                && intent.as_ref().is_some_and(|intent| matches!(intent.focus, DiagnosticFocus::SourceRead { .. }))
        }
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

/// Canonical identity closure is required for history receipts; unavailable SQL bodies remain independent.
/// Both local commit and archive replay use the authenticated manifest's original reference coverage.
pub(crate) fn eligible_manifest(
    source: &CollectionDescription,
    investigation: Option<&DiagnosticInvestigationLineage>,
    references: &ReferenceCoverage,
) -> Result<bool> {
    Ok(eligible_source(source, investigation)?
        && (diagnostic_original_source_kind(source) != Some(DiagnosticSourceKind::QueryHistory)
            || references.is_valid() && references.complete))
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
    investigation: Option<&'a DiagnosticInvestigationLineage>,
) -> Result<&'a CollectionTarget> {
    if !eligible_source(source, investigation)?
        && !matches!(source.observation, SourceObservation::PageWaitTargets { .. })
    {
        return Err(integrity());
    }
    Ok(match &source.observation {
        SourceObservation::SourceRead { .. } => investigation.ok_or_else(integrity)?.focus.target(),
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
fn positioned_references(
    parent: &DiagnosticCaptureRef,
    source: &CollectionDescription,
    rows: Vec<(u32, ExportedRecord)>,
    investigation: Option<&DiagnosticInvestigationLineage>,
) -> Result<Vec<DiagnosticInputRow>> {
    if !parent.is_valid() || source.captured_range()?.scope_id != parent.source_scope_id {
        return Err(integrity());
    }
    let focus = if investigation.is_some() { source_read_result_focus(source, investigation)? } else { None };
    let selected = focus.is_some_and(SourceReadFocus::selects_result_rows);
    let mut positions = BTreeMap::<String, u32>::new();
    rows.into_iter()
        .map(|(ordinal, row)| {
            if row.capture != parent.capture || focus.is_some_and(|focus| !focus.retains_result_record(&row.record)) {
                return Err(integrity());
            }
            let previous = positions.get(&row.batch_id).copied();
            if selected {
                if previous.is_some_and(|previous| ordinal <= previous) {
                    return Err(integrity());
                }
            } else if ordinal != previous.map_or(0, |previous| previous + 1) {
                return Err(integrity());
            }
            positions.insert(row.batch_id.clone(), ordinal);
            let reference = DiagnosticEvidenceRef::from_export(source, &parent.manifest_sha256, &row, ordinal)?;
            Ok(DiagnosticInputRow { reference, row })
        })
        .collect()
}

/// The result selection comes from authenticated intent lineage, never a dataset or source-name guess.
pub(crate) fn source_read_result_focus<'a>(
    source: &CollectionDescription,
    investigation: Option<&'a DiagnosticInvestigationLineage>,
) -> Result<Option<&'a SourceReadFocus>> {
    if !matches!(source.observation, SourceObservation::SourceRead { .. }) {
        return Ok(None);
    }
    let intent = investigation_intent(source, investigation)?.ok_or_else(integrity)?;
    let lineage = investigation.ok_or_else(integrity)?;
    let DiagnosticFocus::SourceRead { focus } = &lineage.focus else { return Err(integrity()) };
    if intent.focus != lineage.focus {
        return Err(integrity());
    }
    Ok(focus.retains_result_rows().then_some(focus.as_ref()))
}
pub(crate) fn source_read_retains_rows(
    source: &CollectionDescription,
    investigation: Option<&DiagnosticInvestigationLineage>,
) -> Result<bool> {
    Ok(source_read_result_focus(source, investigation)?.is_some())
}
