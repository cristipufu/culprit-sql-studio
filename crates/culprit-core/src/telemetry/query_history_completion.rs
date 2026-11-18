//! Final history metadata can confirm its original owner and add only bounded observed membership.
use super::*;
use crate::RuntimePopulationMembership;
use std::collections::{BTreeMap, BTreeSet};

impl QueryHistoryDescription {
    pub(super) fn accepts_completion(&self, completed: &Self) -> bool {
        self.is_valid()
            && completed.is_valid()
            && self.interval == completed.interval
            && self.waits == completed.waits
            && self.native_properties == completed.native_properties
            && match (&self.acquisition_owner, &completed.acquisition_owner) {
                (None, None) => true,
                (Some(before), Some(after)) => after.completes_read(before),
                _ => false,
            }
            && self.accepts_population_completion(completed)
    }

    fn accepts_population_completion(&self, completed: &Self) -> bool {
        if self.population_catalog == completed.population_catalog
            && self.population_binding == completed.population_binding
            && self.dimensions == completed.dimensions
        {
            return true;
        }
        if self.acquisition_owner.is_none()
            || self.population_catalog.is_some()
            || self.population_binding != QueryHistoryPopulationBinding::Unknown
        {
            return false;
        }
        let Some(catalog) = &completed.population_catalog else { return false };
        if !matches!(catalog.membership, RuntimePopulationMembership::ObservedWindow { .. })
            || !catalog.membership.matches_window(Some(&self.interval.from), Some(&self.interval.until))
        {
            return false;
        }
        let QueryHistoryPopulationBinding::Dimensions { bindings } = &completed.population_binding else {
            return false;
        };
        let mut observed = BTreeMap::<String, BTreeSet<String>>::new();
        for binding in bindings {
            for (name, value) in &binding.qualifiers {
                observed.entry(name.clone()).or_default().insert(value.clone());
            }
        }
        // Existing dimension facts are immutable. Added values belong only to the explicit completed bindings.
        self.dimensions.iter().all(|(name, values)| completed.dimensions.get(name) == Some(values))
            && completed.dimensions.iter().all(|(name, values)| {
                self.dimensions.contains_key(name)
                    || observed
                        .get(name)
                        .is_some_and(|observed| values.iter().cloned().collect::<BTreeSet<_>>() == *observed)
            })
    }
}

#[cfg(test)]
mod tests;
