//! Only a live selected native issuer can install authority; sealed/imported JSON remains evidence alone.
use super::completed_capture::ReadControl;
use super::{CompletedCaptureBudget, CompletedSnapshot, Outbox, OutboxDelivery, problem, storage};
use crate::capture::{self, CollectionCancellation, SourceFactory};
use culprit_core::{
    RecipeQualificationGap, RecipeQualificationState, RecipeQualificationSummary, Result, telemetry::*,
};
use culprit_read::admission::SourceGate;
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    sync::{
        Arc, Weak,
        atomic::{AtomicBool, Ordering},
    },
};
use tokio::time::Instant;

pub(super) mod journal;
#[cfg(test)]
mod tests;

/// Bounded journal metadata copied by the existing exclusive owner. Original artifact bytes are read separately.
pub struct QualificationBindingsRead {
    pub bindings: Vec<IssuedQualification>,
    pub bytes_read: u64,
}

/// Safe display plus the live issuance capability from the same canonical verification/commit owner.
/// Only restart restoration needs to read those original artifacts again.
#[derive(Debug)]
pub struct NativeQualificationOutcome {
    pub summary: RecipeQualificationSummary,
    pub issued: Option<IssuedQualification>,
}

/// Trusted executable source policy, explicitly selected by the host's provider registration.
/// Implementing this interface is a trust decision, like replacing the provider factory. Serialized data cannot.
/// The source factory must observe native work through the same reader that performs the recipe.
pub trait NativeQualificationSource: Send + Sync {
    fn source_factory<'a>(&'a self, capture: &'a CaptureIdentity) -> Box<dyn SourceFactory + 'a>;
    fn assess(&self, evidence: NativeQualificationEvidence<'_>) -> Result<NativeQualificationDecision>;
}

/// Borrowed originals authenticated by this Outbox owner, never constructed from caller-supplied JSON.
pub struct NativeQualificationEvidence<'a> {
    pub manifest: &'a CollectionManifest,
    pub captured: &'a CompletedSnapshot,
}

pub use culprit_core::RecipeQualificationAssessment as NativeQualificationDecision;

pub struct NativeQualificationRequest<'a> {
    pub connection: &'a str,
    /// Saved source route only, before adding this connection's installed approval-set revision.
    pub route_revision: &'a str,
    pub recipe: &'a RecipeDescriptor,
    pub request: CollectionRequest,
    pub cancellation: &'a CollectionCancellation,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Binding {
    connection: String,
    route_revision: String,
    receipt: RecipeCostReceipt,
    active: bool,
    dataset: SnapshotDataset,
    revision: u64,
}

impl Binding {
    fn id(&self) -> Result<String> {
        if self.connection.is_empty()
            || self.connection.len() > 512
            || !valid_digest(&self.route_revision)
            || self.revision == 0
            || !self.receipt.is_valid()
        {
            return Err(integrity());
        }
        serde_json::to_vec(&(
            "local-native-qualification-v1",
            &self.connection,
            &self.route_revision,
            self.receipt.key()?,
            self.revision,
        ))
        .map(|bytes| digest(&bytes))
        .map_err(|_| integrity())
    }
}

/// A trusted local issuance association; the consumer must authenticate its original artifact before use.
/// It deliberately has no Deserialize/Serialize implementation or public data constructor.
#[derive(Clone, Debug)]
pub struct IssuedQualification {
    binding: Binding,
    live: Arc<AtomicBool>,
}
impl IssuedQualification {
    pub fn is_active(&self) -> bool {
        self.binding.active && self.live.load(Ordering::Acquire)
    }
    pub fn summary(&self) -> Result<RecipeQualificationSummary> {
        let (state, gaps) = if !self.binding.active {
            (RecipeQualificationState::Revoked, vec![])
        } else if self.is_active() {
            (RecipeQualificationState::Installed, vec![])
        } else {
            (RecipeQualificationState::Refused, vec![RecipeQualificationGap::ApprovalRetired])
        };
        Ok(RecipeQualificationSummary {
            recipe_id: self.binding.receipt.recipe_id.clone(),
            recipe_version: self.binding.receipt.recipe_version,
            state,
            approval_id: Some(self.id()?),
            capture: Some(self.binding.receipt.evidence.capture.clone()),
            source_queries: None,
            read_work: None,
            gaps,
        })
    }
    pub fn connection(&self) -> &str {
        &self.binding.connection
    }
    pub fn route_revision(&self) -> &str {
        &self.binding.route_revision
    }
    pub fn receipt(&self) -> &RecipeCostReceipt {
        &self.binding.receipt
    }
    pub fn dataset(&self) -> SnapshotDataset {
        self.binding.dataset
    }
    pub fn revision(&self) -> u64 {
        self.binding.revision
    }
    pub fn id(&self) -> Result<String> {
        self.binding.id()
    }
}

/// The existing Files owner fences every borrowed approval before it releases the exclusive Outbox lock.
/// This state is never serialized: restart must authenticate the journal association and original artifact again.
#[derive(Default)]
pub(super) struct QualificationLeases(BTreeMap<String, Weak<AtomicBool>>);
impl QualificationLeases {
    fn issue(&mut self, binding: Binding) -> Result<IssuedQualification> {
        let id = binding.id()?;
        self.0.retain(|_, token| token.strong_count() > 0);
        let live = self
            .0
            .get(&id)
            .and_then(Weak::upgrade)
            .filter(|live| live.load(Ordering::Acquire))
            .unwrap_or_else(|| Arc::new(AtomicBool::new(binding.active)));
        self.0.insert(id, Arc::downgrade(&live));
        Ok(IssuedQualification { binding, live })
    }
    fn retire(&mut self, id: &str) {
        if let Some(live) = self.0.remove(id).and_then(|token| token.upgrade()) {
            live.store(false, Ordering::Release);
        }
    }
}
impl Drop for QualificationLeases {
    fn drop(&mut self) {
        for live in self.0.values().filter_map(Weak::upgrade) {
            live.store(false, Ordering::Release);
        }
    }
}

impl Outbox {
    /// One existing capture owner performs the actual run, seals it, then invokes only its trusted live issuer.
    /// No install-from-file/receipt API exists. Refused measurements remain available as ordinary evidence.
    pub async fn qualify_native(
        &mut self,
        gate: &SourceGate,
        source: &dyn NativeQualificationSource,
        input: NativeQualificationRequest<'_>,
    ) -> Result<NativeQualificationOutcome> {
        validate_input(&input)?;
        if input.cancellation.is_cancelled() {
            return Err(CollectionStop::Cancelled.error());
        }
        let mut lease = self.reserve(input.connection, &input.request).await?;
        let identity = lease.identity().clone();
        let directory = lease.directory().to_owned();
        let factory = source.source_factory(&identity);
        let result = capture::collect(
            gate,
            factory.as_ref(),
            input.request.clone(),
            &directory,
            identity.clone(),
            input.cancellation,
            Some(&mut lease),
        )
        .await;
        let finished = lease.finish().await;
        finished?;
        let manifest = result?;
        let mut summary = summary(input.recipe, &manifest)?;
        if manifest.status != CollectionStatus::Complete {
            summary.gaps.push(RecipeQualificationGap::SourceNotComplete);
            return Ok(NativeQualificationOutcome { summary, issued: None });
        }
        let attempt = self.attempt_receipt(&identity, None).await?;
        let description = manifest.description.as_ref().ok_or_else(integrity)?;
        let dataset = description.observation.snapshot().ok_or_else(integrity)?.dataset;
        let captured = self
            .delivery()
            .completed_snapshot_for_connection(
                input.connection,
                &attempt,
                dataset,
                CompletedCaptureBudget::default(),
                input.cancellation,
            )
            .await?;
        let receipt = match source.assess(NativeQualificationEvidence { manifest: &manifest, captured: &captured })? {
            NativeQualificationDecision::Refuse(gaps) => {
                summary.gaps = gaps;
                return Ok(NativeQualificationOutcome { summary, issued: None });
            }
            NativeQualificationDecision::Approve(receipt) => receipt,
        };
        validate_receipt(&receipt, input.recipe, &manifest, &captured)?;
        let binding = Binding {
            connection: input.connection.into(),
            route_revision: input.route_revision.into(),
            receipt: *receipt,
            active: true,
            dataset,
            revision: 1,
        };
        let cancellation = input.cancellation.clone();
        let issued = self
            .files
            .run(|| {
                move |files| {
                    if cancellation.is_cancelled() {
                        return Err(CollectionStop::Cancelled.error());
                    }
                    files.install_qualification(binding, captured)
                }
            })
            .await?;
        summary.state = RecipeQualificationState::Installed;
        summary.approval_id = Some(issued.id()?);
        Ok(NativeQualificationOutcome { summary, issued: Some(issued) })
    }

    pub async fn revoke_qualification(&mut self, connection: &str, recipe_id: &str) -> Result<bool> {
        self.delivery().revoke_qualification(connection, recipe_id).await
    }
}

impl OutboxDelivery {
    pub async fn revoke_qualification(&self, connection: &str, recipe_id: &str) -> Result<bool> {
        let connection = connection.to_owned();
        let recipe_id = recipe_id.to_owned();
        self.acquire_files().await.run_owned((), move |files| files.revoke_qualification(&connection, &recipe_id)).await
    }
    /// Missing/corrupt originals refuse restoration; a raw archived receipt cannot replace the issuance row.
    pub async fn issued_qualifications(
        &self,
        budget: CompletedCaptureBudget,
        cancellation: &CollectionCancellation,
    ) -> Result<QualificationBindingsRead> {
        budget.validate()?;
        let control = ReadControl::Collection {
            cancellation: cancellation.clone(),
            deadline: Instant::now().checked_add(budget.timeout).ok_or_else(|| CollectionStop::TimedOut.error())?,
        };
        control.check()?;
        let access = tokio::select! {
            biased;
            error = control.stopped() => return Err(error),
            access = self.acquire_files() => access,
        };
        access
            .run_owned((), move |files| {
                control.check()?;
                let result = files.issued_qualifications(&control, budget)?;
                control.check()?;
                Ok(result)
            })
            .await
    }
}

fn validate_input(input: &NativeQualificationRequest<'_>) -> Result<()> {
    input.request.validate()?;
    if input.connection.is_empty()
        || input.connection.len() > 512
        || !valid_digest(input.route_revision)
        || !input.recipe.is_valid()
        || input.recipe.purpose != RecipePurpose::Investigation
        || !input.recipe.selector.matches(&input.request)
        || !input.recipe.subjects.iter().any(|subject| subject.accepts(&input.request.subject))
        || input.request.budget.intersection(&input.recipe.budget) != input.request.budget
    {
        return Err(integrity());
    }
    Ok(())
}
fn validate_receipt(
    receipt: &RecipeCostReceipt,
    recipe: &RecipeDescriptor,
    manifest: &CollectionManifest,
    captured: &CompletedSnapshot,
) -> Result<()> {
    if !receipt.matches_recipe(recipe)
        || receipt.evidence.capture_ref() != *captured.capture()
        || manifest.read_metrics.as_ref() != Some(&receipt.read_metrics)
        || manifest.request.budget != receipt.tested_budget
    {
        return Err(integrity());
    }
    let index = captured
        .row_ordinals()
        .iter()
        .position(|ordinal| *ordinal == receipt.evidence.row_ordinal)
        .ok_or_else(integrity)?;
    if captured.reference(index)? != receipt.evidence {
        return Err(integrity());
    }
    Ok(())
}
fn summary(recipe: &RecipeDescriptor, manifest: &CollectionManifest) -> Result<RecipeQualificationSummary> {
    Ok(RecipeQualificationSummary {
        recipe_id: recipe.id.clone(),
        recipe_version: recipe.version,
        state: RecipeQualificationState::Refused,
        approval_id: None,
        capture: Some(manifest.capture.clone()),
        source_queries: Some(manifest.source_queries),
        read_work: manifest
            .read_metrics
            .as_ref()
            .map(|metrics| metrics.summary(manifest.description.as_ref()))
            .transpose()?,
        gaps: vec![],
    })
}
fn integrity() -> culprit_core::Error {
    problem(OutboxProblem::Integrity)
}
