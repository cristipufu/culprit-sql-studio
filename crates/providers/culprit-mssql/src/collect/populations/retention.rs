//! Native response limits and retained report limits are independent: shared payloads can recur in many reports.
use super::*;
use culprit_core::serialization::{JsonSizeError, json_size};
use serde::Serialize;

const MAX_RETAINED_WORKLOAD_BYTES: usize = 64 * 1024 * 1024;
const MAX_POPULATION_SNAPSHOT_BYTES: usize = 128 * 1024 * 1024;

pub(super) struct WorkloadBudget {
    remaining: usize,
    exhausted: bool,
}
impl Default for WorkloadBudget {
    fn default() -> Self {
        Self { remaining: MAX_RETAINED_WORKLOAD_BYTES, exhausted: false }
    }
}
impl WorkloadBudget {
    pub(super) fn exhausted(&self) -> bool {
        self.exhausted
    }
    pub(super) fn retain(
        &mut self,
        workload: &Workload,
        statements: &std::collections::HashMap<String, crate::query_store::costs::CompilationStatement>,
    ) -> Result<()> {
        match serialized_size(&(workload, statements), self.remaining) {
            Ok(bytes) => {
                self.remaining -= bytes;
                Ok(())
            }
            Err(error) => {
                self.exhausted = true;
                Err(error)
            }
        }
    }
}

pub(super) fn project(
    shared: Snapshot,
    workloads: Workloads,
    options: &SnapshotOptions,
) -> Result<PopulationSnapshots> {
    project_with_limit(shared, workloads, options, MAX_POPULATION_SNAPSHOT_BYTES)
}

fn project_with_limit(
    mut shared: Snapshot,
    workloads: Workloads,
    options: &SnapshotOptions,
    limit: usize,
) -> Result<PopulationSnapshots> {
    shared.runtime_populations = workloads.catalog.clone();
    let common_bytes = serialized_size(&shared, limit)?;
    let mut remaining = limit;
    let mut populations = Vec::with_capacity(workloads.populations.len());
    // Reserve an honest unavailable result for every selector before retaining any full shared-payload copy.
    let unavailable = workloads
        .populations
        .iter()
        .map(|(selector, _)| {
            let mut selected = options.clone();
            selected.workload_population = Some(selector.clone());
            let snapshot = limited_snapshot(&shared, &selected);
            let bytes = serialized_size(&snapshot, remaining)?;
            remaining = remaining.checked_sub(bytes).ok_or_else(|| CollectionStop::ByteLimit.error())?;
            Ok((snapshot, bytes))
        })
        .collect::<Result<Vec<_>>>()?;
    let requested_workload_bytes = serialized_size(&workloads.requested.workload, remaining)?;
    remaining = remaining
        .checked_sub(common_bytes + requested_workload_bytes)
        .ok_or_else(|| CollectionStop::ByteLimit.error())?;

    for ((_, part), (mut limited, reserved)) in workloads.populations.into_iter().zip(unavailable) {
        let workload_bytes = serialized_size(&part.workload, limit)?;
        let full_bytes = common_bytes.saturating_add(workload_bytes);
        let additional = full_bytes.saturating_sub(reserved);
        if additional <= remaining {
            remaining -= additional;
            let mut snapshot = shared.clone();
            snapshot.options = limited.options.take();
            part.apply(&mut snapshot);
            populations.push(snapshot);
        } else {
            // The selector remains available for a later explicit request without borrowing healthy-empty facts.
            populations.push(limited);
        }
    }
    workloads.requested.apply(&mut shared);
    let batch = PopulationSnapshots { requested: shared, populations, catalog: workloads.catalog };
    batch.validate(options)?;
    // Includes coverage/notes introduced by projection; a failed accounting estimate never publishes an oversized batch.
    serialized_size(&batch, limit)?;
    Ok(batch)
}

fn limited_snapshot(shared: &Snapshot, options: &SnapshotOptions) -> Snapshot {
    let reason = CollectionStop::ByteLimit.error().message_source();
    let mut server = shared.server.clone();
    for capability in std::mem::take(&mut server.capabilities) {
        server.missing.retain(|missing| missing.capability != capability);
        server.missing.push(MissingCapability::new(capability, reason.clone()));
    }
    let mut snapshot = Snapshot {
        identity: shared.identity.clone(),
        sources: shared
            .sources
            .iter()
            .map(|source| culprit_core::SourceOutcome {
                source: source.source.clone(),
                state: culprit_core::SourceState::Unavailable,
                reason: Some(reason.render()),
                reason_message: Some(reason.clone()),
            })
            .collect(),
        format: shared.format,
        id: shared.id.clone(),
        connection: shared.connection.clone(),
        environment: shared.environment,
        taken_at: shared.taken_at.clone(),
        duration_ms: shared.duration_ms,
        server,
        runtime_populations: shared.runtime_populations.clone(),
        workload: None,
        tables: Vec::new(),
        indexes: Vec::new(),
        columns: Vec::new(),
        foreign_keys: Vec::new(),
        missing_indexes: Vec::new(),
        statistics: Vec::new(),
        waits: Vec::new(),
        resources: Vec::new(),
        settings: Vec::new(),
        pressure: None,
        notes: Vec::new(),
        options: Some(options.clone()),
        query_families: Vec::new(),
        plan_cache: None,
        tuning: None,
        deadlocks: None,
        constraints: Vec::new(),
        histograms: None,
        escalations_before: None,
        compilations_before: None,
        contention_before: None,
        evictions_before: None,
        sizes_before: None,
        operations: Default::default(),
    };
    let mut coverage = Coverage::default();
    coverage.source_failed(&sources::QUERY_STORE, &CollectionStop::ByteLimit.error());
    coverage.append_to(&mut snapshot);
    snapshot
}

fn serialized_size(value: &impl Serialize, limit: usize) -> Result<usize> {
    json_size(value, limit).map_err(|error| match error {
        JsonSizeError::LimitExceeded => CollectionStop::ByteLimit.error(),
        JsonSizeError::Serialization => crate::read::invalid_data(),
    })
}

#[cfg(test)]
mod tests;
