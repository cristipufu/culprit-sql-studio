//! Structural consistency of one original Activity frame. Storage/recovery authenticates its receipts separately.
use super::{activity::ActivityIndex, *};
use std::collections::{BTreeMap, BTreeSet};

pub const MAX_ACTIVITY_FRAME_OBSERVATIONS: usize = 10_000;

#[derive(Clone, Copy)]
pub struct ActivityFrameRow<'a> {
    pub observation: &'a ActivityObservation,
    pub reference: Option<&'a DiagnosticEvidenceRef>,
}

/// Borrowed rows from one native read; neither validation nor a matching digest authenticates their storage owner.
pub struct ActivityFrameEvidence<'a> {
    source: &'a CollectionDescription,
    native: &'a ActivitySnapshotDescription,
    index: ActivityIndex<'a>,
    references: BTreeMap<ActivityKey, &'a DiagnosticEvidenceRef>,
    read_complete: bool,
}

impl<'a> ActivityFrameEvidence<'a> {
    pub fn new(source: &'a CollectionDescription, rows: &[ActivityFrameRow<'a>], read_complete: bool) -> Result<Self> {
        let native = validate_header(source, rows.len(), read_complete)?;
        let index = ActivityIndex::new(rows.iter().map(|row| row.observation))?;
        let references = validate_references(source, native, rows)?;
        Ok(Self { source, native, index, references, read_complete })
    }

    pub fn source(&self) -> &'a CollectionDescription {
        self.source
    }

    pub fn native(&self) -> &'a ActivitySnapshotDescription {
        self.native
    }

    pub fn context(&self) -> NativeReadContext {
        read_context(self.source, self.native)
    }

    pub fn is_qualified(&self) -> bool {
        self.source.is_partition_qualified() && self.native.epoch.is_some()
    }

    pub fn read_complete(&self) -> bool {
        self.read_complete
    }

    /// Present receipts have already been checked to share this exact capture. An empty read has no row receipt;
    /// its caller must authenticate the supplied capture header independently.
    pub(crate) fn capture_ref(&self) -> Option<DiagnosticCaptureRef> {
        self.references.values().next().map(|reference| reference.capture_ref())
    }

    pub fn row(&self, key: &ActivityKey) -> Option<ActivityFrameRow<'a>> {
        self.index.by_key.get(key).map(|observation| self.wrap(observation))
    }

    /// Every returned row belongs to this one validated read and keeps its own optional original receipt.
    pub fn rows(&self) -> impl Iterator<Item = ActivityFrameRow<'a>> + '_ {
        self.index.by_key.values().map(|observation| self.wrap(observation))
    }

    pub fn session_rows(&self, session: &NativeActivityIdentity) -> impl Iterator<Item = ActivityFrameRow<'a>> + '_ {
        self.index.by_session.get(session).into_iter().flatten().map(|observation| self.wrap(observation))
    }

    fn wrap(&self, observation: &'a ActivityObservation) -> ActivityFrameRow<'a> {
        ActivityFrameRow { observation, reference: self.references.get(&observation.key).copied() }
    }
}

fn validate_header(
    source: &CollectionDescription,
    rows: usize,
    read_complete: bool,
) -> Result<&ActivitySnapshotDescription> {
    let SourceObservation::Activity(native) = &source.observation else {
        return Err(invalid());
    };
    if !source.target.is_valid()
        || !source.partition_is_consistent()
        || !native.is_valid()
        || !read_context(source, native).is_valid()
        || native.coverage.source != source.source
        || rows > MAX_ACTIVITY_FRAME_OBSERVATIONS
        || rows > native.observations as usize
        || read_complete && rows != native.observations as usize
    {
        return Err(invalid());
    }
    Ok(native)
}

fn validate_references<'a>(
    source: &CollectionDescription,
    native: &ActivitySnapshotDescription,
    rows: &[ActivityFrameRow<'a>],
) -> Result<BTreeMap<ActivityKey, &'a DiagnosticEvidenceRef>> {
    let scope = source.captured_range()?.scope_id;
    let mut capture = None;
    let mut references = BTreeMap::new();
    let mut positions = BTreeSet::new();
    for row in rows {
        if source
            .target
            .database()
            .is_some_and(|database| !row.observation.database_context_is_consistent(&database.database))
        {
            return Err(invalid());
        }
        let Some(reference) = row.reference else { continue };
        if !reference.is_valid()
            || reference.source_scope_id != scope
            || reference.row_ordinal >= native.observations
            || capture.as_ref().is_some_and(|expected| *expected != reference.capture_ref())
            || !positions.insert((&reference.batch_id, reference.row_ordinal))
        {
            return Err(invalid());
        }
        let exported = ExportedRecord::new(source, &reference.capture, TelemetryRecord::from(row.observation.clone()))?;
        if DiagnosticEvidenceRef::from_export(source, &reference.manifest_sha256, &exported, reference.row_ordinal)?
            != *reference
        {
            return Err(invalid());
        }
        capture = Some(reference.capture_ref());
        references.insert(row.observation.key.clone(), reference);
    }
    if !references.is_empty() && references.len() != rows.len() {
        return Err(invalid());
    }
    Ok(references)
}

fn read_context(source: &CollectionDescription, native: &ActivitySnapshotDescription) -> NativeReadContext {
    NativeReadContext {
        target: source.target.clone(),
        partition: source.partition.clone(),
        observed_from: native.observed_from.clone(),
        observed_until: native.observed_until.clone(),
        database_writable: native.database_writable,
    }
}

fn invalid() -> Error {
    CollectionStop::InvalidPage.error()
}

#[cfg(test)]
pub(crate) mod tests;
