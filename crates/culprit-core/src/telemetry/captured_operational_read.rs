//! Canonical OperationalSamples snapshot positions; independent of live observations and native read rights.
use super::*;
use std::collections::{BTreeMap, BTreeSet};

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
