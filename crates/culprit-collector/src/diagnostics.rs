//! Offline analysis runs on sealed evidence. This module has no provider, credential or native query dependency.
mod planning;
mod transition;
pub(crate) use transition::set_analysis;
#[cfg(test)]
mod tests;
use crate::{
    capture::CollectionCancellation,
    delivery::{diagnostic_recovery, recovery::RecoveryStore},
    outbox::{DiagnosticInput, DiagnosticUpdate, Outbox},
};
use culprit_core::{Error, ErrorCode, ErrorDetail, Result, telemetry::*};
use std::time::Duration;

const RECOVERY_READ_TIMEOUT: Duration = Duration::from_secs(30);

pub struct DiagnosticPolicies {
    assignments: Vec<DiagnosticAssignment>,
}
struct DiagnosticAssignment {
    connection: String,
    subject: CollectionSubject,
    policy: ActivityPolicy,
    recipes: Option<Vec<RecipeDescriptor>>,
}
impl DiagnosticPolicies {
    pub fn for_activity(connection: &str, subject: CollectionSubject, policy: ActivityPolicy) -> Result<Self> {
        subject.validate()?;
        policy.validate()?;
        if connection.is_empty() || connection.len() > 256 || connection.chars().any(char::is_control) {
            return Err(CollectionStop::InvalidPage.error());
        }
        Ok(Self {
            assignments: vec![DiagnosticAssignment { connection: connection.into(), subject, policy, recipes: None }],
        })
    }
    pub fn from_inventory(inventory: &FleetInventory, shard: &str) -> Result<Self> {
        inventory.validate(shard)?;
        let assignments = inventory
            .targets
            .iter()
            .filter(|target| target.shard == shard)
            .flat_map(|target| {
                target.schedules.iter().filter_map(|schedule| {
                    let ScheduledCollection::Activity { policy } = &schedule.collection else {
                        return None;
                    };
                    Some(DiagnosticAssignment {
                        connection: target.connection.clone(),
                        subject: target.subject.clone(),
                        policy: policy.clone(),
                        recipes: None,
                    })
                })
            })
            .collect();
        Ok(Self { assignments })
    }
    /// Bind declarations from the selected provider without constructing it or resolving source credentials.
    /// No declaration authorizes native work or identifies the resource behind a saved route.
    pub fn with_recipes(mut self, connection: &str, recipes: Vec<RecipeDescriptor>) -> Result<Self> {
        if !recipe_catalog_is_valid(&recipes) {
            return Err(CollectionStop::InvalidPage.error());
        }
        let recipes = planning::canonical_catalog(recipes)?;
        let mut matched = false;
        for assignment in self.assignments.iter_mut().filter(|assignment| assignment.connection == connection) {
            assignment.recipes = Some(recipes.clone());
            matched = true;
        }
        if !matched {
            return Err(CollectionStop::InvalidPage.error());
        }
        Ok(self)
    }
    pub(crate) fn connections(&self) -> std::collections::BTreeSet<&str> {
        self.assignments.iter().map(|assignment| assignment.connection.as_str()).collect()
    }
    fn assignment(&self, input: &DiagnosticInput) -> Option<&DiagnosticAssignment> {
        self.assignments.iter().find(|assignment| {
            assignment.connection == input.connection && assignment.subject.accepts(&input.source.target)
        })
    }
    fn for_input(&self, input: &DiagnosticInput) -> ActivityPolicy {
        self.assignment(input).map(|assignment| assignment.policy.clone()).unwrap_or_default()
        // Recovered evidence can have no current saved route. Its analysis explicitly stores this policy;
        // a policy match never supplies native identity or authorizes a new connection.
    }
}

/// Process a bounded queue prefix inside the host's existing turn. Cancellation stops between atomic commits.
pub async fn analyze_pending(
    outbox: &mut Outbox,
    policies: &DiagnosticPolicies,
    cancellation: &CollectionCancellation,
    limit: u32,
    recovery: Option<&dyn RecoveryStore>,
) -> Result<u32> {
    if limit == 0 || limit > MAX_DIAGNOSTIC_INPUTS as u32 {
        return Err(CollectionStop::InvalidPage.error());
    }
    let mut processed = 0;
    while processed < limit && !cancellation.is_cancelled() {
        let Some(input) = outbox.next_diagnostic_input(DIAGNOSTIC_CONTROLLER_VERSION).await? else {
            break;
        };
        if let SourceObservation::FocusedPageWait { result, .. } = &input.source.observation
            && !verify_focus_parent(outbox, &input.connection, &result.focus, recovery, cancellation).await?
        {
            break;
        }
        let analyzed_at = chrono::Utc::now().to_rfc3339();
        let mut update = analyze(&input, &policies.for_input(&input), &analyzed_at)?;
        planning::qualify_questions(&input, policies.assignment(&input), &mut update)?;
        outbox.commit_diagnostics(input, update).await?;
        processed += 1;
    }
    Ok(processed)
}

/// Drain sealed inputs inside an existing host turn. Each batch is bounded and yields; no source is opened.
pub async fn drain_pending(
    outbox: &mut Outbox,
    policies: &DiagnosticPolicies,
    cancellation: &CollectionCancellation,
    recovery: Option<&dyn RecoveryStore>,
) -> Result<()> {
    while !cancellation.is_cancelled() {
        if analyze_pending(outbox, policies, cancellation, MAX_DIAGNOSTIC_INPUTS as u32, recovery).await?
            < MAX_DIAGNOSTIC_INPUTS as u32
        {
            break;
        }
        tokio::task::yield_now().await;
    }
    Ok(())
}

async fn verify_focus_parent(
    outbox: &mut Outbox,
    connection: &str,
    focus: &ActivityWaitFocus,
    recovery: Option<&dyn RecoveryStore>,
    cancellation: &CollectionCancellation,
) -> Result<bool> {
    let Some((source, row)) = resolve_evidence(outbox, connection, &focus.parent, recovery, cancellation).await? else {
        return Ok(false);
    };
    let TelemetryRecord::Activity(original) = &row.record else {
        return Err(CollectionStop::InvalidPage.error());
    };
    if !focus.matches_parent(&source, original, &focus.parent) {
        return Err(CollectionStop::InvalidPage.error());
    }
    Ok(true)
}

/// Authenticate exact local evidence first. Only a genuinely absent capture permits bounded archive recovery.
/// Activity parents are cached as evidence only, so a later native reservation can pin the same original row.
/// Other artifacts remain transient. Neither path installs or acknowledges native work.
pub(crate) async fn resolve_evidence(
    outbox: &mut Outbox,
    connection: &str,
    reference: &DiagnosticEvidenceRef,
    recovery: Option<&dyn RecoveryStore>,
    cancellation: &CollectionCancellation,
) -> Result<Option<(CollectionDescription, ExportedRecord)>> {
    if cancellation.is_cancelled() {
        return Ok(None);
    }
    let evidence = match outbox.diagnostic_evidence(reference).await {
        Ok(evidence) => evidence,
        Err(error) if error.detail == Some(ErrorDetail::Outbox(OutboxProblem::MissingCapture)) => {
            let Some(recovery) = recovery else {
                return Err(error);
            };
            if outbox.recovery_store_id() != Some(recovery.id()) {
                return Err(DeliveryProblem::DestinationChanged.error());
            }
            if reference.stream == TelemetryStream::ActivityObservations {
                let Some(recovered) =
                    read_recovery(recover_activity_evidence(recovery, reference), cancellation).await?
                else {
                    return Ok(None);
                };
                // Cache mutation owns its transaction even if cancellation arrives after the read has settled.
                outbox.cache_diagnostic_evidence(connection, recovered).await?;
                outbox.diagnostic_evidence(reference).await?
            } else {
                let Some(recovered) =
                    read_recovery(diagnostic_recovery::load_diagnostic_evidence(recovery, reference), cancellation)
                        .await?
                else {
                    return Ok(None);
                };
                recovered.into_parts()
            }
        }
        Err(error) => return Err(error),
    };
    Ok(Some(evidence))
}

async fn recover_activity_evidence(
    recovery: &dyn RecoveryStore,
    reference: &DiagnosticEvidenceRef,
) -> Result<diagnostic_recovery::RecoveredDiagnosticInput> {
    let advertised = recovery.capture_manifest(&reference.capture).await?;
    diagnostic_recovery::verify_reference(&advertised, reference)?;
    let advertised_digest = advertised.digest()?;
    let recovered = diagnostic_recovery::load_diagnostic_input(recovery, &advertised.range_id).await?;
    if recovered.range().manifest_digest() != advertised_digest {
        return Err(DeliveryProblem::Integrity.error());
    }
    diagnostic_recovery::verify_reference(recovered.range().manifest(), reference)?;
    Ok(recovered)
}

async fn read_recovery<T>(
    read: impl std::future::Future<Output = Result<T>>,
    cancellation: &CollectionCancellation,
) -> Result<Option<T>> {
    // No source or cache mutation has started. Dropping this read on shutdown leaves every obligation untouched.
    tokio::select! { biased;
        _ = cancellation.cancelled() => Ok(None),
        result = tokio::time::timeout(RECOVERY_READ_TIMEOUT, read) =>
            result.map_err(|_| Error::new(ErrorCode::Timeout, culprit_core::t!("delivery.storage"))
                .with_detail(ErrorDetail::Delivery(DeliveryProblem::Storage)))?.map(Some),
    }
}

fn analyze(input: &DiagnosticInput, policy: &ActivityPolicy, analyzed_at: &str) -> Result<DiagnosticUpdate> {
    let (revisions, checkpoint) = match &input.source.observation {
        SourceObservation::Activity(_) => {
            let rows = input
                .rows
                .iter()
                .map(|row| match &row.row.record {
                    TelemetryRecord::Activity(observation) => {
                        Ok(DiagnosticActivityRow { reference: &row.reference, observation })
                    }
                    _ => Err(CollectionStop::InvalidPage.error()),
                })
                .collect::<Result<Vec<_>>>()?;
            let update = observe_diagnostic_activity(
                &input.source,
                &input.parent,
                &rows,
                input.checkpoint.as_ref(),
                policy,
                analyzed_at,
            )?;
            (update.revisions, update.checkpoint)
        }
        SourceObservation::FocusedPageWait { .. } => {
            let rows = input
                .rows
                .iter()
                .map(|row| match &row.row.record {
                    TelemetryRecord::Operational(OperationalObservation {
                        fact: OperationalFact::PageWait(observation),
                        ..
                    }) => Ok(DiagnosticFocusRow { reference: &row.reference, observation }),
                    _ => Err(CollectionStop::InvalidPage.error()),
                })
                .collect::<Result<Vec<_>>>()?;
            let previous = input.checkpoint.as_ref().ok_or_else(|| DeliveryProblem::Conflict.error())?;
            let update = analyze_diagnostic_focus(&input.source, &input.parent, &rows, previous, analyzed_at)?;
            (update.revisions, update.checkpoint)
        }
        _ => return Err(CollectionStop::InvalidPage.error()),
    };
    let step_id = digest(
        &serde_json::to_vec(&(
            "diagnostic-analysis-v1",
            input.controller_version,
            &input.parent,
            &input.checkpoint,
            matches!(input.source.observation, SourceObservation::Activity(_)).then_some(policy),
        ))
        .map_err(|_| CollectionStop::InvalidPage.error())?,
    );
    Ok(DiagnosticUpdate { step_id, analyzed_at: analyzed_at.into(), revisions, checkpoint })
}
