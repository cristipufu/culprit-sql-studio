//! Original query-history wait fragments retain one immutable acquisition; they do not describe a current scan.
use super::*;
use std::collections::BTreeSet;

pub const MAX_CAPTURED_QUERY_WAIT_ROWS: usize = 100_000;

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct CapturedQueryWaitRow {
    pub reference: CapturedRecordReference,
    pub wait: WaitMeasurement,
}

/// The archive/local owner authenticates original file membership before constructing this value.
/// Validation binds unchanged canonical bytes; it grants no source-read rights or current native continuity.
#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct CapturedQueryWaitRead {
    pub description: CollectionDescription,
    pub read: CapturedReadRef,
    pub reference_coverage: ReferenceCoverage,
    /// Collector acquisition clocks, separate from the source's historical statistics interval.
    pub collected_from: String,
    pub collected_until: String,
    pub rows: Vec<CapturedQueryWaitRow>,
    pub read_complete: bool,
}
impl CapturedQueryWaitRead {
    /// Consume already authenticated original rows and their zero-based stream ordinals. This does not issue
    /// new native evidence: each unchanged canonical body must satisfy its original capture/source binding.
    pub fn from_exports(
        description: CollectionDescription,
        read: CapturedReadRef,
        reference_coverage: ReferenceCoverage,
        collected_bounds: (String, String),
        rows: impl IntoIterator<Item = (ExportedRecord, u32)>,
        read_complete: bool,
    ) -> Result<Self> {
        let binding = read.bind(&description)?;
        let rows = rows
            .into_iter()
            .map(|(row, ordinal)| {
                let reference = binding.reference(&row, ordinal)?;
                let TelemetryRecord::QueryWaits(wait) = row.record else {
                    return Err(CollectionStop::InvalidPage.error());
                };
                Ok(CapturedQueryWaitRow { reference, wait })
            })
            .collect::<Result<Vec<_>>>()?;
        let value = Self {
            description,
            read,
            reference_coverage,
            collected_from: collected_bounds.0,
            collected_until: collected_bounds.1,
            rows,
            read_complete,
        };
        value.is_valid().then_some(value).ok_or_else(|| CollectionStop::InvalidPage.error())
    }

    pub fn is_valid(&self) -> bool {
        let Some(selected) = self.read.captured_rows(TelemetryStream::QueryWaits) else { return false };
        if !matches!(self.description.target, CollectionTarget::Database(_))
            || self.description.partition != SourcePartition::Target
            || self.description.query_history().is_none_or(|history| !history.is_valid())
            || ObservationBounds::new(&self.collected_from, &self.collected_until).is_none()
            || self.read.streams.len() != 1
            || !self.reference_coverage.is_valid()
            || self.rows.len() > MAX_CAPTURED_QUERY_WAIT_ROWS
            || self.rows.len() > selected as usize
            || self.read_complete && self.rows.len() != selected as usize
        {
            return false;
        }
        let Ok(original) = self.read.bind(&self.description) else { return false };
        let mut positions = BTreeSet::new();
        self.rows.iter().all(|row| {
            row.wait.is_valid()
                && positions.insert(row.reference.row_ordinal())
                && (!row.reference.is_informational() || !self.reference_coverage.complete)
                && original
                    .validate_canonical_record(&row.reference, &TelemetryRecord::QueryWaits(row.wait.clone()))
                    .is_ok()
        })
    }
}

/// Sharing withdraws the whole original; redacted native labels never become a replacement receipt.
#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
#[ts(export)]
pub enum RecordedQueryWaitRead {
    Captured { read: Box<CapturedQueryWaitRead> },
    Unavailable { reason: crate::i18n::Message },
}
