//! Local activity-monitor configuration and coverage output; scheduling is owned by the controller.
use super::*;

pub const MIN_ACTIVITY_POLL_SECONDS: u32 = 60;
#[derive(Serialize, Deserialize, TS, Clone, Debug)]
#[ts(export)]
pub struct ActivityMonitorOptions {
    pub poll_seconds: u32,
    pub max_polls: Option<u32>,
    pub policy: ActivityPolicy,
    pub budget: CollectionBudget,
}
impl Default for ActivityMonitorOptions {
    fn default() -> Self {
        Self {
            poll_seconds: MIN_ACTIVITY_POLL_SECONDS,
            max_polls: None,
            policy: ActivityPolicy::default(),
            budget: CollectionBudget::default(),
        }
    }
}
impl ActivityMonitorOptions {
    pub fn validate(&self) -> Result<()> {
        if !(MIN_ACTIVITY_POLL_SECONDS..=86_400).contains(&self.poll_seconds) || self.max_polls == Some(0) {
            return Err(Error::invalid(t!("activity.invalid_schedule")));
        }
        self.policy.validate()?;
        self.budget.validate()
    }
}
#[derive(Serialize, Deserialize, TS, Clone, Debug)]
#[ts(export)]
pub struct ActivityMonitorEvent {
    pub attempt: u32,
    pub status: CollectionStatus,
    pub error_code: Option<ErrorCode>,
    pub capture: Option<CaptureIdentity>,
    pub observed_at: Option<String>,
    pub active_episodes: Option<u32>,
    pub uncertain_episodes: Option<u32>,
    pub native_coverage: Option<StateSourceCoverage>,
    pub source_queries: Option<u32>,
}
#[derive(Serialize, Deserialize, TS, Clone, Debug, Default)]
#[ts(export)]
pub struct ActivityMonitorSummary {
    pub attempts: u32,
    pub complete: u32,
    pub unknown: u32,
}
