//! Canonical OperationalSamples snapshot positions; independent of live observations and native read rights.
use super::*;
use std::collections::{BTreeMap, BTreeSet, btree_map::Entry};

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct CapturedOperationalRead {
    pub origin: CapturedSnapshotOrigin,
    pub description: CollectionDescription,
    pub observations: Vec<OperationalObservation>,
    /// Body completeness is independent of each original native source's visibility.
    pub read_complete: bool,
}
impl CapturedOperationalRead {
    /// The caller authenticates the exact original capture, manifest and row membership before conversion.
    /// No new receipt is issued, and a partial retained body never rewrites original native counts.
    pub fn captured(
        description: CollectionDescription,
        capture: DiagnosticCaptureRef,
        positions: Vec<CapturedSnapshotPosition>,
        observations: Vec<OperationalObservation>,
        read_complete: bool,
    ) -> Result<Self> {
        let origin = CapturedSnapshotOrigin::captured(&description, capture, positions)?;
        let read = Self { origin, description, observations, read_complete };
        read.is_valid().then_some(read).ok_or_else(invalid)
    }

    pub fn is_valid(&self) -> bool {
        self.accepts_rows() && self.origin.validate(&self.description, self.records(), self.read_complete).is_ok()
    }

    pub fn context(&self) -> Result<NativeReadContext> {
        NativeReadContext::from_snapshot(&self.description)
    }

    pub fn source_coverage(&self, name: &str) -> Option<&StateSourceCoverage> {
        self.description.observation.snapshot()?.sources.iter().find(|coverage| coverage.source == name)
    }

    pub fn captured_references(&self) -> Result<Vec<DiagnosticEvidenceRef>> {
        if !self.accepts_rows() {
            return Err(invalid());
        }
        self.origin.references(&self.description, self.records(), self.read_complete)
    }

    fn records(&self) -> impl Iterator<Item = TelemetryRecord> + '_ {
        self.observations.iter().cloned().map(TelemetryRecord::Operational)
    }

    fn accepts_rows(&self) -> bool {
        let Some(header) = self.description.observation.snapshot() else { return false };
        if header.dataset.stream() != TelemetryStream::OperationalSamples
            || !header.is_valid()
            || !self.description.target.is_valid()
            || !self.description.partition_is_consistent()
            || !self.description.observation.covers_request()
            || !valid_key(&self.description.source)
            || header.records > CollectionBudget::upper_limits().max_snapshot_rows
        {
            return false;
        }
        let mut counts = header
            .sources
            .iter()
            .map(|coverage| (coverage.source.as_str(), (coverage, 0usize)))
            .collect::<BTreeMap<_, _>>();
        let mut keys = BTreeSet::new();
        for row in &self.observations {
            let Some((coverage, count)) = counts.get_mut(row.source.as_str()) else { return false };
            if !row.is_valid()
                || !matches!(coverage.state, crate::SourceState::Collected | crate::SourceState::Partial)
                || !keys.insert((row.source.as_str(), row.key.as_str()))
                || *count >= coverage.records as usize
            {
                return false;
            }
            *count += 1;
        }
        counts.values().all(|(coverage, count)| {
            *count <= coverage.records as usize && (!self.read_complete || *count == coverage.records as usize)
        })
    }
}
fn invalid() -> Error {
    CollectionStop::InvalidPage.error()
}

#[cfg(test)]
mod tests;

/// A saved family retains complete original evidence or an explicit withdrawal; no masked receipt is issued.
#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
#[ts(export)]
pub enum RecordedOperationalRead {
    Captured { read: Box<CapturedOperationalRead> },
    Unavailable { reason: crate::i18n::Message },
}

/// Bounded saved attachments do not change the Outbox/store retention or native collection limits.
pub const MAX_RECORDED_OPERATIONAL_READS: usize = 32;
impl RecordedOperationalRead {
    /// Classify every retained position before presentation limits. Exact repetitions are one original;
    /// conflicting copies withdraw the whole capture identity, including a conflict after a display prefix.
    /// This validates collection identity only: the family still validates its native source and body.
    pub fn replay_positions(originals: &[Self]) -> Vec<Result<usize>> {
        let index = OriginalIndex::new(originals);
        originals
            .iter()
            .enumerate()
            .filter_map(|(position, original)| match original {
                Self::Unavailable { .. } => Some(Ok(position)),
                Self::Captured { read } => {
                    let Some(entry) = index.captures.get(&original_key(read)) else {
                        return Some(Err(invalid()));
                    };
                    (entry.position == position)
                        .then(|| if entry.conflicted { Err(DeliveryProblem::Conflict.error()) } else { Ok(position) })
                }
            })
            .collect()
    }
}

pub fn append_recorded_operational_read(
    originals: &mut Vec<RecordedOperationalRead>,
    read: Box<CapturedOperationalRead>,
) -> Result<()> {
    if !read.is_valid() {
        return Err(invalid());
    }
    if !OriginalIndex::new(originals).requires_append(&read)? {
        return Ok(());
    }
    if originals.len() >= MAX_RECORDED_OPERATIONAL_READS {
        return Err(CollectionStop::RowLimit.error());
    }
    originals.push(RecordedOperationalRead::Captured { read });
    Ok(())
}

// The closed original-capture key mirrors equality, not a database/member/table-name join. Exhaustive
// destructuring keeps a future capture-identity field from silently falling outside this one policy.
type OriginalKey<'a> = (&'a str, Option<(&'a str, &'a str, &'a str)>);
fn original_key(read: &CapturedOperationalRead) -> OriginalKey<'_> {
    let CaptureIdentity { observation_id, revision } = &read.origin.capture.capture;
    let revision = revision.as_ref().map(|CaptureRevision { outbox_id, owner_epoch, sequence }| {
        (outbox_id.as_str(), owner_epoch.as_str(), sequence.as_str())
    });
    (observation_id.as_str(), revision)
}
struct OriginalEntry<'a> {
    position: usize,
    read: &'a CapturedOperationalRead,
    conflicted: bool,
}
struct OriginalIndex<'a> {
    captures: BTreeMap<OriginalKey<'a>, OriginalEntry<'a>>,
}
impl<'a> OriginalIndex<'a> {
    fn new(originals: &'a [RecordedOperationalRead]) -> Self {
        let mut captures: BTreeMap<OriginalKey<'a>, OriginalEntry<'a>> = BTreeMap::new();
        for (position, original) in originals.iter().enumerate() {
            let RecordedOperationalRead::Captured { read } = original else { continue };
            match captures.entry(original_key(read)) {
                Entry::Vacant(entry) => {
                    entry.insert(OriginalEntry { position, read, conflicted: false });
                }
                Entry::Occupied(mut entry) => {
                    let entry = entry.get_mut();
                    entry.conflicted |= entry.read != read.as_ref();
                }
            }
        }
        Self { captures }
    }
    fn requires_append(&self, read: &CapturedOperationalRead) -> Result<bool> {
        match self.captures.get(&original_key(read)) {
            None => Ok(true),
            Some(entry) if !entry.conflicted && entry.read == read => Ok(false),
            Some(_) => Err(DeliveryProblem::Conflict.error()),
        }
    }
}
