//! Immutable targeted questions and source-issued cost approvals. Structural validity is never native admission.
use super::*;
use crate::ComponentVersion;

/// Approval issued by the selected source registration for one measured native context and exact tested budget.
/// Before native admission, the caller must authenticate `evidence` and run that registration's pure verifier
/// against the recovered native artifact. Client read metrics alone establish neither native cost nor safety.
#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct RecipeCostReceipt {
    pub recipe_id: String,
    pub recipe_version: u32,
    pub scope: DiagnosticScope,
    /// Exact native-reader/qualification authority identity supplied by the selected registration.
    pub component: ComponentVersion,
    pub tested_budget: CollectionBudget,
    /// An independently authenticated native-cost artifact, interpreted only by that source's pure verifier.
    pub evidence: DiagnosticEvidenceRef,
    /// Successful, settled client accounting for that experiment; it is not a native CPU/I/O measurement.
    pub read_metrics: ReadWorkMetrics,
}

impl RecipeCostReceipt {
    /// Validate bounded identity and accounting only. This does not authenticate or approve the native artifact.
    pub fn is_valid(&self) -> bool {
        valid_key(&self.recipe_id)
            && self.recipe_version > 0
            && self.scope.is_valid()
            && self.scope.native_epoch.is_some()
            && qualification_component_is_valid(&self.component)
            && self.tested_budget.validate().is_ok()
            && self.evidence.is_valid()
            && settled_accounting(&self.read_metrics, &self.tested_budget)
    }

    pub fn key(&self) -> Result<String> {
        if !self.is_valid() {
            return Err(invalid());
        }
        hash(&("recipe-cost-receipt-v1", self))
    }

    /// Check the declared context of the concrete page investigation without granting authority or live admission.
    pub fn matches_recipe(&self, recipe: &RecipeDescriptor) -> bool {
        self.is_valid()
            && self.recipe_id == recipe.id
            && self.recipe_version == recipe.version
            && recipe.purpose == RecipePurpose::Investigation
            && recipe.selector == RecipeSelector::FocusedPageWait
            && recipe.subjects.contains(&RecipeSubject::Database)
            && within(&self.tested_budget, &recipe.budget)
    }
}

/// One immutable attempt to investigate one page-wait episode. Route/admission ownership stays with the scheduler.
/// The selected registration's authenticated cost verifier remains mandatory even when every relation here matches.
#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct DiagnosticInvestigationIntent {
    pub episode_id: String,
    pub episode_revision: String,
    pub recipe_id: String,
    pub recipe_version: u32,
    pub focus: ActivityWaitFocus,
    pub budget: CollectionBudget,
    pub qualification: RecipeCostReceipt,
}

/// Owner-written provenance retained before source authentication or description. The request already owns its
/// exact original question; this stores the remaining fields once so failure cannot erase admitted recipe history.
/// It records structural provenance, never permission to dispatch or a replacement for the source's verifier.
#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct DiagnosticInvestigationLineage {
    pub intent_key: String,
    pub episode_id: String,
    pub episode_revision: String,
    pub recipe_id: String,
    pub recipe_version: u32,
    pub budget: CollectionBudget,
    pub qualification: RecipeCostReceipt,
}

impl DiagnosticInvestigationLineage {
    pub fn from_intent(intent: &DiagnosticInvestigationIntent) -> Result<Self> {
        Ok(Self {
            intent_key: intent.key()?,
            episode_id: intent.episode_id.clone(),
            episode_revision: intent.episode_revision.clone(),
            recipe_id: intent.recipe_id.clone(),
            recipe_version: intent.recipe_version,
            budget: intent.budget.clone(),
            qualification: intent.qualification.clone(),
        })
    }

    /// Reconstruct the exact retired intent using only the request's original question and this provenance.
    /// Parent/cost artifact authentication and selected-source authority remain separate caller-owned checks.
    pub fn reconstruct(&self, focus: &ActivityWaitFocus) -> Result<DiagnosticInvestigationIntent> {
        if !valid_digest(&self.intent_key) {
            return Err(invalid());
        }
        let intent = DiagnosticInvestigationIntent {
            episode_id: self.episode_id.clone(),
            episode_revision: self.episode_revision.clone(),
            recipe_id: self.recipe_id.clone(),
            recipe_version: self.recipe_version,
            focus: focus.clone(),
            budget: self.budget.clone(),
            qualification: self.qualification.clone(),
        };
        if intent.key()? != self.intent_key {
            return Err(invalid());
        }
        Ok(intent)
    }

    /// Match immutable provenance, including the exact native accounting encoding, to the original question.
    pub fn matches_intent(&self, focus: &ActivityWaitFocus, intent: &DiagnosticInvestigationIntent) -> bool {
        intent.key().is_ok_and(|expected| self.intent_key == expected && self.reconstruct(focus).is_ok())
    }

    /// Match admitted owner metadata before any source exists. This grants no source or native authority.
    pub fn matches_request(&self, request: &CollectionRequest) -> bool {
        self.intent_for_request(request).is_ok()
    }

    pub(super) fn intent_for_request(&self, request: &CollectionRequest) -> Result<DiagnosticInvestigationIntent> {
        request.validate()?;
        let CollectionSelection::FocusedPageWait { focus } = &request.selection else {
            return Err(invalid());
        };
        let intent = self.reconstruct(focus)?;
        if intent.budget != request.budget || !request.subject.accepts(&intent.focus.target) {
            return Err(invalid());
        }
        Ok(intent)
    }
}

impl DiagnosticInvestigationIntent {
    /// Assemble a structurally related question. The caller still owns parent/cost authentication, the selected
    /// source's pure verifier, durable scheduling and every live native admission/cancellation guard.
    pub fn new(
        episode: &DiagnosticEpisode,
        recipe: &RecipeDescriptor,
        focus: ActivityWaitFocus,
        component: &ComponentVersion,
        caller_budget: &CollectionBudget,
    ) -> Result<Self> {
        let RecipeCostQualification::Measured(receipt) = &recipe.cost else {
            return Err(invalid());
        };
        let value = Self {
            episode_id: episode.id.clone(),
            episode_revision: episode.revision.clone(),
            recipe_id: recipe.id.clone(),
            recipe_version: recipe.version,
            focus,
            budget: receipt.tested_budget.clone(),
            qualification: receipt.as_ref().clone(),
        };
        if !value.matches_admission(episode, recipe, component, caller_budget) {
            return Err(invalid());
        }
        Ok(value)
    }

    /// Structural validity is suitable for durable storage; it cannot authorize a provider or authenticate evidence.
    pub fn is_valid(&self) -> bool {
        valid_digest(&self.episode_id)
            && super::diagnostics::positive(&self.episode_revision).is_some()
            && valid_key(&self.recipe_id)
            && self.recipe_version > 0
            && self.focus.is_valid()
            && self.qualification.is_valid()
            && self.recipe_id == self.qualification.recipe_id
            && self.recipe_version == self.qualification.recipe_version
            && self.budget == self.qualification.tested_budget
            && self.qualification.scope.source == self.focus.source
            && self.qualification.scope.native_epoch.as_deref() == Some(self.focus.epoch.as_str())
            && self.qualification.scope.target.telemetry_id().ok() == self.focus.target.telemetry_id().ok()
    }

    pub fn key(&self) -> Result<String> {
        if !self.is_valid() {
            return Err(invalid());
        }
        hash(&("diagnostic-investigation-intent-v1", self))
    }

    /// New baselines, recipe versions and revisions cannot create competing outstanding work for this same stage.
    pub fn episode_stage_key(&self) -> Result<String> {
        if !self.is_valid() {
            return Err(invalid());
        }
        hash(&("diagnostic-page-wait-stage-v1", &self.episode_id))
    }

    /// Revalidate the immutable question against current, authenticated context. A newer same-episode revision
    /// may still owe this analysis, including after recovery; rollback, replacement and terminal analysis fail.
    /// A true result still requires the selected registration's native-cost verifier and live admission guards.
    pub fn matches_admission(
        &self,
        episode: &DiagnosticEpisode,
        recipe: &RecipeDescriptor,
        component: &ComponentVersion,
        caller_budget: &CollectionBudget,
    ) -> bool {
        let RecipeCostQualification::Measured(receipt) = &recipe.cost else { return false };
        self.is_valid()
            && episode.is_valid()
            && episode.analysis.needs_work()
            && episode.id == self.episode_id
            && super::diagnostics::positive(&episode.revision) >= super::diagnostics::positive(&self.episode_revision)
            && episode.matches_focus(&self.focus)
            && episode.scope.key().ok() == self.qualification.scope.key().ok()
            && recipe.is_valid()
            && self.qualification.matches_recipe(recipe)
            && self.qualification.key().ok() == receipt.key().ok()
            && qualification_component_is_valid(component)
            && *component == self.qualification.component
            && within(&self.budget, caller_budget)
    }
}

/// A package version alone cannot identify native reader changes across private builds.
pub fn qualification_component_is_valid(component: &ComponentVersion) -> bool {
    valid_key(&component.component)
        && component.version.as_deref().is_some_and(valid_key)
        && component.revision.as_deref().is_some_and(valid_key)
}

fn settled_accounting(metrics: &ReadWorkMetrics, budget: &CollectionBudget) -> bool {
    if !metrics.is_valid()
        || metrics.active_tasks != 0
        || metrics.untracked_tasks != 0
        || metrics.templates.is_empty()
        || metrics.templates.iter().any(|metric| metric.completed_tasks == 0 || metric.failures != 0)
    {
        return false;
    }
    let totals = metrics.templates.iter().try_fold((0u64, 0u64), |(queries, bytes), metric| {
        Some((
            queries.checked_add(u64::from(metric.completed_tasks))?,
            bytes.checked_add(metric.charged_value_bytes.parse::<u64>().ok()?)?,
        ))
    });
    totals.is_some_and(|(queries, bytes)| {
        queries <= u64::from(budget.max_queries) && bytes <= u64::from(budget.total_bytes)
    })
}

fn within(budget: &CollectionBudget, ceiling: &CollectionBudget) -> bool {
    budget.validate().is_ok() && ceiling.validate().is_ok() && budget.intersection(ceiling) == *budget
}

fn hash(value: &impl Serialize) -> Result<String> {
    serde_json::to_vec(value).map(|bytes| digest(&bytes)).map_err(|_| invalid())
}

fn invalid() -> Error {
    CollectionStop::InvalidPage.error()
}
