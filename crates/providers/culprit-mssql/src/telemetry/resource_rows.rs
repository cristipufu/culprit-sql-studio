//! Export pagination owns stream position and receipt counts; native resources are already decoded and validated.
use crate::query_store::decode;
use culprit_core::{Result, telemetry::*};
#[derive(Default)]
pub(super) struct ResourcePager {
    position: Option<String>,
    rows: u32,
    finished: bool,
}
impl ResourcePager {
    pub(super) fn check_cursor(&self, cursor: Option<&str>) -> Result<()> {
        if self.finished || cursor != self.position.as_deref() {
            return Err(decode::malformed());
        }
        Ok(())
    }
    pub(super) fn accept(&mut self, rows: Vec<ResourceMeasurement>, limit: u32) -> Result<CollectionRead> {
        if limit == 0 || rows.len() > limit as usize + 1 {
            return Err(decode::malformed());
        }
        let more = rows.len() > limit as usize;
        let mut records = Vec::new();
        for sample in &rows {
            if !sample.is_valid() {
                return Err(decode::malformed());
            }
            if records.len() < limit as usize {
                records.push(TelemetryRecord::ResourceSample(sample.clone()));
            }
        }
        let next = records.last().and_then(|row| match row {
            TelemetryRecord::ResourceSample(row) => Some(row.at.clone()),
            _ => None,
        });
        self.rows = self.rows.checked_add(records.len() as u32).ok_or_else(decode::malformed)?;
        self.position = next;
        self.finished = !more;
        Ok(CollectionRead::Page { records, next_cursor: if more { self.position.clone() } else { None } })
    }
    pub(super) fn complete(&self, expected: u32) -> bool {
        self.finished && self.rows == expected
    }
}
