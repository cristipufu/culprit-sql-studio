//! A selected provider owns both native construction and its static declarations. Listing never runs its factory.
use crate::ProviderFactory;
use culprit_core::{
    ComponentVersion, ConnectionSpec, Error, Provider, Result,
    telemetry::{
        CollectionDescription, DiagnosticEvidenceRef, ExportedRecord, RecipeCostQualification, RecipeCostReceipt,
        RecipeDescriptor, qualification_component_is_valid, recipe_catalog_is_valid,
    },
};
use std::sync::Arc;

/// Interpret authenticated native cost facts for this source implementation. The verifier must substantiate
/// native work and receipt accounting; a bounded request or successful client timing alone is insufficient.
pub type RecipeCostVerifier =
    Arc<dyn Fn(&RecipeCostReceipt, &CollectionDescription, &ExportedRecord) -> Result<bool> + Send + Sync>;

#[derive(Clone)]
struct QualificationAuthority {
    component: ComponentVersion,
    verifier: RecipeCostVerifier,
}

#[derive(Clone)]
pub struct ProviderRegistration {
    factory: ProviderFactory,
    recipes: Vec<RecipeDescriptor>,
    qualification: Option<QualificationAuthority>,
}
impl ProviderRegistration {
    /// An empty catalog is explicitly undeclared support. Declarations establish neither native availability
    /// nor production cost qualification; the provider retains all native admission and lifetime checks.
    pub fn new(factory: ProviderFactory, recipes: Vec<RecipeDescriptor>) -> Result<Self> {
        if !recipe_catalog_is_valid(&recipes) {
            return Err(Error::invalid(culprit_core::t!("engine.registry.invalid_recipes")));
        }
        Ok(Self { factory, recipes, qualification: None })
    }

    pub fn telemetry_recipes(&self) -> &[RecipeDescriptor] {
        &self.recipes
    }

    /// Attach a pure authority to this exact selected registration. Construction performs no source work.
    pub fn with_qualification_authority(
        mut self,
        component: ComponentVersion,
        verifier: RecipeCostVerifier,
    ) -> Result<Self> {
        if !qualification_component_is_valid(&component) {
            return Err(Error::invalid(culprit_core::t!("engine.registry.invalid_qualification")));
        }
        self.qualification = Some(QualificationAuthority { component, verifier });
        Ok(self)
    }

    pub fn qualification_component(&self) -> Option<&ComponentVersion> {
        self.qualification.as_ref().map(|authority| &authority.component)
    }

    /// The caller first authenticates the artifact's manifest membership and ordinal through outbox/recovery.
    /// This gate checks exact selected receipt/source/payload consistency, then invokes only this registration's
    /// pure native-cost verifier. It never runs the factory, discovers credentials or authorizes live native work.
    pub fn verify_cost(
        &self,
        receipt: &RecipeCostReceipt,
        source: &CollectionDescription,
        record: &ExportedRecord,
    ) -> Result<bool> {
        let Some(authority) = &self.qualification else { return Ok(false) };
        if !receipt.is_valid() || receipt.component != authority.component {
            return Ok(false);
        }
        let Some(recipe) = self
            .recipes
            .iter()
            .find(|recipe| recipe.id == receipt.recipe_id && recipe.version == receipt.recipe_version)
        else {
            return Ok(false);
        };
        let RecipeCostQualification::Measured(selected) = &recipe.cost else { return Ok(false) };
        if !receipt.matches_recipe(recipe)
            || receipt.key()? != selected.key()?
            || !source.target.is_valid()
            || source.target.telemetry_id()? != receipt.scope.target.telemetry_id()?
        {
            return Ok(false);
        }
        let reference = match DiagnosticEvidenceRef::from_export(
            source,
            &receipt.evidence.manifest_sha256,
            record,
            receipt.evidence.row_ordinal,
        ) {
            Ok(reference) => reference,
            Err(_) => return Ok(false),
        };
        if reference != receipt.evidence {
            return Ok(false);
        }
        (authority.verifier)(receipt, source, record)
    }

    pub(crate) fn open(&self, spec: &ConnectionSpec) -> Result<Box<dyn Provider>> {
        (self.factory)(spec)
    }
}

#[cfg(test)]
mod tests;
