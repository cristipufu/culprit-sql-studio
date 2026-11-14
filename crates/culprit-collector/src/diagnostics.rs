//! Offline analysis runs on sealed evidence. This module has no provider, credential or native query dependency.
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
    assignments: Vec<(String, CollectionSubject, ActivityPolicy)>,
}
impl DiagnosticPolicies {
    pub fn for_activity(connection: &str, subject: CollectionSubject, policy: ActivityPolicy) -> Result<Self> {
        subject.validate()?;
        policy.validate()?;
        if connection.is_empty() || connection.len() > 256 || connection.chars().any(char::is_control) {
            return Err(CollectionStop::InvalidPage.error());
        }
        Ok(Self { assignments: vec![(connection.into(), subject, policy)] })
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
                    Some((target.connection.clone(), target.subject.clone(), policy.clone()))
                })
            })
            .collect();
        Ok(Self { assignments })
    }
    fn for_input(&self, input: &DiagnosticInput) -> ActivityPolicy {
        self.assignments
            .iter()
            .find(|(connection, subject, _)| *connection == input.connection && subject.accepts(&input.source.target))
            .map(|(_, _, policy)| policy.clone())
            .unwrap_or_default()
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
        let update = analyze(&input, &policies.for_input(&input), &analyzed_at)?;
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
    let (source, row) = match outbox.diagnostic_evidence(&focus.parent).await {
        Ok(evidence) => evidence,
        Err(error) if error.detail == Some(ErrorDetail::Outbox(OutboxProblem::MissingCapture)) => {
            let Some(recovery) = recovery else {
                return Err(error);
            };
            if outbox.recovery_store_id() != Some(recovery.id()) {
                return Err(DeliveryProblem::DestinationChanged.error());
            }
            // These reads own no database work and have not started a cache/journal mutation. Retiring them on
            // shutdown leaves the input pending; once the atomic cache write starts, its owner must finish.
            let recovered = tokio::select! { biased;
                _ = cancellation.cancelled() => return Ok(false),
                result = tokio::time::timeout(RECOVERY_READ_TIMEOUT, recover_focus_parent(recovery, focus)) =>
                    result.map_err(|_| Error::new(ErrorCode::Timeout, culprit_core::t!("delivery.storage"))
                        .with_detail(ErrorDetail::Delivery(DeliveryProblem::Storage)))??,
            };
            outbox.cache_diagnostic_evidence(connection, recovered).await?;
            outbox.diagnostic_evidence(&focus.parent).await?
        }
        Err(error) => return Err(error),
    };
    let TelemetryRecord::Activity(original) = &row.record else {
        return Err(CollectionStop::InvalidPage.error());
    };
    if !focus.matches_parent(&source, original, &focus.parent) {
        return Err(CollectionStop::InvalidPage.error());
    }
    Ok(true)
}

async fn recover_focus_parent(
    recovery: &dyn RecoveryStore,
    focus: &ActivityWaitFocus,
) -> Result<diagnostic_recovery::RecoveredDiagnosticInput> {
    let manifest = recovery.capture_manifest(&focus.parent.capture).await?;
    if diagnostic_recovery::capture_reference(&manifest)? != focus.parent.capture_ref() {
        return Err(DeliveryProblem::Integrity.error());
    }
    diagnostic_recovery::verify_reference(&manifest, &focus.parent)?;
    diagnostic_recovery::load_diagnostic_input(recovery, &manifest.range_id).await
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
