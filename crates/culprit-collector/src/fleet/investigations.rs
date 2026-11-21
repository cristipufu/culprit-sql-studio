//! Existing fleet admission owns targeted work; offline evidence never grants native authority by itself.
mod evidence;
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
        if is_baseline(job) {
            self.bindings.invalidate(&job.target);
        }
    }

    pub async fn completed_baseline(
        &mut self,
        job: &Job,
        result: &WorkResult,
        route: Option<&str>,
        outbox: &mut crate::outbox::Outbox,
    ) -> Result<()> {
        if shared_owner_baseline(job)
            && result.event.status == CollectionStatus::Complete
            && let (Some(capture), Some(source), Some(revision)) = (&result.event.capture, &result.source, route)
            && let Some((owner, verified, _)) = outbox.diagnostic_owner(capture).await?
        {
            if verified != *source {
                return Err(CollectionStop::InvalidPage.error());
            }
            self.bindings.observe_shared(&job.target, source, owner, revision)?;
        }
        if (matches!(job.schedule.collection, ScheduledCollection::Activity { .. })
            || DiagnosticSourceKind::supports_collection(job.schedule.collection.collection()))
            && result.event.status == CollectionStatus::Complete
            && let (Some(source), Some(revision)) = (&result.source, route)
            && RouteBindings::accepts_source(source)
        {
            self.bindings.observe(&job.target, source, revision)?;
        }
        Ok(())
    }
}

pub(super) fn is_baseline(job: &Job) -> bool {
    matches!(job.schedule.collection, ScheduledCollection::Activity { .. })
        || DiagnosticSourceKind::supports_collection(job.schedule.collection.collection())
        || shared_owner_baseline(job)
}

fn shared_owner_baseline(job: &Job) -> bool {
    matches!(job.subject, CollectionSubject::Shared { .. })
        && matches!(job.schedule.collection, ScheduledCollection::Snapshot { dataset: SnapshotDataset::Operational })
}
