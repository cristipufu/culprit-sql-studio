//! Source-declared execution populations are distinct from physical compute members and routing preferences.
use crate::{i18n::Message, telemetry::NativeIdentity};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use ts_rs::TS;
mod snapshots;
pub use snapshots::PopulationSnapshots;

pub const MAX_RUNTIME_POPULATIONS: usize = 128;
const MAX_POPULATION_METADATA_BYTES: usize = 256 * 1024;

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq)]
#[ts(export)]
pub struct RuntimePopulation {
    /// The selected source owns the native identifier and qualifiers. A role label cannot choose this identity.
    pub identity: NativeIdentity,
    pub label: Message,
    /// Native catalog history for inspection. Common consumers never infer a current member from these values.
    pub native_properties: BTreeMap<String, String>,
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq)]
#[ts(export)]
pub struct RuntimePopulationCatalog {
    pub source: String,
    /// False only when the source positively established that its native statistics have no population dimension.
    pub partitioned: bool,
    /// Observed alternatives need a selection before this source can produce one comparable scan workload.
    pub requires_selection: bool,
    pub populations: Vec<RuntimePopulation>,
}

/// Source metadata retained with history. Display messages are deliberately separate from these native facts.
#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[ts(export)]
pub struct NativeRuntimePopulation {
    pub identity: NativeIdentity,
    pub native_properties: BTreeMap<String, String>,
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[ts(export)]
pub struct NativeRuntimePopulationCatalog {
    pub source: String,
    pub partitioned: bool,
    pub requires_selection: bool,
    pub populations: Vec<NativeRuntimePopulation>,
}

impl RuntimePopulationCatalog {
    pub fn is_valid(&self) -> bool {
        valid_catalog(
            &self.source,
            self.partitioned,
            self.requires_selection,
            self.populations.iter().map(|population| (&population.identity, &population.native_properties)),
        )
    }

    pub fn contains(&self, identity: &NativeIdentity) -> bool {
        self.is_valid()
            && identity.is_valid()
            && self.populations.iter().any(|population| &population.identity == identity)
    }

    pub fn native_facts(&self) -> NativeRuntimePopulationCatalog {
        NativeRuntimePopulationCatalog {
            source: self.source.clone(),
            partitioned: self.partitioned,
            requires_selection: self.requires_selection,
            populations: self
                .populations
                .iter()
                .map(|population| NativeRuntimePopulation {
                    identity: population.identity.clone(),
                    native_properties: population.native_properties.clone(),
                })
                .collect(),
        }
    }
}

impl NativeRuntimePopulationCatalog {
    pub fn is_valid(&self) -> bool {
        valid_catalog(
            &self.source,
            self.partitioned,
            self.requires_selection,
            self.populations.iter().map(|population| (&population.identity, &population.native_properties)),
        )
    }
}

fn valid_catalog<'a>(
    source: &str,
    partitioned: bool,
    requires_selection: bool,
    populations: impl ExactSizeIterator<Item = (&'a NativeIdentity, &'a BTreeMap<String, String>)>,
) -> bool {
    let count = populations.len();
    if source.trim().is_empty()
        || source.len() > 512
        || source.chars().any(char::is_control)
        || count > MAX_RUNTIME_POPULATIONS
        || !partitioned && (count != 0 || requires_selection)
        || requires_selection && count == 0
    {
        return false;
    }
    let mut identities = BTreeSet::new();
    let mut metadata_bytes = 0;
    for (identity, properties) in populations {
        if !identity.is_valid() || !identities.insert(identity) || properties.len() > 32 {
            return false;
        }
        for (key, value) in properties {
            metadata_bytes += key.len() + value.len();
            if key.is_empty()
                || key.len() > 128
                || key.chars().any(char::is_control)
                || value.len() > 16 * 1024
                || metadata_bytes > MAX_POPULATION_METADATA_BYTES
            {
                return false;
            }
        }
    }
    true
}

#[cfg(test)]
mod tests;
