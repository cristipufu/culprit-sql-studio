//! Existing fleet admission owns targeted work; offline evidence never grants native authority by itself.
mod execution;
mod planning;
mod queue;
mod routes;

pub(super) use routes::RouteBindings;
#[cfg(test)]
mod tests;

use super::{
    scheduler::{FocusedJob, Job},
    work::WorkResult,
};
use culprit_core::{Result, telemetry::*};

pub(super) struct Investigations {
    targets: Vec<FleetTarget>,
    bindings: RouteBindings,
    maintenance: queue::Maintenance,
}

impl Investigations {
    pub fn new(inventory: &FleetInventory, shard: &str) -> Self {
        Self {
            targets: inventory.targets.iter().filter(|target| target.shard == shard).cloned().collect(),
            bindings: RouteBindings::default(),
            maintenance: queue::Maintenance::default(),
        }
    }

    pub fn route_available(&self, job: &FocusedJob) -> bool {
        self.bindings.get(&job.target).is_some()
    }

    pub fn planning_time(&self) -> std::time::Duration {
        planning::PLANNING_TIME
    }

    pub fn starting_baseline(&mut self, job: &Job) {
        if matches!(job.schedule.collection, ScheduledCollection::Activity { .. }) {
            self.bindings.invalidate(&job.target);
        }
    }

    pub fn completed_baseline(&mut self, job: &Job, result: &WorkResult, route: Option<&str>) -> Result<()> {
        if matches!(job.schedule.collection, ScheduledCollection::Activity { .. })
            && result.event.status == CollectionStatus::Complete
            && let (Some(source), Some(revision)) = (&result.source, route)
            && source.observation.activity().is_some_and(|frame| frame.epoch.is_some())
        {
            self.bindings.observe(&job.target, source, revision)?;
        }
        Ok(())
    }
}
