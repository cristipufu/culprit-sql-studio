//! Selected trusted native execution, exact local issuance and per-connection restoration share existing owners.
use super::*;
use crate::{ProviderRegistration, RecipeQualificationIssuer};
use culprit_collector::outbox::{
    CompletedCaptureBudget, NativeQualificationDecision, NativeQualificationEvidence, NativeQualificationRequest,
    NativeQualificationSource, OutboxDelivery,
};
use culprit_core::{RecipeQualificationSummary, telemetry::*};
use std::time::Instant;

impl Workspace {
    /// An explicit experiment invokes only the selected built-in/plugin native source and its trusted policy.
    /// Selection files are requests, never authority. Parent receipts are authenticated by the existing lease.
    pub async fn qualify_recipe(
        &self,
        name: &str,
        recipe_id: &str,
        mut request: CollectionRequest,
        outbox: &mut Outbox,
        cancellation: &CollectionCancellation,
    ) -> Result<RecipeQualificationSummary> {
        request.validate()?;
        let connections = self.connection_service();
        let route_revision = connections.native_route_revision(name)?;
        let route = connections.saved_route(name)?;
        let registration = connections.selected_registration(self.store.get(name)?.engine)?;
        let recipe = registration
            .telemetry_recipes()
            .iter()
            .find(|recipe| recipe.id == recipe_id)
            .cloned()
            .ok_or_else(invalid)?;
        let issuer = registration.native_issuer().ok_or_else(invalid)?;
        request.budget = request.budget.intersection(&recipe.budget);
        let admission = SourceAdmission::acquire(self.collection_admission_path()).await?;
        // The existing AuthenticatedSource records the actual allocated identity when native work starts.
        // Outbox supplies that identity; no request/capture is manufactured by an imported approval.
        let selected = LiveIssuer { workspace: self, name, admission: &admission, registration, issuer, route };
        let result = outbox
            .qualify_native(
                &self.source_gate,
                &selected,
                NativeQualificationRequest {
                    connection: name,
                    route_revision: &route_revision,
                    recipe: &recipe,
                    request,
                    cancellation,
                },
            )
            .await;
        self.settle_collection(&admission).await?;
        let outcome = result?;
        if let Some(binding) = outcome.issued {
            self.qualification_bindings.install(binding)?;
        }
        Ok(outcome.summary)
    }

    /// Authenticate original artifacts within one cumulative local byte/row/deadline allowance. The durable
    /// issued association is necessary; recovered JSON or a matching scope alone cannot populate the cache.
    pub async fn restore_qualifications(
        &self,
        delivery: &OutboxDelivery,
        budget: CompletedCaptureBudget,
        cancellation: &CollectionCancellation,
    ) -> Result<()> {
        let result = self.read_qualification_bindings(delivery, budget, cancellation).await;
        match result {
            Ok(bindings) => self.qualification_bindings.replace(bindings),
            Err(error) => {
                self.qualification_bindings.replace(vec![])?;
                Err(error)
            }
        }
    }

    async fn read_qualification_bindings(
        &self,
        delivery: &OutboxDelivery,
        mut budget: CompletedCaptureBudget,
        cancellation: &CollectionCancellation,
    ) -> Result<Vec<culprit_collector::outbox::IssuedQualification>> {
        budget.validate()?;
        let deadline = Instant::now() + budget.timeout;
        let issued = delivery.issued_qualifications(budget, cancellation).await?;
        budget.max_bytes = budget.max_bytes.checked_sub(issued.bytes_read).ok_or_else(invalid)?;
        let mut accepted = Vec::new();
        for binding in issued.bindings {
            if !binding.is_active() {
                continue;
            }
            if cancellation.is_cancelled() {
                return Err(CollectionStop::Cancelled.error());
            }
            let Ok(stored) = self.store.get(binding.connection()) else {
                continue;
            };
            let connections = self.connection_service();
            if connections.native_route_revision(binding.connection())? != binding.route_revision() {
                continue;
            }
            let registration = connections.selected_registration(stored.engine)?;
            if registration.qualification_component() != Some(&binding.receipt().component) {
                continue;
            }
            budget.timeout = deadline.saturating_duration_since(Instant::now());
            if budget.timeout.is_zero() {
                return Err(CollectionStop::TimedOut.error());
            }
            budget.validate()?;
            let source_id = &binding.receipt().evidence.capture;
            let attempt = DiagnosticAttemptReceipt {
                capture: source_id.clone(),
                manifest_sha256: Some(binding.receipt().evidence.manifest_sha256.clone()),
            };
            let dataset = binding.dataset();
            let captured = delivery
                .completed_snapshot_for_connection(binding.connection(), &attempt, dataset, budget, cancellation)
                .await?;
            budget.max_bytes = budget.max_bytes.checked_sub(captured.bytes_read()).ok_or_else(invalid)?;
            budget.max_rows = budget.max_rows.checked_sub(captured.records().len() as u32).ok_or_else(invalid)?;
            let index = captured
                .row_ordinals()
                .iter()
                .position(|ordinal| *ordinal == binding.receipt().evidence.row_ordinal)
                .ok_or_else(invalid)?;
            if captured.reference(index)? != binding.receipt().evidence
                || !registration.verify_candidate(binding.receipt(), captured.source(), &captured.records()[index])?
            {
                return Err(invalid());
            }
            accepted.push(binding);
        }
        Ok(accepted)
    }

    pub async fn recipe_qualification_status(
        &self,
        name: &str,
        delivery: &OutboxDelivery,
        cancellation: &CollectionCancellation,
    ) -> Result<Vec<RecipeQualificationSummary>> {
        // Removed/renamed connections still expose their original issuance so the operator can release its pin.
        let route = self.connection_service().native_route_revision(name).ok();
        delivery
            .issued_qualifications(CompletedCaptureBudget::default(), cancellation)
            .await?
            .bindings
            .iter()
            .filter(|binding| binding.connection() == name)
            .map(|binding| {
                let mut summary = binding.summary()?;
                if binding.is_active() && Some(binding.route_revision()) != route.as_deref() {
                    summary.state = culprit_core::RecipeQualificationState::Refused;
                    summary.gaps.push(culprit_core::RecipeQualificationGap::ConnectionChanged);
                }
                Ok(summary)
            })
            .collect()
    }

    pub async fn revoke_recipe_qualification(
        &self,
        name: &str,
        recipe_id: &str,
        delivery: &OutboxDelivery,
    ) -> Result<bool> {
        // Withhold locally before the durable transition; a failed commit returns an error and never claims revoke.
        self.qualification_bindings.remove(name, recipe_id)?;
        delivery.revoke_qualification(name, recipe_id).await
    }
}

/// The capture owner supplies the exact allocation; the original authenticated source owns native execution.
struct LiveIssuer<'a> {
    workspace: &'a Workspace,
    name: &'a str,
    admission: &'a SourceAdmission,
    registration: ProviderRegistration,
    issuer: RecipeQualificationIssuer,
    route: crate::connection::SavedRoute,
}
impl NativeQualificationSource for LiveIssuer<'_> {
    fn source_factory<'a>(&'a self, capture: &'a CaptureIdentity) -> Box<dyn SourceFactory + 'a> {
        Box::new(AuthenticatedSource {
            workspace: self.workspace,
            name: self.name,
            admission: self.admission,
            observation_id: &capture.observation_id,
            selection: SourceSelection::Qualification { route: &self.route, registration: &self.registration },
        })
    }
    fn assess(&self, evidence: NativeQualificationEvidence<'_>) -> Result<NativeQualificationDecision> {
        self.workspace.connection_service().require_current_route(&self.route)?;
        let decision =
            (self.issuer)(NativeQualificationEvidence { manifest: evidence.manifest, captured: evidence.captured })?;
        if let NativeQualificationDecision::Approve(receipt) = &decision {
            let index = evidence
                .captured
                .row_ordinals()
                .iter()
                .position(|ordinal| *ordinal == receipt.evidence.row_ordinal)
                .ok_or_else(invalid)?;
            if !self.registration.verify_candidate(
                receipt,
                evidence.captured.source(),
                &evidence.captured.records()[index],
            )? {
                return Err(invalid());
            }
        }
        Ok(decision)
    }
}
fn invalid() -> culprit_core::Error {
    culprit_core::Error::invalid(culprit_core::t!("engine.registry.invalid_qualification"))
}
#[cfg(test)]
mod tests;
