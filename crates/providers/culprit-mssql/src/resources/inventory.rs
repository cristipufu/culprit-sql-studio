use super::{MAX_RETAINED_SAMPLES, SAMPLE_PERIOD_MS};
use crate::{
    read::{integer, invalid_data, text},
    value::{Rec, Val},
};
use culprit_core::{Result, telemetry::*};
pub(crate) struct Inventory {
    pub history: SampleHistoryDescription,
    pub replica_role: Option<i64>,
    first: Option<String>,
    last: Option<String>,
}
impl Inventory {
    pub(super) fn decode(row: &Rec, window: &super::Window) -> Result<Self> {
        let mut value = Self::decode_history(row, window, Some(SAMPLE_PERIOD_MS))?;
        if integer(row, "retained_count")? > 0 {
            value.replica_role = Some(integer(row, "replica_role")?);
        }
        Ok(value)
    }
    pub(super) fn shared(row: &Rec, window: &super::Window) -> Result<Self> {
        Self::decode_history(row, window, None)
    }
    fn decode_history(row: &Rec, window: &super::Window, period_ms: Option<u32>) -> Result<Self> {
        let expected_samples = u32::try_from(integer(row, "selected_count")?).map_err(|_| invalid_data())?;
        let retained = integer(row, "retained_count")?;
        if retained < 0 {
            return Err(invalid_data());
        }
        if retained > i64::from(MAX_RETAINED_SAMPLES) {
            return Err(CollectionStop::RowLimit.error());
        }
        if i64::from(expected_samples) > retained {
            return Err(invalid_data());
        }
        let value = Self {
            replica_role: None,
            history: SampleHistoryDescription {
                from: window.from.to_rfc3339(),
                until: window.until.to_rfc3339(),
                observed_at: canonical_time(&text(row, "observed_at")?)?,
                available_from: timestamp(row, "available_from")?,
                available_until: timestamp(row, "available_until")?,
                expected_samples,
                nominal_period_ms: period_ms,
            },
            first: timestamp(row, "selected_first")?,
            last: timestamp(row, "selected_last")?,
        };
        if (expected_samples == 0) != (value.first.is_none() && value.last.is_none())
            || value.first.is_some() != value.last.is_some()
        {
            return Err(invalid_data());
        }
        Ok(value)
    }
    pub(crate) fn same_rows(&self, other: &Self) -> bool {
        self.history.expected_samples == other.history.expected_samples
            && self.first == other.first
            && self.last == other.last
    }
}
fn timestamp(row: &Rec, name: &str) -> Result<Option<String>> {
    match row.columns().find(|(column, _)| *column == name).map(|(_, value)| value) {
        Some(Val::Null) => Ok(None),
        Some(Val::Text(text)) => canonical_time(text).map(Some),
        _ => Err(invalid_data()),
    }
}
