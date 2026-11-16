//! Diagnostic recovery retains the entire working set and exact original receipt references, not one latest episode.
use super::{
    evidence::{RangeManifest, range_id_for},
    recovery::{self, CommittedRange, RecoveryRecords, RecoveryStore},
};
use culprit_core::{Result, telemetry::*};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

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
    cached_rows: Option<Vec<ExportedRecord>>,
}
impl RecoveredDiagnosticEvidence {
    pub fn source(&self) -> &CollectionDescription {
        &self.range.manifest().source
    }
    pub fn record(&self) -> &ExportedRecord {
        &self.record
    }
    pub(crate) fn manifest_digest(&self) -> &str {
        self.range.manifest_digest()
    }
    pub fn into_parts(self) -> (CollectionDescription, ExportedRecord) {
        (self.range.manifest().source.clone(), self.record)
    }
    pub(crate) fn can_cache(&self) -> bool {
        self.cached_rows.is_some()
    }
    pub(crate) fn into_cached_input(self) -> Result<RecoveredDiagnosticInput> {
        Ok(RecoveredDiagnosticInput { range: self.range, rows: self.cached_rows.ok_or_else(invalid)? })
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
    let cacheable =
        crate::outbox::evidence_cache_source(&range.manifest().source, range.manifest().investigation.as_ref())?;
    let rows = read_rows(store, range.manifest(), (!cacheable).then_some(reference.stream)).await?;
    // A validated range has one batch per stream. EOF authenticates the entire selected batch before its
    // exact receipt position is returned, including when the requested row occurs near the start.
    let record = rows
        .iter()
        .filter(|row| row.batch_id == reference.batch_id)
        .nth(reference.row_ordinal as usize)
        .cloned()
        .ok_or_else(invalid)?;
    let candidate = DiagnosticEvidenceRef::from_export(
        &range.manifest().source,
        &reference.manifest_sha256,
        &record,
        reference.row_ordinal,
    )?;
    if candidate != *reference {
        return Err(invalid());
    }
    Ok(RecoveredDiagnosticEvidence { range, record, cached_rows: cacheable.then_some(rows) })
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
        || !crate::outbox::diagnostic_source_eligible(&manifest.source, manifest.investigation.as_ref())?
    {
        return Err(invalid());
    }
    Ok(())
}
pub async fn load_diagnostic_input(store: &dyn RecoveryStore, range_id: &str) -> Result<RecoveredDiagnosticInput> {
    let range = recovery::reopen(store, range_id).await?;
    if !crate::outbox::diagnostic_source_eligible(&range.manifest().source, range.manifest().investigation.as_ref())? {
        return Err(invalid());
    }
    let rows = read_rows(store, range.manifest(), None).await?;
    let mut references = crate::collection_references::ObservationReferences::default();
    for row in &rows {
        references.record(&row.record);
    }
    if !references.coverage(true, Some(&range.manifest().source)).complete {
        return Err(invalid());
    }
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
    let mut authenticated = BTreeSet::new();
    let mut authenticated_reports = BTreeSet::new();
    let mut shared_references = BTreeMap::<String, BTreeSet<String>>::new();
    for revision in &references.diagnostic_revisions {
        let Some(event) = &revision.investigation else { continue };
        authenticate_investigation_intent(store, &manifest.contract, &event.intent, &mut authenticated).await?;
        authorize_shared_owner(&mut shared_references, &event.intent)?;
        let DiagnosticInvestigationOutcome::Captured { capture } = &event.outcome else { continue };
        let range_id = range_id_for(&manifest.contract, &capture.capture)?;
        if !manifests.contains_key(&range_id) {
            if manifests.len() >= MAX_DIAGNOSTIC_EPISODES {
                return Err(DeliveryProblem::Capacity.error());
            }
            // Receipt membership alone cannot authenticate retained native payloads. Read to verified EOF before
            // an immutable owner event may claim this result, including a valid zero-row focused result.
            let recovered = load_diagnostic_input(store, &range_id).await?;
            manifests.insert(range_id.clone(), recovered.range().manifest().clone());
        }
        let native = manifests.get(&range_id).ok_or_else(invalid)?;
        if capture_reference(native)? != *capture
            || event.intent.focus.participant().target.telemetry_id()? != manifest.source.target.telemetry_id()?
            || !event.intent.matches_result_source(&native.source)
        {
            return Err(invalid());
        }
        let focus = native.source.observation.diagnostic_focus().ok_or_else(invalid)?;
        if !native.source.observation.covers_request()
            || focus != event.intent.focus
            || native.investigation.as_ref().is_none_or(|owner| !owner.matches_intent(&focus, &event.intent))
        {
            return Err(invalid());
        }
    }
    for episode in references.diagnostic_revisions.iter().map(|revision| &revision.episode).chain(&checkpoint.episodes)
    {
        let Some(report) = &episode.report else { continue };
        let range_id = range_id_for(&manifest.contract, &report.evidence().capture)?;
        if !manifests.contains_key(&range_id) {
            if manifests.len() >= MAX_DIAGNOSTIC_EPISODES {
                return Err(DeliveryProblem::Capacity.error());
            }
            manifests.insert(range_id.clone(), store.load(&range_id).await?);
        }
        let native = manifests.get(&range_id).ok_or_else(invalid)?;
        let report_key = digest(&serde_json::to_vec(&(&episode.id, report)).map_err(|_| invalid())?);
        if authenticated_reports.insert(report_key) {
            authenticate_report_participants(store, native, episode).await?;
        }
        if matches!(native.source.target, CollectionTarget::Shared(_)) {
            let intent = authenticate_owned_report(store, native, episode, &mut authenticated).await?;
            authorize_shared_owner(&mut shared_references, &intent)?;
            shared_references.entry(episode.id.clone()).or_default().insert(report.evidence().key()?);
        }
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
        if capture_reference(capture)? != *proof.capture() {
            return Err(invalid());
        }
        let intent = native_intent(capture)?;
        if matches!(capture.source.target, CollectionTarget::Shared(_)) {
            let recovered = load_diagnostic_input(store, &capture.range_id).await?;
            if recovered.range().manifest_digest() != capture.digest()? {
                return Err(invalid());
            }
            let intent = intent.as_ref().ok_or_else(invalid)?;
            if !intent.matches_episode(&episode) {
                return Err(invalid());
            }
            authenticate_investigation_intent(store, &capture.contract, intent, &mut authenticated).await?;
            authorize_shared_owner(&mut shared_references, intent)?;
        }
        let matches_source = match &intent {
            Some(intent) => proof.matches_source_with_intent(&episode, &capture.source, proof.capture(), intent),
            None => proof.matches_source(&episode, &capture.source, proof.capture()),
        };
        if !matches_source {
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
    let mut exact_refs = BTreeMap::new();
    for revision in &references.diagnostic_revisions {
        for reference in revision
            .inputs
            .iter()
            .chain([&revision.episode.first, &revision.episode.latest])
            .chain(revision.episode.peak.iter())
            .chain(revision.episode.report.iter().flat_map(DiagnosticReport::references))
        {
            let entry = exact_refs.entry(reference.key()?).or_insert_with(|| (reference.clone(), BTreeSet::new()));
            entry.1.insert(revision.episode.id.clone());
        }
    }
    for episode in &checkpoint.episodes {
        for reference in [&episode.first, &episode.latest]
            .into_iter()
            .chain(episode.peak.iter())
            .chain(episode.report.iter().flat_map(DiagnosticReport::references))
        {
            let entry = exact_refs.entry(reference.key()?).or_insert_with(|| (reference.clone(), BTreeSet::new()));
            entry.1.insert(episode.id.clone());
        }
    }
    for (reference, episodes) in exact_refs.into_values() {
        let reference_key = reference.key()?;
        let range_id = range_id_for(&manifest.contract, &reference.capture)?;
        if !manifests.contains_key(&range_id) {
            if manifests.len() >= MAX_DIAGNOSTIC_EPISODES {
                return Err(DeliveryProblem::Capacity.error());
            }
            manifests.insert(range_id.clone(), store.load(&range_id).await?);
        }
        let referenced = manifests.get(&range_id).ok_or_else(invalid)?;
        if referenced.source.target.telemetry_id()? != manifest.source.target.telemetry_id()?
            && !episodes
                .iter()
                .all(|episode| shared_references.get(episode).is_some_and(|allowed| allowed.contains(&reference_key)))
        {
            return Err(invalid());
        }
        verify_reference(referenced, &reference)?;
    }
    // The capture digest is an attestation from preparation, not independently recovered raw manifest.json bytes.
    let parent = store.load(&range_id_for(&manifest.contract, &description.parent.capture)?).await?;
    if capture_reference(&parent)? != description.parent {
        return Err(invalid());
    }
    if parent.source.target != manifest.source.target {
        let recovered = load_diagnostic_input(store, &parent.range_id).await?;
        if recovered.range().manifest_digest() != parent.digest()? {
            return Err(invalid());
        }
        let intent = native_intent(&parent)?.ok_or_else(invalid)?;
        if intent.focus.participant().target != manifest.source.target
            || !matches!(intent.execution, DiagnosticExecutionBinding::Shared { .. })
        {
            return Err(invalid());
        }
        authenticate_investigation_intent(store, &parent.contract, &intent, &mut authenticated).await?;
    }
    Ok(())
}

fn native_intent(manifest: &RangeManifest) -> Result<Option<DiagnosticInvestigationIntent>> {
    let Some(owner) = &manifest.investigation else { return Ok(None) };
    let focus = manifest.source.observation.diagnostic_focus().ok_or_else(invalid)?;
    let intent = owner.reconstruct(&focus)?;
    if !intent.matches_result_source(&manifest.source) {
        return Err(invalid());
    }
    Ok(Some(intent))
}

fn input_rows(input: &RecoveredDiagnosticInput) -> Result<Vec<(DiagnosticEvidenceRef, &ExportedRecord)>> {
    let manifest = input.range().manifest();
    let mut ordinals = BTreeMap::<&str, u32>::new();
    input
        .rows
        .iter()
        .map(|row| {
            let ordinal = ordinals.entry(&row.batch_id).or_default();
            let reference =
                DiagnosticEvidenceRef::from_export(&manifest.source, &manifest.capture_manifest_sha256, row, *ordinal)?;
            *ordinal = ordinal.checked_add(1).ok_or_else(invalid)?;
            Ok((reference, row))
        })
        .collect()
}

async fn authenticate_report_participants(
    store: &dyn RecoveryStore,
    manifest: &RangeManifest,
    episode: &DiagnosticEpisode,
) -> Result<()> {
    let report = episode.report.as_ref().ok_or_else(invalid)?;
    let input = load_diagnostic_input(store, &manifest.range_id).await?;
    if input.range().manifest_digest() != manifest.digest()? {
        return Err(invalid());
    }
    let rows = input_rows(&input)?;
    let primary = rows.iter().find(|(reference, _)| reference == report.evidence()).ok_or_else(invalid)?;
    if !report.matches_observation(episode, &manifest.source, primary.1) {
        return Err(invalid());
    }
    match report {
        DiagnosticReport::Blocking { .. } => {
            let activity = rows
                .iter()
                .map(|(reference, row)| match &row.record {
                    TelemetryRecord::Activity(observation) => Ok((reference, observation.as_ref())),
                    _ => Err(invalid()),
                })
                .collect::<Result<Vec<_>>>()?;
            if !report.matches_blocking_input(episode, &manifest.source, activity) {
                return Err(invalid());
            }
        }
        DiagnosticReport::CurrentRequestWork { .. } => {
            let observations = rows
                .iter()
                .map(|(reference, row)| match &row.record {
                    TelemetryRecord::Operational(observation) => Ok((reference, observation)),
                    _ => Err(invalid()),
                })
                .collect::<Result<Vec<_>>>()?;
            if !report.matches_current_request_work_input(&manifest.source, observations) {
                return Err(invalid());
            }
        }
        DiagnosticReport::PageWaitWriters { blocking_contexts, .. } => {
            let SourceObservation::PageWaitWriters { focus, .. } = &manifest.source.observation else {
                return Err(invalid());
            };
            let observations = rows
                .iter()
                .map(|(reference, row)| match &row.record {
                    TelemetryRecord::Operational(observation) => Ok((reference, observation)),
                    _ => Err(invalid()),
                })
                .collect::<Result<Vec<_>>>()?;
            if !report.matches_writer_input(&manifest.source, observations) {
                return Err(invalid());
            }
            if *blocking_contexts != authenticate_page_writer_focus(store, &manifest.contract, focus).await? {
                return Err(invalid());
            }
        }
        DiagnosticReport::CompilationAdmission { .. } => {
            let observations = rows
                .iter()
                .map(|(reference, row)| match &row.record {
                    TelemetryRecord::Operational(observation) => Ok((reference, observation)),
                    _ => Err(invalid()),
                })
                .collect::<Result<Vec<_>>>()?;
            if !report.matches_compiler_admission_input(&manifest.source, observations) {
                return Err(invalid());
            }
        }
        DiagnosticReport::RequestMemory { .. } => {
            let grants = rows
                .iter()
                .map(|(reference, row)| match &row.record {
                    TelemetryRecord::Operational(OperationalObservation {
                        fact: OperationalFact::ExecutionGrant(grant),
                        ..
                    }) => Ok((reference, grant.as_ref())),
                    _ => Err(invalid()),
                })
                .collect::<Result<Vec<_>>>()?;
            if !report.matches_request_memory_input(&manifest.source, grants) {
                return Err(invalid());
            }
        }
        DiagnosticReport::PageWait { blocking_contexts, .. } => {
            let SourceObservation::FocusedPageWait { result, .. } = &manifest.source.observation else {
                return Err(invalid());
            };
            let TelemetryRecord::Operational(OperationalObservation { fact: OperationalFact::PageWait(page), .. }) =
                &primary.1.record
            else {
                return Err(invalid());
            };
            let focus = &result.focus.participant;
            let parent_id = range_id_for(&manifest.contract, &focus.parent.capture)?;
            let parent = load_diagnostic_input(store, &parent_id).await?;
            verify_reference(parent.range().manifest(), &focus.parent)?;
            let original_rows = input_rows(&parent)?;
            let activity = original_rows
                .iter()
                .map(|(reference, row)| match &row.record {
                    TelemetryRecord::Activity(observation) => {
                        Ok(ActivityFrameRow { observation, reference: Some(reference) })
                    }
                    _ => Err(invalid()),
                })
                .collect::<Result<Vec<_>>>()?;
            let frame = ActivityFrameEvidence::new(&parent.range().manifest().source, &activity, true)?;
            let participation = culprit_core::page_wait_blocking_participation(&manifest.source, page, focus, &frame)?;
            if !blocking_contexts.iter().eq(participation.references()) {
                return Err(invalid());
            }
        }
    }
    Ok(())
}

async fn authenticate_owned_report(
    store: &dyn RecoveryStore,
    manifest: &RangeManifest,
    episode: &DiagnosticEpisode,
    authenticated: &mut BTreeSet<String>,
) -> Result<DiagnosticInvestigationIntent> {
    let report = episode.report.as_ref().ok_or_else(invalid)?;
    let recovered = load_diagnostic_evidence(store, report.evidence()).await?;
    if recovered.manifest_digest() != manifest.digest()? {
        return Err(invalid());
    }
    let intent = native_intent(manifest)?.ok_or_else(invalid)?;
    if !intent.matches_episode(episode) || !report.matches_observation(episode, recovered.source(), recovered.record())
    {
        return Err(invalid());
    }
    authenticate_investigation_intent(store, &manifest.contract, &intent, authenticated).await?;
    Ok(intent)
}

fn authorize_shared_owner(
    authorized: &mut BTreeMap<String, BTreeSet<String>>,
    intent: &DiagnosticInvestigationIntent,
) -> Result<()> {
    if let DiagnosticExecutionBinding::Shared { owner } = &intent.execution {
        authorized.entry(intent.episode_id.clone()).or_default().insert(owner.key()?);
    }
    Ok(())
}

async fn authenticate_investigation_intent(
    store: &dyn RecoveryStore,
    contract: &crate::delivery::contract::Contract,
    intent: &DiagnosticInvestigationIntent,
    authenticated: &mut BTreeSet<String>,
) -> Result<()> {
    let writer = match &intent.focus {
        DiagnosticFocus::PageWaitWriters { focus } => Some(focus.as_ref()),
        _ => None,
    };
    let shared = match &intent.execution {
        DiagnosticExecutionBinding::Shared { owner } => Some(owner.as_ref()),
        _ => None,
    };
    if writer.is_none() && shared.is_none() {
        return Ok(());
    }
    let key = intent.key()?;
    if authenticated.contains(&key) {
        return Ok(());
    }
    if authenticated.len() >= MAX_DIAGNOSTIC_EPISODES {
        return Err(DeliveryProblem::Capacity.error());
    }
    if let Some(focus) = writer {
        authenticate_page_writer_focus(store, contract, focus).await?;
    }
    if let Some(owner) = shared {
        let parent = load_diagnostic_evidence(store, &intent.focus.participant().parent).await?;
        let TelemetryRecord::Activity(original) = &parent.record().record else {
            return Err(invalid());
        };
        if !intent.focus.matches_parent(parent.source(), original, &intent.focus.participant().parent) {
            return Err(invalid());
        }
        let recovered_owner = load_diagnostic_evidence(store, owner).await?;
        if !intent.matches_shared_owner(recovered_owner.source(), recovered_owner.record()) {
            return Err(invalid());
        }
    }
    authenticated.insert(key);
    Ok(())
}

async fn authenticate_page_writer_focus(
    store: &dyn RecoveryStore,
    contract: &crate::delivery::contract::Contract,
    focus: &PageWaitWriterFocus,
) -> Result<Vec<DiagnosticEvidenceRef>> {
    let original = focus.waiter.participant();
    let parent = load_diagnostic_input(store, &range_id_for(contract, &original.parent.capture)?).await?;
    verify_reference(parent.range().manifest(), &original.parent)?;
    let rows = input_rows(&parent)?;
    let activity = activity_rows(&rows)?;
    let frame = ActivityFrameEvidence::new(&parent.range().manifest().source, &activity, true)?;
    if !focus.matches_activity_frame(&frame) {
        return Err(invalid());
    }
    let page = load_diagnostic_evidence(store, &focus.page).await?;
    let TelemetryRecord::Operational(OperationalObservation { fact: OperationalFact::PageWait(observation), .. }) =
        &page.record().record
    else {
        return Err(invalid());
    };
    if !focus.matches_page(page.source(), observation, &focus.page) {
        return Err(invalid());
    }
    let participation = culprit_core::page_wait_blocking_participation(page.source(), observation, original, &frame)?;
    Ok(participation.references().cloned().collect())
}

fn activity_rows<'a>(rows: &'a [(DiagnosticEvidenceRef, &'a ExportedRecord)]) -> Result<Vec<ActivityFrameRow<'a>>> {
    rows.iter()
        .map(|(reference, row)| match &row.record {
            TelemetryRecord::Activity(observation) => Ok(ActivityFrameRow { observation, reference: Some(reference) }),
            _ => Err(invalid()),
        })
        .collect()
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
            batch.validate_row(&manifest.source, &manifest.capture, ordinal + 1, &row)?;
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

#[cfg(test)]
mod tests;
