//! Execution-population membership comes only from the authenticated source's native bindings.
use super::*;
use culprit_core::telemetry::{
    ExportedRecord, NativeIdentity, QueryExecutionPopulation, QueryHistoryPopulationBinding, RuntimeMeasurement,
    TelemetryRecord, WaitMeasurement,
};
use std::collections::BTreeMap;

#[derive(Default)]
pub(super) struct NativeRows<'a> {
    pub runtime: Vec<(&'a ExportedRecord, &'a RuntimeMeasurement)>,
    pub waits: Vec<(&'a ExportedRecord, &'a WaitMeasurement)>,
}

pub(super) struct PopulationIndex<'a> {
    selected: Vec<CollectedQueryPopulation>,
    unpartitioned: NativeRows<'a>,
    partitioned: BTreeMap<&'a NativeIdentity, NativeRows<'a>>,
}

impl<'a> PopulationIndex<'a> {
    pub fn new(capture: &'a EvidenceCapture, requested: Option<&NativeIdentity>) -> Result<Self> {
        let selected = selected(capture, requested)?;
        let history = capture.source().query_history().ok_or_else(invalid)?;
        let partitioned = history
            .population_catalog
            .iter()
            .flat_map(|catalog| &catalog.populations)
            .filter(|population| {
                selected.iter().any(|selection| {
                    matches!(selection,
                CollectedQueryPopulation::Population {identity} if identity == &population.identity)
                })
            })
            .map(|population| (&population.identity, NativeRows::default()))
            .collect();
        let mut index = Self { selected, unpartitioned: NativeRows::default(), partitioned };
        for record in capture.records() {
            let record = record.row();
            let Some(key) = record.record.measurement_key() else { continue };
            // The complete source and every native receipt were validated before this one membership pass.
            // Unbound qualifiers remain untouched for the source's independent cohort relation.
            let rows = match history.population_binding.resolve(&key.native.qualifiers)? {
                QueryExecutionPopulation::Unknown => None,
                QueryExecutionPopulation::Unpartitioned => Some(&mut index.unpartitioned),
                QueryExecutionPopulation::Population(identity) => index.partitioned.get_mut(identity),
            };
            let Some(rows) = rows else { continue };
            match &record.record {
                TelemetryRecord::QueryRuntime(runtime) => rows.runtime.push((record, runtime)),
                TelemetryRecord::QueryWaits(wait) => rows.waits.push((record, wait)),
                _ => (),
            }
        }
        Ok(index)
    }

    pub fn selected(&self) -> &[CollectedQueryPopulation] {
        &self.selected
    }

    pub fn rows(&self, population: &CollectedQueryPopulation) -> Result<&NativeRows<'a>> {
        match population {
            CollectedQueryPopulation::Unknown | CollectedQueryPopulation::Unpartitioned => Ok(&self.unpartitioned),
            CollectedQueryPopulation::Population { identity } => self.partitioned.get(identity).ok_or_else(invalid),
        }
    }
}

fn selected(capture: &EvidenceCapture, requested: Option<&NativeIdentity>) -> Result<Vec<CollectedQueryPopulation>> {
    let history = capture.source().query_history().ok_or_else(invalid)?;
    if requested.is_some_and(|population| !population.is_valid()) {
        return Err(culprit_core::Error::invalid(culprit_core::t!("engine.changes.population_invalid")));
    }
    match &history.population_binding {
        QueryHistoryPopulationBinding::Unknown if requested.is_none() => Ok(vec![CollectedQueryPopulation::Unknown]),
        QueryHistoryPopulationBinding::Unpartitioned if requested.is_none() => {
            Ok(vec![CollectedQueryPopulation::Unpartitioned])
        }
        QueryHistoryPopulationBinding::Dimensions { .. } => {
            let catalog = history.population_catalog.as_ref().ok_or_else(invalid)?;
            let mut identities =
                catalog.populations.iter().map(|population| population.identity.clone()).collect::<Vec<_>>();
            if let Some(requested) = requested {
                if !identities.contains(requested) {
                    return Err(unavailable());
                }
                identities.retain(|identity| identity == requested);
            }
            if identities.is_empty() {
                return Ok(vec![CollectedQueryPopulation::Unknown]);
            }
            identities.sort();
            Ok(identities.into_iter().map(|identity| CollectedQueryPopulation::Population { identity }).collect())
        }
        _ => Err(unavailable()),
    }
}

fn unavailable() -> culprit_core::Error {
    culprit_core::Error::new(
        culprit_core::ErrorCode::Unsupported,
        culprit_core::t!("model.workload.population_unavailable"),
    )
}
