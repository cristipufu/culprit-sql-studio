//! Native adapters declare population membership; common readers never interpret dimension names or role labels.
use super::*;
use crate::NativeRuntimePopulationCatalog;
use std::collections::{BTreeMap, BTreeSet};

const MAX_POPULATION_BINDING_BYTES: usize = 256 * 1024;

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
#[ts(export)]
pub enum QueryHistoryPopulationBinding {
    /// The source cannot establish execution-population membership, including whether rows are unpartitioned.
    Unknown,
    /// The source positively established that its statistics have no execution-population dimension.
    Unpartitioned,
    /// These disjoint native qualifier tuples map every retained measurement to one exact catalog identity.
    Dimensions { bindings: Vec<QueryHistoryPopulationBindingEntry> },
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct QueryHistoryPopulationBindingEntry {
    pub population: NativeIdentity,
    pub qualifiers: BTreeMap<String, String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QueryExecutionPopulation<'a> {
    Unknown,
    Unpartitioned,
    Population(&'a NativeIdentity),
}

impl QueryHistoryPopulationBinding {
    pub(super) fn is_valid(
        &self,
        catalog: Option<&NativeRuntimePopulationCatalog>,
        dimensions: &BTreeMap<String, Vec<String>>,
    ) -> bool {
        if catalog.is_some_and(|catalog| !catalog.is_valid()) {
            return false;
        }
        match self {
            Self::Unknown => catalog.is_none_or(|catalog| catalog.partitioned),
            Self::Unpartitioned => catalog.is_some_and(|catalog| !catalog.partitioned),
            Self::Dimensions { bindings } => {
                let Some(catalog) = catalog.filter(|catalog| catalog.partitioned) else { return false };
                valid_bindings(bindings, catalog, dimensions)
            }
        }
    }

    /// Resolve an exact native tuple after validating its source description. An unknown tuple is invalid, not an
    /// unpartitioned measurement. Cohort, outcome and plan qualifiers remain independent of this selection.
    pub fn resolve(&self, qualifiers: &BTreeMap<String, String>) -> Result<QueryExecutionPopulation<'_>> {
        match self {
            Self::Unknown => Ok(QueryExecutionPopulation::Unknown),
            Self::Unpartitioned => Ok(QueryExecutionPopulation::Unpartitioned),
            Self::Dimensions { bindings } => {
                let mut matching = bindings.iter().filter(|binding| {
                    !binding.qualifiers.is_empty()
                        && binding.qualifiers.iter().all(|(key, value)| qualifiers.get(key) == Some(value))
                });
                let first = matching.next().ok_or_else(|| CollectionStop::InvalidPage.error())?;
                if matching.next().is_some() {
                    return Err(CollectionStop::InvalidPage.error());
                }
                Ok(QueryExecutionPopulation::Population(&first.population))
            }
        }
    }
}

impl QueryHistoryDescription {
    /// A standalone caller receives no selection authority from an invalid or incomplete declaration.
    pub fn execution_population(&self, qualifiers: &BTreeMap<String, String>) -> Result<QueryExecutionPopulation<'_>> {
        if !self.is_valid() {
            return Err(CollectionStop::InvalidPage.error());
        }
        self.population_binding.resolve(qualifiers)
    }
}

fn valid_bindings(
    bindings: &[QueryHistoryPopulationBindingEntry],
    catalog: &NativeRuntimePopulationCatalog,
    dimensions: &BTreeMap<String, Vec<String>>,
) -> bool {
    if bindings.len() != catalog.populations.len()
        || bindings.len() > crate::runtime_population::MAX_RUNTIME_POPULATIONS
    {
        return false;
    }
    let Some(first) = bindings.first() else { return true };
    if first.qualifiers.is_empty() || first.qualifiers.len() > MAX_NATIVE_QUALIFIERS {
        return false;
    }
    let expected = catalog.populations.iter().map(|population| &population.identity).collect::<BTreeSet<_>>();
    let mut identities = BTreeSet::new();
    let mut tuples = BTreeSet::new();
    let mut bytes = 0usize;
    for binding in bindings {
        if !binding.population.is_valid()
            || !expected.contains(&binding.population)
            || !identities.insert(&binding.population)
            || !binding.qualifiers.keys().eq(first.qualifiers.keys())
            || !tuples.insert(&binding.qualifiers)
            || !binding
                .qualifiers
                .iter()
                .all(|(key, value)| dimensions.get(key).is_some_and(|allowed| allowed.contains(value)))
        {
            return false;
        }
        bytes += binding.population.id.len()
            + binding
                .population
                .qualifiers
                .iter()
                .chain(&binding.qualifiers)
                .map(|(key, value)| key.len() + value.len())
                .sum::<usize>();
        if bytes > MAX_POPULATION_BINDING_BYTES {
            return false;
        }
    }
    identities == expected
}

#[cfg(test)]
mod tests;
