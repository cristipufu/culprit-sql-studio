//! Native waiting-task fields are point evidence. No task address or blocking session number supplies a
//! generation, a SQL owner, a continuously held latch or an application-level causal relationship.
use super::*;

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[ts(export)]
pub struct MssqlPageTaskWait {
    pub observed_at: String,
    pub session_id: u32,
    pub request_id: u32,
    pub execution_context_id: u32,
    pub task_address: String,
    pub wait_type: String,
    pub wait_duration_ms: Option<String>,
    pub resource: ObservedWaitResource,
    pub resource_address: Option<String>,
    pub blocking_task_address: Option<String>,
    /// Preserve negative native sentinels and unknown future values without attaching a current session.
    pub blocking_session_id: Option<i32>,
    pub blocking_execution_context_id: Option<i32>,
}

impl MssqlPageTaskWait {
    pub fn is_valid(&self) -> bool {
        let kind = if self.wait_type.starts_with("PAGELATCH_") {
            WaitResourceKind::MemoryPage
        } else if self.wait_type.starts_with("PAGEIOLATCH_") {
            WaitResourceKind::PageIo
        } else {
            return false;
        };
        canonical_time(&self.observed_at).is_ok()
            && self.session_id > 0
            && self.session_id <= i16::MAX as u32
            && self.request_id <= i32::MAX as u32
            && self.execution_context_id <= i32::MAX as u32
            && address(&self.task_address)
            && self.task_address.bytes().any(|byte| byte != b'0')
            && self.wait_type.len() <= 60
            && self
                .wait_duration_ms
                .as_deref()
                .is_none_or(|value| valid_counter(value) && value.parse::<i64>().is_ok_and(|value| value >= 0))
            && self.resource.is_valid()
            && self.resource.kind == kind
            && self.resource.native.qualifiers.get("source").map(String::as_str)
                == Some("sys.dm_os_waiting_tasks.resource_description")
            && [&self.resource_address, &self.blocking_task_address].into_iter().flatten().all(|value| address(value))
            && self.blocking_session_id.is_none_or(|value| i16::try_from(value).is_ok())
    }

    pub(super) fn matches_request(&self, request: &ActivityObservation) -> bool {
        self.matches_activity_key(&request.key)
    }

    pub(crate) fn matches_activity_key(&self, key: &ActivityKey) -> bool {
        key.session.id.parse::<u32>().ok() == Some(self.session_id)
            && key.request.as_ref().and_then(|key| key.id.parse::<u32>().ok()) == Some(self.request_id)
    }
}

fn address(value: &str) -> bool {
    value.len() == 16 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}
