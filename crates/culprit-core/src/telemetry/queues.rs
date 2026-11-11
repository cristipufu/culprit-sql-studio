//! Current queue gauges require native request-level database attribution.
use super::*;

#[derive(Serialize, Deserialize, TS, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum QueueKind {
    MemoryGrants,
    WorkerTasks,
}
#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[ts(export)]
pub struct QueueSample {
    pub kind: QueueKind,
    /// Native queue members confirmed in `scope`; grants and tasks are different counting units.
    pub count: String,
    pub scope: crate::MeasurementScope,
    /// Members of the shared native queue whose request database is unavailable. Coverage metadata only:
    /// these are not this database's measurements and must not be summed across database captures.
    pub unattributed_count: String,
    pub longest_wait_ms: Option<String>,
    /// Native selection threshold, including when an observed count is zero.
    pub minimum_wait_ms: u32,
}
impl QueueSample {
    pub fn attribution_complete(&self) -> bool {
        self.unattributed_count.parse::<u64>() == Ok(0)
    }
    pub fn is_valid(&self) -> bool {
        valid_counter(&self.count)
            && valid_counter(&self.unattributed_count)
            && self.longest_wait_ms.as_deref().is_none_or(valid_counter)
            && (self.count.parse::<u64>() != Ok(0) || self.longest_wait_ms.is_none())
            && self
                .longest_wait_ms
                .as_deref()
                .is_none_or(|value| value.parse::<u64>().is_ok_and(|value| value >= u64::from(self.minimum_wait_ms)))
    }
}
