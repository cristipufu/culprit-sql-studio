//! One closed original-history carrier for native runtime and wait fragments; no live-source authority.
use super::*;
use std::collections::BTreeSet;

pub const MAX_CAPTURED_QUERY_HISTORY_ROWS: usize = 100_000;
#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq)]
#[serde(tag = "kind", content = "data", rename_all = "kebab-case", deny_unknown_fields)]
#[ts(export)]
pub enum QueryHistoryFragment {
    Runtime(Box<RuntimeMeasurement>),
    Waits(Box<WaitMeasurement>),
}
impl QueryHistoryFragment {
    pub fn stream(&self) -> TelemetryStream {
        match self {
            Self::Runtime(_) => TelemetryStream::QueryRuntime,
            Self::Waits(_) => TelemetryStream::QueryWaits,
        }
    }
    pub fn runtime(&self) -> Option<&RuntimeMeasurement> {
        match self {
            Self::Runtime(row) => Some(row),
            Self::Waits(_) => None,
        }
    }
    pub fn wait(&self) -> Option<&WaitMeasurement> {
        match self {
            Self::Waits(row) => Some(row),
            Self::Runtime(_) => None,
        }
    }
    fn record(&self) -> TelemetryRecord {
        match self {
            Self::Runtime(row) => TelemetryRecord::QueryRuntime(row.as_ref().clone()),
            Self::Waits(row) => TelemetryRecord::QueryWaits(row.as_ref().clone()),
        }
    }
    fn from_record(record: TelemetryRecord) -> Result<Self> {
        match record {
            TelemetryRecord::QueryRuntime(row) => Ok(Self::Runtime(Box::new(row))),
            TelemetryRecord::QueryWaits(row) => Ok(Self::Waits(Box::new(row))),
            _ => Err(CollectionStop::InvalidPage.error()),
        }
    }
}
#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct CapturedQueryHistoryRow {
    pub reference: CapturedRecordReference,
    pub fragment: QueryHistoryFragment,
}

/// An archive/local owner authenticates immutable source, original membership and acquisition metadata first.
/// Structural replay binds those unchanged bytes; it grants no native read, current ownership or causal proof.
#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct CapturedQueryHistoryRead {
    pub description: CollectionDescription,
    pub read: CapturedReadRef,
    pub reference_coverage: ReferenceCoverage,
    pub collected_from: String,
    pub collected_until: String,
    pub rows: Vec<CapturedQueryHistoryRow>,
    pub read_complete: bool,
}
impl CapturedQueryHistoryRead {
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
                Ok(CapturedQueryHistoryRow { reference, fragment: QueryHistoryFragment::from_record(row.record)? })
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
    pub fn stream(&self) -> Option<TelemetryStream> {
        match self.read.streams.as_slice() {
            [selected] if matches!(selected.stream, TelemetryStream::QueryRuntime | TelemetryStream::QueryWaits) => {
                Some(selected.stream)
            }
            _ => None,
        }
    }
    pub fn is_valid(&self) -> bool {
        let Some(stream) = self.stream() else { return false };
        let Some(selected) = self.read.captured_rows(stream) else { return false };
        if !matches!(self.description.target, CollectionTarget::Database(_))
            || self.description.partition != SourcePartition::Target
            || self.description.query_history().is_none_or(|history| !history.is_valid())
            || ObservationBounds::new(&self.collected_from, &self.collected_until).is_none()
            || !self.reference_coverage.is_valid()
            || self.rows.len() > MAX_CAPTURED_QUERY_HISTORY_ROWS
            || self.rows.len() > selected as usize
            || self.read_complete && self.rows.len() != selected as usize
        {
            return false;
        }
        let Ok(original) = self.read.bind(&self.description) else { return false };
        let mut positions = BTreeSet::new();
        self.rows.iter().all(|row| {
            row.fragment.stream() == stream
                && positions.insert(row.reference.row_ordinal())
                && (!row.reference.is_informational() || !self.reference_coverage.complete)
                && original.validate_canonical_record(&row.reference, &row.fragment.record()).is_ok()
        })
    }
}
#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
#[ts(export)]
pub enum RecordedQueryHistoryRead {
    Captured { read: Box<CapturedQueryHistoryRead> },
    Unavailable { reason: crate::i18n::Message },
}
