//! Sanitized historical acquisition metadata, never native scope, collection admission or cost authority.
use crate::{Result, SourceOverheadCaptureSummary, SourceOverheadReport, i18n::Message, telemetry::*};
use serde::{Deserialize, Serialize};
use ts_rs::TS;

pub const MAX_RECORDED_SOURCE_ACQUISITIONS: usize = 32;
const MAX_SAVED_SUMMARY_BYTES: usize = 128 * 1024;
const FINGERPRINT_DOMAIN: &str = "culprit-sanitized-source-acquisition-v1";

#[derive(Serialize, Deserialize, TS, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum SourceAcquisitionSeal {
    Sealed,
    Provisional,
}

#[derive(Serialize, TS, Clone, Debug, PartialEq)]
#[ts(export)]
pub struct CapturedSourceAcquisition {
    pub manifest_sha256: String,
    pub seal: SourceAcquisitionSeal,
    pub summary: SourceOverheadCaptureSummary,
    /// Saved-body consistency only. Public metadata cannot restore original native proof.
    pub fingerprint: String,
}
impl CapturedSourceAcquisition {
    /// The caller must authenticate the exact original manifest bytes and journal association first.
    pub fn captured(
        manifest_sha256: String,
        seal: SourceAcquisitionSeal,
        summary: SourceOverheadCaptureSummary,
    ) -> Result<Self> {
        let mut value = Self { manifest_sha256, seal, summary, fingerprint: String::new() };
        if !value.has_valid_body() {
            return Err(invalid());
        }
        value.fingerprint = value.expected_fingerprint()?;
        value.is_valid().then_some(value).ok_or_else(invalid)
    }
    pub fn is_valid(&self) -> bool {
        self.has_valid_body()
            && valid_digest(&self.fingerprint)
            && self.expected_fingerprint().is_ok_and(|expected| expected == self.fingerprint)
    }
    fn has_valid_body(&self) -> bool {
        valid_digest(&self.manifest_sha256)
            && self.summary.capture.is_valid()
            && self.summary.is_valid()
            && (self.seal != SourceAcquisitionSeal::Sealed || self.summary.finished_at.is_some())
            && crate::serialization::json_size(self, MAX_SAVED_SUMMARY_BYTES).is_ok()
    }
    pub fn is_saveable(&self) -> bool {
        self.seal == SourceAcquisitionSeal::Sealed && self.is_valid()
    }
    fn expected_fingerprint(&self) -> Result<String> {
        serde_json::to_vec(&(FINGERPRINT_DOMAIN, &self.manifest_sha256, self.seal, &self.summary))
            .map(|bytes| digest(&bytes))
            .map_err(|_| invalid())
    }
}
#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
#[ts(export)]
pub enum RecordedSourceAcquisition {
    Captured { original: Box<CapturedSourceAcquisition> },
    Unavailable { reason: Message },
}
#[derive(Serialize, Deserialize, TS, Clone, Debug)]
#[serde(tag = "kind", rename_all = "kebab-case")]
#[ts(export)]
pub enum HistoricalSourceOverhead {
    Available { manifest_sha256: String, report: Box<SourceOverheadReport> },
    Unavailable { reason: String },
}
/// Reject conflicting bodies for the same original capture/revision. Other captures are independent history.
pub fn append_source_acquisition(
    originals: &mut Vec<RecordedSourceAcquisition>,
    original: Box<CapturedSourceAcquisition>,
) -> Result<()> {
    if !original.is_saveable() {
        return Err(invalid());
    }
    let mut repeated = false;
    for before in originals.iter() {
        if let RecordedSourceAcquisition::Captured { original: before } = before
            && before.summary.capture == original.summary.capture
        {
            if before.as_ref() != original.as_ref() || !before.is_saveable() {
                return Err(DeliveryProblem::Conflict.error());
            }
            repeated = true;
        }
    }
    if repeated {
        return Ok(());
    }
    if originals.len() >= MAX_RECORDED_SOURCE_ACQUISITIONS {
        return Err(CollectionStop::RowLimit.error());
    }
    originals.push(RecordedSourceAcquisition::Captured { original });
    Ok(())
}
/// Inspect every original before the presentation cap; a conflicting later copy withdraws its whole identity.
pub fn source_acquisition_positions(originals: &[RecordedSourceAcquisition]) -> Vec<Result<usize>> {
    use std::collections::{BTreeMap, btree_map::Entry};
    let mut captures = BTreeMap::new();
    for (position, original) in originals.iter().enumerate() {
        let RecordedSourceAcquisition::Captured { original } = original else { continue };
        match captures.entry(original_key(original)) {
            Entry::Vacant(entry) => {
                entry.insert((position, original.as_ref(), false));
            }
            Entry::Occupied(mut entry) => {
                let (_, before, conflicted) = entry.get_mut();
                *conflicted |= *before != original.as_ref();
            }
        }
    }
    originals
        .iter()
        .enumerate()
        .filter_map(|(position, original)| {
            let RecordedSourceAcquisition::Captured { original } = original else { return Some(Ok(position)) };
            let Some((first, _, conflicted)) = captures.get(&original_key(original)) else {
                return Some(Err(invalid()));
            };
            (*first == position)
                .then(|| if *conflicted { Err(DeliveryProblem::Conflict.error()) } else { Ok(position) })
        })
        .collect()
}
// Original capture revision equality, independent of database/member labels or the summary fingerprint.
type OriginalKey<'a> = (&'a str, Option<(&'a str, &'a str, &'a str)>);
fn original_key(original: &CapturedSourceAcquisition) -> OriginalKey<'_> {
    let CaptureIdentity { observation_id, revision } = &original.summary.capture;
    let revision = revision.as_ref().map(|CaptureRevision { outbox_id, owner_epoch, sequence }| {
        (outbox_id.as_str(), owner_epoch.as_str(), sequence.as_str())
    });
    (observation_id.as_str(), revision)
}

fn invalid() -> crate::Error {
    DeliveryProblem::Integrity.error()
}

impl<'de> Deserialize<'de> for CapturedSourceAcquisition {
    fn deserialize<D: serde::Deserializer<'de>>(reader: D) -> std::result::Result<Self, D::Error> {
        // Reject hidden fields at every nested curated boundary instead of discarding their private payloads.
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Wire {
            manifest_sha256: String,
            seal: SourceAcquisitionSeal,
            summary: SourceOverheadCaptureSummary,
            fingerprint: String,
        }
        let value = serde_json::Value::deserialize(reader)?;
        let wire: Wire = serde_json::from_value(value.clone()).map_err(serde::de::Error::custom)?;
        let original = Self {
            manifest_sha256: wire.manifest_sha256,
            seal: wire.seal,
            summary: wire.summary,
            fingerprint: wire.fingerprint,
        };
        if serde_json::to_value(&original).map_err(serde::de::Error::custom)? != value {
            return Err(serde::de::Error::custom("noncanonical source acquisition summary"));
        }
        Ok(original)
    }
}

#[cfg(test)]
mod tests;
