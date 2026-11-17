//! Immutable targeted questions and source-issued cost approvals. Structural validity is never native admission.
use super::*;
use crate::ComponentVersion;
mod execution;
mod scope;
pub use execution::{DiagnosticExecutionBinding, DiagnosticSharedOwner};
pub use scope::RecipeCostScope;

/// Approval issued by the selected source registration for one measured native context and exact tested budget.
/// Before native admission, the caller must authenticate `evidence` and run that registration's pure verifier
/// against the recovered native artifact. Client read metrics alone establish neither native cost nor safety.
#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct RecipeCostReceipt {
    pub recipe_id: String,
    pub recipe_version: u32,
    pub selector: RecipeSelector,
    pub scope: RecipeCostScope,
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
            && matches!(
                self.selector,
                RecipeSelector::FocusedPageWait
                    | RecipeSelector::FocusedBlocking
                    | RecipeSelector::FocusedPageWaitWriters
                    | RecipeSelector::FocusedPageTaskHolders
                    | RecipeSelector::FocusedRequestMemory
                    | RecipeSelector::FocusedCompilationAdmission
                    | RecipeSelector::CurrentRequestWork
                    | RecipeSelector::PageWaitTasks
            )
            && self.scope.is_valid()
            && self.scope.native_epoch().is_some()
            && self.scope.partition().is_qualified()
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

    /// Check the declared context of the focused investigation without granting authority or live admission.
    pub fn matches_recipe(&self, recipe: &RecipeDescriptor) -> bool {
        self.is_valid()
            && self.recipe_id == recipe.id
            && self.recipe_version == recipe.version
            && recipe.purpose == RecipePurpose::Investigation
            && recipe.selector == self.selector
            && recipe.subjects.iter().any(|subject| subject.accepts(&self.scope.subject()))
            && within(&self.tested_budget, &recipe.budget)
    }
}

/// One immutable focused question for an episode. Route/admission ownership stays with the scheduler.
/// The selected registration's authenticated cost verifier remains mandatory even when every relation here matches.
#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct DiagnosticInvestigationIntent {
    pub episode_id: String,
    pub episode_revision: String,
    pub recipe_id: String,
    pub recipe_version: u32,
    pub focus: DiagnosticFocus,
    pub execution: DiagnosticExecutionBinding,
    pub budget: CollectionBudget,
    pub qualification: RecipeCostReceipt,
}

/// Owner-written provenance retained before source authentication or description. The complete admitted question
/// stays available even when the request/native header carries only its original capture association.
/// It records structural provenance, never permission to dispatch or a replacement for the source's verifier.
#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct DiagnosticInvestigationLineage {
    pub intent_key: String,
    /// The admitted question stays immutable when a native header contains only a capture-level association.
    /// Public analytical metadata projects its opaque key, never this private original request or SQL.
    pub focus: DiagnosticFocus,
    pub episode_id: String,
    pub episode_revision: String,
    pub recipe_id: String,
    pub recipe_version: u32,
    pub budget: CollectionBudget,
    pub qualification: RecipeCostReceipt,
    pub execution: DiagnosticExecutionBinding,
}

impl DiagnosticInvestigationLineage {
    pub fn from_intent(intent: &DiagnosticInvestigationIntent) -> Result<Self> {
        Ok(Self {
            intent_key: intent.key()?,
            focus: intent.focus.clone(),
            episode_id: intent.episode_id.clone(),
            episode_revision: intent.episode_revision.clone(),
            recipe_id: intent.recipe_id.clone(),
            recipe_version: intent.recipe_version,
            budget: intent.budget.clone(),
            qualification: intent.qualification.clone(),
            execution: intent.execution.clone(),
        })
    }

    /// Reconstruct the exact retired intent using only the request's original question and this provenance.
    /// Parent/cost artifact authentication and selected-source authority remain separate caller-owned checks.
    pub fn reconstruct(&self, focus: &DiagnosticFocus) -> Result<DiagnosticInvestigationIntent> {
        if !valid_digest(&self.intent_key) || *focus != self.focus {
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
            execution: self.execution.clone(),
        };
        if intent.key()? != self.intent_key {
            return Err(invalid());
        }
        Ok(intent)
    }

    /// Match immutable provenance, including the exact native accounting encoding, to the original question.
    pub fn matches_intent(&self, focus: &DiagnosticFocus, intent: &DiagnosticInvestigationIntent) -> bool {
        intent.key().is_ok_and(|expected| self.intent_key == expected && self.reconstruct(focus).is_ok())
    }

    /// The source authenticates its actual result scope separately. A capture-level native header cannot
    /// reconstruct or replace the exact original incident participant retained by this owner.
    pub fn intent_for_source(&self, source: &CollectionDescription) -> Result<DiagnosticInvestigationIntent> {
        let intent = self.reconstruct(&self.focus)?;
        if !intent.matches_result_source(source) {
            return Err(invalid());
        }
        Ok(intent)
    }

    /// Match admitted owner metadata before any source exists. This grants no source or native authority.
    pub fn matches_request(&self, request: &CollectionRequest) -> bool {
        self.intent_for_request(request).is_ok()
    }

    pub(super) fn intent_for_request(&self, request: &CollectionRequest) -> Result<DiagnosticInvestigationIntent> {
        request.validate()?;
        let intent = self.reconstruct(&self.focus)?;
        if intent.budget != request.budget
            || intent.subject() != request.subject
            || intent.selection() != request.selection
        {
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
        focus: DiagnosticFocus,
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
            execution: DiagnosticExecutionBinding::Database,
            budget: receipt.tested_budget.clone(),
            qualification: receipt.as_ref().clone(),
        };
        if !value.matches_admission(episode, recipe, component, caller_budget) {
            return Err(invalid());
        }
        Ok(value)
    }

    /// The caller authenticates the positive shared owner through the same outbox/recovery proof path as the parent.
    pub fn new_shared(
        episode: &DiagnosticEpisode,
        recipe: &RecipeDescriptor,
        focus: DiagnosticFocus,
        component: &ComponentVersion,
        caller_budget: &CollectionBudget,
        owner: DiagnosticSharedOwner<'_>,
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
            execution: DiagnosticExecutionBinding::Shared { owner: Box::new(owner.reference.clone()) },
            budget: receipt.tested_budget.clone(),
            qualification: receipt.as_ref().clone(),
        };
        if !value.matches_admission(episode, recipe, component, caller_budget)
            || !value.matches_shared_owner(owner.source, owner.row)
        {
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
            && self.focus.selector() == self.qualification.selector
            && self.recipe_id == self.qualification.recipe_id
            && self.recipe_version == self.qualification.recipe_version
            && self.budget == self.qualification.tested_budget
            && self.execution.matches_focus(&self.qualification.scope, &self.focus)
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
        if let DiagnosticFocus::PageWaitTasks { focus, .. } = &self.focus {
            // One native population belongs to this exact original capture, even if several incident rows
            // can justify it. Another trigger cannot spend the same population stage again after restart.
            return page_wait_tasks_stage_key(&focus.parent.capture_ref(), &self.qualification.scope.target());
        }
        hash(&(
            "diagnostic-question-stage-v3",
            &self.episode_id,
            self.focus.selector(),
            self.qualification.scope.target().telemetry_id()?,
        ))
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
            && self.matches_episode(episode)
            && episode.analysis.needs_work()
            && recipe.is_valid()
            && self.qualification.matches_recipe(recipe)
            && self.qualification.key().ok() == receipt.key().ok()
            && qualification_component_is_valid(component)
            && *component == self.qualification.component
            && within(&self.budget, caller_budget)
    }
}

/// The exact earlier frame and execution resource identify one population stage before its rows are recovered.
/// This key grants no evidence authentication, source authority or native admission.
pub fn page_wait_tasks_stage_key(
    original: &DiagnosticCaptureRef,
    execution_target: &CollectionTarget,
) -> Result<String> {
    if !original.is_valid() {
        return Err(invalid());
    }
    hash(&(
        "diagnostic-activity-population-stage-v1",
        original,
        RecipeSelector::PageWaitTasks,
        execution_target.telemetry_id()?,
    ))
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
        || metrics.native_session_work.as_ref().is_some_and(|work| work.settlement() != NativeReadSettlement::Quiescent)
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
