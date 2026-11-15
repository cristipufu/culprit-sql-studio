//! Fresh native observations bind a saved route only for this runner activation.
use culprit_core::{Result, telemetry::*};
use std::{collections::BTreeMap, sync::Mutex};

#[derive(Default)]
pub(in crate::fleet) struct RouteBindings {
    routes: BTreeMap<String, RouteBinding>,
    planning: Mutex<(Option<String>, usize)>,
}

pub(in crate::fleet) struct RouteBinding {
    pub source: CollectionDescription,
    pub revision: String,
    pub shared_owner: Option<DiagnosticEvidenceRef>,
}

impl RouteBinding {
    pub fn matches_scope(&self, scope: &RecipeCostScope) -> bool {
        scope.matches_source(&self.source)
    }

    pub fn matches_episode(&self, scope: &DiagnosticScope) -> bool {
        if let CollectionTarget::Shared(resource) = &self.source.target {
            return self.shared_owner.is_some()
                && self.source.is_partition_qualified()
                && self.source.observation.snapshot().is_some()
                && matches!(&scope.partition, SourcePartition::InstanceLocal { member: Some(member), .. }
                    if member == resource);
        }
        DiagnosticScope::from_source(&self.source).is_ok_and(|current| {
            current.native_epoch.is_some() && current.partition.is_qualified() && current.key().ok() == scope.key().ok()
        })
    }

    pub fn observed_after(&self, at: &str) -> bool {
        let until = self
            .source
            .observation
            .activity()
            .map(|frame| frame.at.as_str())
            .or_else(|| self.source.observation.snapshot().map(|frame| frame.observed_until.as_str()));
        until.is_some_and(|until| ObservationBounds::new(at, until).is_some())
    }

    pub fn budget<'a>(&self, target: &'a FleetTarget) -> Option<&'a CollectionBudget> {
        target.schedules.iter().find_map(|schedule| {
            let matches = match (&schedule.collection, &self.source.observation) {
                (ScheduledCollection::Activity { .. }, SourceObservation::Activity(_)) => true,
                (ScheduledCollection::Snapshot { dataset }, SourceObservation::Snapshot(snapshot)) => {
                    self.shared_owner.is_some() && dataset == &snapshot.dataset
                }
                _ => false,
            };
            matches.then_some(&schedule.budget)
        })
    }
}

impl RouteBindings {
    pub fn get(&self, target: &str) -> Option<&RouteBinding> {
        self.routes.get(target)
    }

    pub fn invalidate(&mut self, target: &str) {
        self.routes.remove(target);
    }

    pub fn observe(&mut self, target: &str, source: &CollectionDescription, revision: &str) -> Result<()> {
        let scope = DiagnosticScope::from_source(source)?;
        if scope.native_epoch.is_none()
            || !scope.partition.is_qualified()
            || revision.is_empty()
            || revision.len() > 256
            || revision.chars().any(char::is_control)
        {
            return Err(CollectionStop::InvalidPage.error());
        }
        self.routes.insert(
            target.into(),
            RouteBinding { source: source.clone(), revision: revision.into(), shared_owner: None },
        );
        Ok(())
    }

    pub fn observe_shared(
        &mut self,
        target: &str,
        source: &CollectionDescription,
        owner: DiagnosticEvidenceRef,
        revision: &str,
    ) -> Result<()> {
        let CollectionTarget::Shared(resource) = &source.target else {
            return Err(CollectionStop::InvalidPage.error());
        };
        let scope = RecipeCostScope::SharedResource {
            target: resource.clone(),
            partition: source.partition.clone(),
            source: source.source.clone(),
        };
        if !scope.matches_source(source)
            || !owner.is_valid()
            || owner.stream != TelemetryStream::OperationalSamples
            || source.captured_range()?.scope_id != owner.source_scope_id
            || revision.is_empty()
            || revision.len() > 256
            || revision.chars().any(char::is_control)
        {
            return Err(CollectionStop::InvalidPage.error());
        }
        self.routes.insert(
            target.into(),
            RouteBinding { source: source.clone(), revision: revision.into(), shared_owner: Some(owner) },
        );
        Ok(())
    }

    pub fn planning_position(&self) -> (Option<String>, usize) {
        self.planning.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone()
    }

    pub fn set_planning_position(&self, head: Option<String>, episode: usize) {
        // This cursor supplies fairness only. It never supplies route identity, qualification or native permission.
        *self.planning.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = (head, episode);
    }
}
