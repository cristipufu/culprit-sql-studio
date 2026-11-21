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
        if matches!(&scope.population, DiagnosticPopulation::Source { .. }) {
            return SourceReadExecutionScope::context_from_source(&self.source).is_ok_and(|context| {
                if !context.has_qualified_local_owner()
                    || context.database_writable.is_none()
                    || context.target != scope.target
                {
                    return false;
                }
                match scope.partition {
                    // Historical logical data has no proven executor. Its independently qualified acquisition
                    // owner may route a new read without becoming that historical population's member/clock.
                    SourcePartition::Target => NativeSourceReadScope::from_context(&context).is_ok(),
                    SourcePartition::InstanceLocal { .. } => context.partition == scope.partition,
                }
            });
        }
        DiagnosticScope::from_source(&self.source).is_ok_and(|current| {
            current.native_epoch.is_some() && current.partition.is_qualified() && current.key().ok() == scope.key().ok()
        })
    }

    pub fn observed_after(&self, at: &str) -> bool {
        if self.shared_owner.is_some() {
            return self
                .source
                .observation
                .snapshot()
                .is_some_and(|snapshot| ObservationBounds::new(at, &snapshot.observed_until).is_some());
        }
        SourceReadExecutionScope::context_from_source(&self.source)
            .is_ok_and(|context| ObservationBounds::new(at, &context.observed_until).is_some())
    }

    pub fn budget<'a>(&self, target: &'a FleetTarget) -> Option<&'a CollectionBudget> {
        target.schedules.iter().find_map(|schedule| {
            let matches = schedule.collection.collection() == self.source.observation.collection()
                && (self.shared_owner.is_some() || RouteBindings::accepts_source(&self.source));
            matches.then_some(&schedule.budget)
        })
    }
}

impl RouteBindings {
    /// Activity keeps its native frame authority. Source-only questions additionally need a known acquisition
    /// role; retained logical history must qualify that acquisition independently of its historical population.
    pub fn accepts_source(source: &CollectionDescription) -> bool {
        let Ok(scope) = DiagnosticScope::from_source(source) else { return false };
        let local_scope = scope.native_epoch.is_some() && scope.partition.is_qualified();
        let context = SourceReadExecutionScope::context_from_source(source);
        let acquired_history = matches!(source.observation, SourceObservation::QueryHistory(_))
            && context.as_ref().is_ok_and(|context| NativeSourceReadScope::from_context(context).is_ok());
        let native_authority = source.observation.activity().is_some()
            || context.is_ok_and(|context| context.has_qualified_local_owner() && context.database_writable.is_some());
        native_authority && (local_scope || acquired_history)
    }

    pub fn get(&self, target: &str) -> Option<&RouteBinding> {
        self.routes.get(target)
    }

    pub fn invalidate(&mut self, target: &str) {
        self.routes.remove(target);
    }

    pub fn observe(&mut self, target: &str, source: &CollectionDescription, revision: &str) -> Result<()> {
        if !Self::accepts_source(source)
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
