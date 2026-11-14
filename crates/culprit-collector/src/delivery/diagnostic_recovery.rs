//! Diagnostic recovery retains the entire working set and exact original receipt references, not one latest episode.
use super::{
    evidence::{RangeManifest, RowReceipt, range_id_for},
    recovery::{self, CommittedRange, RecoveryRecords, RecoveryStore},
};
use culprit_core::{Result, telemetry::*};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

const MAX_DIAGNOSTIC_RECOVERY_BYTES: u64 = 16 * 1024 * 1024;
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct DiagnosticHead {
    pub controller_key: String,
    pub controller_version: u32,
    pub target: CollectionTarget,
    pub parent: DiagnosticCaptureRef,
    pub capture: DiagnosticCaptureRef,
    pub range_id: String,
    pub manifest_digest: String,
}
impl DiagnosticHead {
    pub fn validate(&self) -> Result<()> {
        let expected = DiagnosticCheckpoint {
            controller_version: self.controller_version,
            target: self.target.clone(),
            input: self.parent.clone(),
            episodes: vec![],
        }
        .key()?;
        if expected != self.controller_key
            || !self.capture.is_valid()
            || !valid_digest(&self.range_id)
            || !valid_digest(&self.manifest_digest)
        {
            return Err(invalid());
        }
        Ok(())
    }
    fn rank(&self) -> Result<(u64, u64)> {
        let revision = self.capture.capture.revision.as_ref().ok_or_else(invalid)?;
        Ok((revision.owner_epoch.parse().map_err(|_| invalid())?, revision.sequence.parse().map_err(|_| invalid())?))
    }
}
#[derive(Clone)]
pub struct DiagnosticHeadPage {
    pub heads: Vec<DiagnosticHead>,
    pub next_cursor: Option<String>,
    pub high_sequence: u64,
}
#[derive(Default, Serialize, Deserialize)]
#[serde(transparent)]
pub(super) struct DiagnosticHeads(BTreeMap<String, DiagnosticHead>);
impl DiagnosticHeads {
    pub fn validate(&self) -> Result<()> {
        if self.0.len() > MAX_DIAGNOSTIC_EPISODES {
            return Err(DeliveryProblem::Capacity.error());
        }
        for (key, head) in &self.0 {
            head.validate()?;
            if key != &head.controller_key {
                return Err(invalid());
            }
        }
        Ok(())
    }
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
    pub fn observe(&mut self, manifest: &RangeManifest, manifest_digest: &str) -> Result<()> {
        let SourceObservation::Diagnostics(description) = &manifest.source.observation else {
            return Ok(());
        };
        let key = DiagnosticCheckpoint {
            controller_version: description.controller_version,
            target: manifest.source.target.clone(),
            input: description.parent.clone(),
            episodes: vec![],
        }
        .key()?;
        let candidate = DiagnosticHead {
            controller_key: key.clone(),
            controller_version: description.controller_version,
            target: manifest.source.target.clone(),
            parent: description.parent.clone(),
            capture: capture_reference(manifest)?,
            range_id: manifest.range_id.clone(),
            manifest_digest: manifest_digest.into(),
        };
        candidate.validate()?;
        if let Some(previous) = self.0.get(&key) {
            if previous.capture.capture.revision.as_ref().map(|revision| &revision.outbox_id)
                != candidate.capture.capture.revision.as_ref().map(|revision| &revision.outbox_id)
            {
                return Err(DeliveryProblem::Conflict.error());
            }
            if previous.rank()? > candidate.rank()? {
                return Ok(());
            }
            if previous.rank()? == candidate.rank()? && previous != &candidate {
                return Err(invalid());
            }
        } else if self.0.len() == MAX_DIAGNOSTIC_EPISODES {
            return Err(DeliveryProblem::Capacity.error());
        }
        self.0.insert(key, candidate);
        Ok(())
    }
    pub fn page(&self, cursor: Option<&str>, limit: u32, high_sequence: u64) -> Result<DiagnosticHeadPage> {
        self.validate()?;
        if !(1..=128).contains(&limit) {
            return Err(DeliveryProblem::Capacity.error());
        }
        let snapshot = digest(&serde_json::to_vec(self).map_err(|_| invalid())?);
        let after = match cursor {
            Some(text) if text.len() <= 512 => {
                let (prior, after): (String, String) = serde_json::from_str(text).map_err(|_| invalid())?;
                if prior != snapshot || !valid_digest(&after) {
                    return Err(DeliveryProblem::Conflict.error());
                }
                after
            }
            Some(_) => return Err(invalid()),
            None => String::new(),
        };
        let mut heads = self
            .0
            .range((std::ops::Bound::Excluded(after), std::ops::Bound::Unbounded))
            .take(limit as usize + 1)
            .map(|(_, head)| head.clone())
            .collect::<Vec<_>>();
        let next_cursor = if heads.len() > limit as usize {
            heads.pop();
            Some(
                serde_json::to_string(&(snapshot, &heads.last().ok_or_else(invalid)?.controller_key))
                    .map_err(|_| invalid())?,
            )
        } else {
            None
        };
        Ok(DiagnosticHeadPage { heads, next_cursor, high_sequence })
    }
}

pub struct RecoveredDiagnosticInput {
    range: CommittedRange,
    rows: Vec<ExportedRecord>,
}
/// One exact immutable row authenticated by the recovery manifest and bounded payload reader.
/// Loading it never installs or acknowledges a diagnostic input obligation.
pub struct RecoveredDiagnosticEvidence {
    range: CommittedRange,
    record: ExportedRecord,
}
impl RecoveredDiagnosticEvidence {
    pub fn source(&self) -> &CollectionDescription {
        &self.range.manifest().source
    }
    pub fn record(&self) -> &ExportedRecord {
        &self.record
    }
    pub fn into_parts(self) -> (CollectionDescription, ExportedRecord) {
        (self.range.manifest().source.clone(), self.record)
    }
}

/// Resolve an artifact by exact receipt, including operational qualification evidence outside the work queue.
pub async fn load_diagnostic_evidence(
    store: &dyn RecoveryStore,
    reference: &DiagnosticEvidenceRef,
) -> Result<RecoveredDiagnosticEvidence> {
    if !reference.is_valid() {
        return Err(invalid());
    }
    let advertised = store.capture_manifest(&reference.capture).await?;
    verify_reference(&advertised, reference)?;
    let advertised_digest = advertised.digest()?;
    let range = recovery::reopen(store, &advertised.range_id).await?;
    if range.manifest_digest() != advertised_digest || capture_reference(range.manifest())? != reference.capture_ref() {
        return Err(invalid());
    }
    let rows = read_rows(store, range.manifest(), Some(reference.stream)).await?;
    // A validated range has one batch per stream. EOF authenticates the entire selected batch before its
    // exact receipt position is returned, including when the requested row occurs near the start.
    let record = rows.into_iter().nth(reference.row_ordinal as usize).ok_or_else(invalid)?;
    let candidate = DiagnosticEvidenceRef::from_export(
        &range.manifest().source,
        &reference.manifest_sha256,
        &record,
        reference.row_ordinal,
    )?;
    if candidate != *reference {
        return Err(invalid());
    }
    Ok(RecoveredDiagnosticEvidence { range, record })
}
impl RecoveredDiagnosticInput {
    pub fn range(&self) -> &CommittedRange {
        &self.range
    }
    pub(crate) fn into_parts(self) -> (CommittedRange, Vec<ExportedRecord>) {
        (self.range, self.rows)
    }
}
pub struct RecoveredDiagnosticCheckpoint {
    range: CommittedRange,
    checkpoint: DiagnosticCheckpoint,
}
impl RecoveredDiagnosticCheckpoint {
    pub fn range(&self) -> &CommittedRange {
        &self.range
    }
    pub fn checkpoint(&self) -> &DiagnosticCheckpoint {
        &self.checkpoint
    }
    pub(crate) fn into_parts(self) -> (CommittedRange, DiagnosticCheckpoint) {
        (self.range, self.checkpoint)
    }
}
pub fn capture_reference(manifest: &RangeManifest) -> Result<DiagnosticCaptureRef> {
    let reference = DiagnosticCaptureRef {
        capture: manifest.capture.clone(),
        manifest_sha256: manifest.capture_manifest_sha256.clone(),
        source_scope_id: manifest.source.captured_range()?.scope_id,
    };
    if !reference.is_valid() {
        return Err(invalid());
    }
    Ok(reference)
}
pub(super) fn validate_pending_input(
    manifest: &RangeManifest,
    input: &DiagnosticCaptureRef,
    controller_version: u32,
) -> Result<()> {
    if controller_version != DIAGNOSTIC_CONTROLLER_VERSION {
        return Err(DeliveryProblem::UnsupportedSchema.error());
    }
    if capture_reference(manifest)? != *input
        || !matches!(
            manifest.source.observation,
            SourceObservation::Activity(_) | SourceObservation::FocusedPageWait { .. }
        )
    {
        return Err(invalid());
    }
    Ok(())
}
pub async fn load_diagnostic_input(store: &dyn RecoveryStore, range_id: &str) -> Result<RecoveredDiagnosticInput> {
    let range = recovery::reopen(store, range_id).await?;
    if !matches!(
        range.manifest().source.observation,
        SourceObservation::Activity(_) | SourceObservation::FocusedPageWait { .. }
    ) {
        return Err(invalid());
    }
    let rows = read_rows(store, range.manifest(), None).await?;
    Ok(RecoveredDiagnosticInput { range, rows })
}
pub async fn load_diagnostic_checkpoint(
    store: &dyn RecoveryStore,
    head: &DiagnosticHead,
) -> Result<RecoveredDiagnosticCheckpoint> {
    head.validate()?;
    let range = recovery::reopen(store, &head.range_id).await?;
    if range.manifest_digest() != head.manifest_digest || capture_reference(range.manifest())? != head.capture {
        return Err(invalid());
    }
    let checkpoint = read_checkpoint(store, range.manifest()).await?;
    if checkpoint.key()? != head.controller_key
        || checkpoint.input != head.parent
        || checkpoint.target != head.target
        || checkpoint.controller_version != head.controller_version
    {
        return Err(invalid());
    }
    Ok(RecoveredDiagnosticCheckpoint { range, checkpoint })
}
/// Called before the store's single authoritative commit; an invalid/missing checkpoint cannot release a native input.
pub(super) async fn validate_staged(store: &dyn RecoveryStore, manifest: &RangeManifest) -> Result<()> {
    let SourceObservation::Diagnostics(description) = &manifest.source.observation else {
        return Ok(());
    };
    let rows = read_rows(store, manifest, None).await?;
    let mut references = crate::collection_references::ObservationReferences::default();
    for row in &rows {
        references.record(&row.record);
    }
    if !references.coverage(true, Some(&manifest.source)).complete {
        return Err(invalid());
    }
    let checkpoint = references.diagnostic_checkpoint().ok_or_else(invalid)?;
    if checkpoint.input != description.parent {
        return Err(invalid());
    }
    let mut manifests = BTreeMap::new();
    let mut exact_refs = BTreeMap::new();
    for revision in &references.diagnostic_revisions {
        for reference in revision
            .inputs
            .iter()
            .chain([&revision.episode.first, &revision.episode.latest])
            .chain(revision.episode.peak.iter())
            .chain(revision.episode.report.as_ref().map(DiagnosticReport::evidence))
        {
            exact_refs.insert(reference.key()?, reference.clone());
        }
    }
    for episode in &checkpoint.episodes {
        for reference in [&episode.first, &episode.latest]
            .into_iter()
            .chain(episode.peak.iter())
            .chain(episode.report.as_ref().map(DiagnosticReport::evidence))
        {
            exact_refs.insert(reference.key()?, reference.clone());
        }
    }
    for reference in exact_refs.into_values() {
        let range_id = range_id_for(&manifest.contract, &reference.capture)?;
        if !manifests.contains_key(&range_id) {
            if manifests.len() >= MAX_DIAGNOSTIC_EPISODES {
                return Err(DeliveryProblem::Capacity.error());
            }
            manifests.insert(range_id.clone(), store.load(&range_id).await?);
        }
        let referenced = manifests.get(&range_id).ok_or_else(invalid)?;
        if referenced.source.target.telemetry_id()? != manifest.source.target.telemetry_id()? {
            return Err(invalid());
        }
        verify_reference(referenced, &reference)?;
    }
    let episodes = references
        .diagnostic_revisions
        .iter()
        .map(|revision| revision.episode.clone())
        .chain(checkpoint.episodes.iter().cloned())
        .collect::<Vec<_>>();
    for episode in episodes {
        let Some(proof) = &episode.recovery else {
            continue;
        };
        let key = range_id_for(&manifest.contract, &proof.capture().capture)?;
        if !manifests.contains_key(&key) {
            if manifests.len() >= MAX_DIAGNOSTIC_EPISODES {
                return Err(DeliveryProblem::Capacity.error());
            }
            manifests.insert(key.clone(), store.load(&key).await?);
        }
        let capture = manifests.get(&key).ok_or_else(invalid)?;
        if capture_reference(capture)? != *proof.capture()
            || !proof.matches_source(&episode, &capture.source, proof.capture())
        {
            return Err(invalid());
        }
        if let DiagnosticRecoveryProof::FocusedParticipantGone { parent, .. } = proof {
            let parent_key = range_id_for(&manifest.contract, &parent.capture)?;
            if !manifests.contains_key(&parent_key) {
                if manifests.len() >= MAX_DIAGNOSTIC_EPISODES {
                    return Err(DeliveryProblem::Capacity.error());
                }
                manifests.insert(parent_key.clone(), store.load(&parent_key).await?);
            }
            verify_reference(manifests.get(&parent_key).ok_or_else(invalid)?, parent)?;
        }
    }
    // The capture digest is an attestation from preparation, not independently recovered raw manifest.json bytes.
    let parent = store.load(&range_id_for(&manifest.contract, &description.parent.capture)?).await?;
    if capture_reference(&parent)? != description.parent || parent.source.target != manifest.source.target {
        return Err(invalid());
    }
    Ok(())
}
/// Checks the original committed manifest attestation and exact row position without opening raw payloads.
pub fn verify_reference(manifest: &RangeManifest, reference: &DiagnosticEvidenceRef) -> Result<()> {
    manifest.validate()?;
    if !reference.is_valid() || capture_reference(manifest)? != reference.capture_ref() {
        return Err(invalid());
    }
    let batch = manifest
        .batches
        .iter()
        .find(|batch| batch.stream == reference.stream && batch.batch_id == reference.batch_id)
        .ok_or_else(invalid)?;
    let receipt = batch.rows.get(reference.row_ordinal as usize).ok_or_else(invalid)?;
    if receipt.key.record_id != reference.record_id
        || receipt.payload_digest != reference.payload_sha256
        || receipt.key.representation_version != reference.representation_version
        || receipt.native_key != reference.native_key
    {
        return Err(invalid());
    }
    Ok(())
}
async fn read_checkpoint(store: &dyn RecoveryStore, manifest: &RangeManifest) -> Result<DiagnosticCheckpoint> {
    let SourceObservation::Diagnostics(description) = &manifest.source.observation else {
        return Err(invalid());
    };
    let rows = read_rows(store, manifest, Some(TelemetryStream::DiagnosticCheckpoints)).await?;
    if rows.len() != description.checkpoint_records as usize {
        return Err(invalid());
    }
    let parts = rows
        .into_iter()
        .map(|row| match row.record {
            TelemetryRecord::DiagnosticCheckpoint(part) => Ok(part),
            _ => Err(invalid()),
        })
        .collect::<Result<Vec<_>>>()?;
    let checkpoint = DiagnosticCheckpointPart::assemble(parts)?;
    if checkpoint.target != manifest.source.target
        || checkpoint.input != description.parent
        || checkpoint.controller_version != description.controller_version
    {
        return Err(invalid());
    }
    Ok(checkpoint)
}
async fn read_rows(
    store: &dyn RecoveryStore,
    manifest: &RangeManifest,
    only: Option<TelemetryStream>,
) -> Result<Vec<ExportedRecord>> {
    let batches =
        manifest.batches.iter().filter(|batch| only.is_none_or(|stream| stream == batch.stream)).collect::<Vec<_>>();
    if batches.iter().map(|batch| batch.bytes).sum::<u64>() > MAX_DIAGNOSTIC_RECOVERY_BYTES
        || batches.iter().map(|batch| batch.rows.len()).sum::<usize>() > MAX_DIAGNOSTIC_EPISODES * 2
    {
        return Err(DeliveryProblem::Capacity.error());
    }
    let mut output = Vec::new();
    for batch in batches {
        let mut records = RecoveryRecords::open(store, batch).await?;
        let mut ordinal = 0;
        while let Some(row) = records.next().await? {
            if row.capture != manifest.capture
                || !row.record.is_valid()
                || ExportedRecord::new(&manifest.source, &manifest.capture, row.record.clone())? != row
                || batch.rows.get(ordinal) != Some(&RowReceipt::from_export(&row)?)
            {
                return Err(invalid());
            }
            output.push(row);
            ordinal += 1;
        }
        if ordinal != batch.rows.len() {
            return Err(invalid());
        }
    }
    Ok(output)
}
fn invalid() -> culprit_core::Error {
    DeliveryProblem::Integrity.error()
}
