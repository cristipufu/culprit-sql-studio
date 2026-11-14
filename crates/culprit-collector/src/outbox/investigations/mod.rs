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
    pub connection: String,
    pub rows: Vec<DiagnosticInputRow>,
    pub controller_version: u32,
    pub checkpoint: Option<DiagnosticCheckpoint>,
    expected_head: Option<String>,
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
    pub connection: String,
    pub rows: Vec<ExportedRecord>,
}

pub(super) fn eligible(request: &CollectionRequest) -> bool {
    matches!(request.selection, CollectionSelection::Activity | CollectionSelection::FocusedPageWait { .. })
}

fn encode(value: &impl serde::Serialize) -> Result<String> {
    super::journal::encode(value)
}
fn decode<T: serde::de::DeserializeOwned>(text: &str) -> Result<T> {
    serde_json::from_str(text).map_err(|_| integrity())
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
