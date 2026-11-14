//! A selected provider owns both native construction and its static declarations. Listing never runs its factory.
use crate::ProviderFactory;
use culprit_core::{
    ConnectionSpec, Error, Provider, Result,
    telemetry::{RecipeDescriptor, recipe_catalog_is_valid},
};

#[derive(Clone)]
pub struct ProviderRegistration {
    factory: ProviderFactory,
    recipes: Vec<RecipeDescriptor>,
}
impl ProviderRegistration {
    /// An empty catalog is explicitly undeclared support. Declarations establish neither native availability
    /// nor production cost qualification; the provider retains all native admission and lifetime checks.
    pub fn new(factory: ProviderFactory, recipes: Vec<RecipeDescriptor>) -> Result<Self> {
        if !recipe_catalog_is_valid(&recipes) {
            return Err(Error::invalid(culprit_core::t!("engine.registry.invalid_recipes")));
        }
        Ok(Self { factory, recipes })
    }

    pub fn telemetry_recipes(&self) -> &[RecipeDescriptor] {
        &self.recipes
    }

    pub(crate) fn open(&self, spec: &ConnectionSpec) -> Result<Box<dyn Provider>> {
        (self.factory)(spec)
    }
}

#[cfg(test)]
mod tests;
