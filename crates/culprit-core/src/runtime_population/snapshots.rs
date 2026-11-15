//! One native acquisition can supply independent workloads without making the populations interchangeable.
use super::*;
use crate::{Result, Snapshot, SnapshotOptions, telemetry::CollectionStop};

/// An exact requested snapshot and separately selected snapshots from the same native acquisition.
/// Providers acquire shared sources once; each returned snapshot remains an independently replayable pure input.
#[derive(Serialize, Deserialize, TS, Clone, Debug)]
#[ts(export)]
pub struct PopulationSnapshots {
    pub requested: Snapshot,
    pub populations: Vec<Snapshot>,
    pub catalog: Option<RuntimePopulationCatalog>,
}

impl PopulationSnapshots {
    /// A provider without a population dimension performs its existing single native acquisition.
    pub fn single(mut snapshot: Snapshot, options: &SnapshotOptions) -> Result<Self> {
        snapshot.options.get_or_insert_with(|| options.clone());
        let catalog = snapshot
            .runtime_populations
            .clone()
            .or_else(|| snapshot.workload.as_ref().and_then(|workload| workload.population_catalog.clone()));
        snapshot.runtime_populations = catalog.clone();
        let populations = if options.workload_population.is_none()
            && let Some(population) = snapshot.workload.as_ref().and_then(|workload| workload.population.clone())
        {
            let mut selected = snapshot.clone();
            selected.options = Some(SnapshotOptions { workload_population: Some(population), ..options.clone() });
            vec![selected]
        } else {
            Vec::new()
        };
        let batch = Self { requested: snapshot, populations, catalog };
        batch.validate(options)?;
        Ok(batch)
    }

    /// Validate exact namespaces before any analysis or cache mutation. Missing workloads remain explicit failures;
    /// a different population or a malformed catalog must never replace the requested result.
    pub fn validate(&self, options: &SnapshotOptions) -> Result<()> {
        if self.populations.len() > MAX_RUNTIME_POPULATIONS
            || !same_options(self.requested.options.as_ref(), options)
            || self.catalog.as_ref().is_some_and(|catalog| !catalog.is_valid())
            || self.requested.runtime_populations != self.catalog
            || !workload_matches(&self.requested, options.workload_population.as_ref(), self.catalog.as_ref())
        {
            return Err(invalid());
        }
        if options.workload_population.is_some() {
            return if self.populations.is_empty() { Ok(()) } else { Err(invalid()) };
        }
        let expected = self
            .catalog
            .iter()
            .flat_map(|catalog| &catalog.populations)
            .map(|population| &population.identity)
            .collect::<BTreeSet<_>>();
        let mut observed = BTreeSet::new();
        let mut native_window = self
            .requested
            .workload
            .as_ref()
            .filter(|workload| workload.windowed)
            .map(|workload| (&workload.window_start, &workload.window_end, workload.window_hours));
        for snapshot in &self.populations {
            let Some(population) = snapshot.options.as_ref().and_then(|options| options.workload_population.as_ref())
            else {
                return Err(invalid());
            };
            let selected = SnapshotOptions { workload_population: Some(population.clone()), ..options.clone() };
            if !expected.contains(population)
                || !observed.insert(population)
                || !same_options(snapshot.options.as_ref(), &selected)
                || !snapshot.same_database(&self.requested)
                || snapshot.taken_at != self.requested.taken_at
                || snapshot.runtime_populations != self.catalog
                || !workload_matches(snapshot, Some(population), self.catalog.as_ref())
            {
                return Err(invalid());
            }
            if let Some(workload) = snapshot.workload.as_ref().filter(|workload| workload.windowed) {
                let window = (&workload.window_start, &workload.window_end, workload.window_hours);
                if native_window.as_ref().is_some_and(|previous| previous != &window) {
                    return Err(invalid());
                }
                native_window = Some(window);
            }
        }
        if observed != expected {
            return Err(invalid());
        }
        Ok(())
    }
}

fn same_options(recorded: Option<&SnapshotOptions>, requested: &SnapshotOptions) -> bool {
    recorded == Some(requested)
}

fn workload_matches(
    snapshot: &Snapshot,
    selected: Option<&NativeIdentity>,
    catalog: Option<&RuntimePopulationCatalog>,
) -> bool {
    if selected
        .is_some_and(|selected| !selected.is_valid() || catalog.is_some_and(|catalog| !catalog.contains(selected)))
    {
        return false;
    }
    let Some(workload) = &snapshot.workload else { return true };
    if workload.population.as_ref().is_some_and(|population| !population.is_valid()) {
        return false;
    }
    if workload.population_catalog.as_ref().is_some_and(|recorded| Some(recorded) != catalog) {
        return false;
    }
    match selected {
        Some(selected) => {
            workload.population.as_ref() == Some(selected) && catalog.is_some_and(|catalog| catalog.contains(selected))
        }
        None if catalog.is_some_and(|catalog| catalog.requires_selection) => {
            // The implemented independent fallback owns cache totals, not a pooled history workload.
            workload.population.is_none()
                && workload.population_catalog.is_none()
                && workload.kind() == Some(crate::WorkloadKind::PlanCache)
        }
        None => workload
            .population
            .as_ref()
            .is_none_or(|population| catalog.is_some_and(|catalog| catalog.contains(population))),
    }
}

fn invalid() -> crate::Error {
    CollectionStop::InvalidPage.error()
}

#[cfg(test)]
mod tests;
