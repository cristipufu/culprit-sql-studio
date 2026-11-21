//! A selected provider owns both native construction and its static declarations. Listing never runs its factory.
use crate::ProviderFactory;
use culprit_core::{
    ComponentVersion, ConnectionSpec, Error, Provider, ProviderOperationObservation, Result,
    telemetry::{
        CollectionDescription, DiagnosticEvidenceRef, ExportedRecord, RecipeCostQualification, RecipeCostReceipt,
        RecipeDescriptor, qualification_component_is_valid, recipe_catalog_is_valid,
    },
};
use std::sync::Arc;
#[cfg(feature = "collector")]
mod profile;
#[cfg(feature = "collector")]
use culprit_core::telemetry::{
    CollectionRequest, RecipePurpose, RecipeSelector, SourceProfileAdmission, TelemetrySource,
};

/// Interpret authenticated native cost facts for this source implementation. The verifier must substantiate
/// native work and receipt accounting; a bounded request or successful client timing alone is insufficient.
pub type RecipeCostVerifier =
    Arc<dyn Fn(&RecipeCostReceipt, &CollectionDescription, &ExportedRecord) -> Result<bool> + Send + Sync>;

#[cfg(feature = "collector")]
pub type RecipeQualificationFactory = Arc<
    dyn Fn(
            &ConnectionSpec,
            &culprit_core::telemetry::CollectionRequest,
        ) -> Result<Box<dyn culprit_core::telemetry::TelemetrySource>>
        + Send
        + Sync,
>;

#[cfg(feature = "collector")]
pub type RecipeQualificationIssuer = Arc<
    dyn for<'a> Fn(
            culprit_collector::outbox::NativeQualificationEvidence<'a>,
        ) -> Result<culprit_collector::outbox::NativeQualificationDecision>
        + Send
        + Sync,
>;

/// Native profile intake reuses the constructed provider and its existing reader. Construction stays lazy.
/// The implementation must check the expected native owner/role before source work and at its final witness;
/// rejecting the result only after an unqualified query ran is insufficient. The same reader checks approval
/// liveness at every internal dispatch, preserves the original deadline/caller caps and retains cancellation work.
/// No default source fallback exists and this declaration issues no native cost approval.
#[cfg(feature = "collector")]
pub type ProfileSourceFactory = Arc<
    dyn Fn(&dyn Provider, &CollectionRequest, &SourceProfileAdmission) -> Result<Box<dyn TelemetrySource>>
        + Send
        + Sync,
>;

#[cfg(feature = "collector")]
#[derive(Clone)]
struct ProfileSourceIntake {
    selectors: Vec<RecipeSelector>,
    open: ProfileSourceFactory,
}

/// The normal observed path never invokes an unverified/eager constructor. Configuration work is client-only.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProviderConstructionPolicy {
    Unverified,
    DeferredNativeWork,
}

#[derive(Clone)]
struct QualificationAuthority {
    component: ComponentVersion,
    verifier: RecipeCostVerifier,
}

#[derive(Clone)]
pub struct ProviderRegistration {
    factory: ProviderFactory,
    construction: ProviderConstructionPolicy,
    recipes: Vec<RecipeDescriptor>,
    qualification: Option<QualificationAuthority>,
    #[cfg(feature = "collector")]
    profile_source: Option<ProfileSourceIntake>,
    #[cfg(feature = "collector")]
    issuer: Option<RecipeQualificationIssuer>,
    #[cfg(feature = "collector")]
    qualification_factory: Option<RecipeQualificationFactory>,
}
impl ProviderRegistration {
    /// An empty catalog is explicitly undeclared support. Declarations establish neither native availability
    /// nor production cost qualification; the provider retains all native admission and lifetime checks.
    pub fn new(
        factory: ProviderFactory,
        construction: ProviderConstructionPolicy,
        recipes: Vec<RecipeDescriptor>,
    ) -> Result<Self> {
        if !recipe_catalog_is_valid(&recipes) {
            return Err(Error::invalid(culprit_core::t!("engine.registry.invalid_recipes")));
        }
        Ok(Self {
            factory,
            construction,
            recipes,
            qualification: None,
            #[cfg(feature = "collector")]
            profile_source: None,
            #[cfg(feature = "collector")]
            issuer: None,
            #[cfg(feature = "collector")]
            qualification_factory: None,
        })
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

    /// Declare only the existing native routes that can enforce a source/profile expectation before reading.
    /// This grants no cost approval, performs no native work and changes no ordinary explicit source route.
    #[cfg(feature = "collector")]
    pub fn with_profile_source(mut self, selectors: Vec<RecipeSelector>, open: ProfileSourceFactory) -> Result<Self> {
        if self.construction != ProviderConstructionPolicy::DeferredNativeWork
            || selectors.is_empty()
            || selectors.len() > self.recipes.len()
            || selectors.iter().enumerate().any(|(index, selector)| {
                selectors[..index].contains(selector)
                    || !self
                        .recipes
                        .iter()
                        .any(|recipe| recipe.purpose == RecipePurpose::Baseline && recipe.selector == *selector)
            })
        {
            return Err(Error::invalid(culprit_core::t!("engine.registry.invalid_qualification")));
        }
        self.profile_source = Some(ProfileSourceIntake { selectors, open });
        Ok(self)
    }

    #[cfg(feature = "collector")]
    pub fn supports_profile_source(&self, selector: RecipeSelector) -> bool {
        self.construction == ProviderConstructionPolicy::DeferredNativeWork
            && self.profile_source.as_ref().is_some_and(|intake| intake.selectors.contains(&selector))
    }

    #[cfg(feature = "collector")]
    pub(crate) fn validate_profile(&self, request: &CollectionRequest, profile: &SourceProfileAdmission) -> Result<()> {
        let receipt = profile.qualification();
        let matches_selected = self.recipes.iter().any(|recipe| {
            recipe.purpose == RecipePurpose::Baseline
                && recipe.accepts(request)
                && request.budget.intersection(&recipe.budget) == request.budget
                && matches!(&recipe.cost, RecipeCostQualification::Measured(selected)
                    if selected.as_ref() == receipt)
        });
        if !profile.matches_request(request)
            || self.qualification_component() != Some(&receipt.component)
            || !self.supports_profile_source(receipt.selector)
            || !matches_selected
        {
            return Err(Error::invalid(culprit_core::t!("engine.registry.invalid_qualification")));
        }
        Ok(())
    }

    #[cfg(feature = "collector")]
    pub(crate) fn profile_source(
        &self,
        provider: &dyn Provider,
        request: &CollectionRequest,
        profile: &SourceProfileAdmission,
    ) -> Result<Box<dyn TelemetrySource>> {
        self.validate_profile(request, profile)?;
        let intake = self
            .profile_source
            .as_ref()
            .ok_or_else(|| Error::invalid(culprit_core::t!("engine.registry.invalid_qualification")))?;
        let source = (intake.open)(provider, request, profile)?;
        Ok(Box::new(profile::ProfileSource::new(source, profile.clone())))
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
        let Some(authority) = &self.qualification else {
            return Ok(false);
        };
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
        let RecipeCostQualification::Measured(selected) = &recipe.cost else {
            return Ok(false);
        };
        if !receipt.matches_recipe(recipe)
            || receipt.key()? != selected.key()?
            || !source.target.is_valid()
            || source.target.telemetry_id()? != receipt.scope.target().telemetry_id()?
        {
            return Ok(false);
        }
        self.verify_candidate(receipt, source, record)
    }

    /// Bootstrap only through selected trusted native issuance code. This does not install or activate a recipe.
    pub(crate) fn verify_candidate(
        &self,
        receipt: &RecipeCostReceipt,
        source: &CollectionDescription,
        record: &ExportedRecord,
    ) -> Result<bool> {
        let Some(authority) = &self.qualification else {
            return Ok(false);
        };
        let Some(recipe) = self
            .recipes
            .iter()
            .find(|recipe| recipe.id == receipt.recipe_id && recipe.version == receipt.recipe_version)
        else {
            return Ok(false);
        };
        if !receipt.is_valid()
            || receipt.component != authority.component
            || !receipt.matches_recipe(recipe)
            || source.target.telemetry_id()? != receipt.scope.target().telemetry_id()?
            || receipt.evidence.validate_canonical_record(source, &record.record).is_err()
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

    #[cfg(feature = "collector")]
    /// Installing a measurement issuer grants no acceptance authority. Its Approve result additionally requires
    /// this registration's independently declared pure native-cost verifier.
    pub fn with_native_qualification_issuer(
        mut self,
        factory: RecipeQualificationFactory,
        issuer: RecipeQualificationIssuer,
    ) -> Self {
        self.issuer = Some(issuer);
        self.qualification_factory = Some(factory);
        self
    }

    #[cfg(feature = "collector")]
    pub(crate) fn qualification_source(
        &self,
        spec: &ConnectionSpec,
        request: &culprit_core::telemetry::CollectionRequest,
    ) -> Result<Box<dyn culprit_core::telemetry::TelemetrySource>> {
        self.qualification_factory
            .as_ref()
            .ok_or_else(|| Error::invalid(culprit_core::t!("engine.registry.invalid_qualification")))?(
            spec, request
        )
    }

    #[cfg(feature = "collector")]
    pub(crate) fn native_issuer(&self) -> Option<RecipeQualificationIssuer> {
        self.issuer.clone()
    }

    #[cfg(feature = "collector")]
    pub(crate) fn with_installed(mut self, receipts: impl IntoIterator<Item = RecipeCostReceipt>) -> Result<Self> {
        for receipt in receipts {
            let Some(authority) = &self.qualification else {
                continue;
            };
            if receipt.component != authority.component {
                continue;
            }
            if let Some(recipe) = self
                .recipes
                .iter_mut()
                .find(|recipe| recipe.id == receipt.recipe_id && recipe.version == receipt.recipe_version)
                && receipt.matches_recipe(recipe)
            {
                recipe.cost = RecipeCostQualification::Measured(Box::new(receipt));
            }
        }
        if !recipe_catalog_is_valid(&self.recipes) {
            return Err(Error::invalid(culprit_core::t!("engine.registry.invalid_qualification")));
        }
        Ok(self)
    }

    pub(crate) fn open(&self, spec: &ConnectionSpec) -> Result<Box<dyn Provider>> {
        (self.factory)(spec, culprit_core::ProviderReadContext::unobserved())
    }
    pub(crate) fn open_observed(
        &self,
        spec: &ConnectionSpec,
        observation: &ProviderOperationObservation,
    ) -> Result<Box<dyn Provider>> {
        let span = observation.factory();
        let result = (|| {
            if self.construction != ProviderConstructionPolicy::DeferredNativeWork {
                return Err(Error::new(
                    culprit_core::ErrorCode::Unsupported,
                    culprit_core::t!("engine.registry.deferred_native_required"),
                ));
            }
            observation.bind_engine(crate::registry::engine(spec)?);
            (self.factory)(spec, observation.context())
        })();
        span.finish(&result);
        result
    }
}

#[cfg(test)]
mod tests;
