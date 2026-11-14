//! A conservative single-target loop. No overlapping reads, catch-up bursts or incident resolution on failed polls.
use crate::Workspace;
use culprit_collector::{
    capture::CollectionCancellation,
    diagnostics::{DiagnosticPolicies, drain_pending},
    outbox::Outbox,
};
use culprit_core::{Result, telemetry::*};
use std::time::Duration;

const MAX_FAILURE_DELAY_SECONDS: u64 = 900;
impl Workspace {
    pub async fn monitor_activity(
        &self,
        name: &str,
        options: &ActivityMonitorOptions,
        outbox: &mut Outbox,
        cancellation: &CollectionCancellation,
        mut report: impl FnMut(&ActivityMonitorEvent) -> Result<()>,
    ) -> Result<ActivityMonitorSummary> {
        options.validate()?;
        let policies = DiagnosticPolicies::for_activity(name, CollectionSubject::Database, options.policy.clone())?;
        drain_pending(outbox, &policies, cancellation, None).await?;
        self.connection(name)?;
        let policies = policies.with_recipes(name, self.telemetry_recipes(name)?)?;
        let mut summary = ActivityMonitorSummary::default();
        let mut failures = 0u32;
        loop {
            if cancellation.is_cancelled() {
                break;
            }
            summary.attempts = summary.attempts.checked_add(1).ok_or_else(|| CollectionStop::QueryLimit.error())?;
            let request = CollectionRequest {
                subject: culprit_core::telemetry::CollectionSubject::Database,
                selection: CollectionSelection::Activity,
                budget: options.budget.clone(),
            };
            let result = self.collect_into_outbox(name, request, outbox, cancellation).await;
            let event = match result {
                Ok(manifest) => {
                    if manifest.status == CollectionStatus::Complete {
                        drain_pending(outbox, &policies, cancellation, None).await?;
                    }
                    event(summary.attempts, manifest, outbox).await?
                }
                Err(error) => {
                    if matches!(error.detail, Some(culprit_core::ErrorDetail::Outbox(_)))
                        || matches!(
                            error.detail,
                            Some(culprit_core::ErrorDetail::CollectionStopped(CollectionStop::CancellationUnconfirmed))
                        )
                    {
                        return Err(error);
                    }
                    ActivityMonitorEvent {
                        attempt: summary.attempts,
                        status: CollectionStatus::Incomplete(CollectionStop::from_error(&error)),
                        error_code: Some(error.code),
                        capture: None,
                        observed_at: None,
                        active_episodes: None,
                        uncertain_episodes: None,
                        native_coverage: None,
                        source_queries: None,
                    }
                }
            };
            if event.status == CollectionStatus::Complete {
                summary.complete += 1;
                failures = 0;
            } else {
                summary.unknown += 1;
                failures = failures.saturating_add(1);
            }
            report(&event)?;
            if event.status == CollectionStatus::Incomplete(CollectionStop::CancellationUnconfirmed) {
                return Err(CollectionStop::CancellationUnconfirmed.error());
            }
            if cancellation.is_cancelled() || options.max_polls.is_some_and(|limit| summary.attempts >= limit) {
                break;
            }
            let delay = delay(options.poll_seconds, failures);
            tokio::select! {biased;_=cancellation.cancelled()=>break,_=tokio::time::sleep(delay)=>{}}
        }
        Ok(summary)
    }
}
async fn event(attempt: u32, manifest: CollectionManifest, outbox: &Outbox) -> Result<ActivityMonitorEvent> {
    let episodes =
        if manifest.status == CollectionStatus::Complete { episode_counts(outbox, &manifest).await? } else { None };
    Ok(ActivityMonitorEvent {
        attempt,
        status: manifest.status,
        error_code: None,
        observed_at: manifest
            .description
            .as_ref()
            .and_then(|source| source.observation.activity())
            .map(|snapshot| snapshot.at.clone()),
        native_coverage: manifest
            .description
            .as_ref()
            .and_then(|source| source.observation.activity())
            .map(|snapshot| snapshot.coverage.clone()),
        active_episodes: episodes.as_ref().map(|counts| counts.active),
        uncertain_episodes: episodes.as_ref().map(|counts| counts.uncertain),
        capture: Some(manifest.capture),
        source_queries: Some(manifest.source_queries),
    })
}
struct EpisodeCounts {
    active: u32,
    uncertain: u32,
}
async fn episode_counts(outbox: &Outbox, manifest: &CollectionManifest) -> Result<Option<EpisodeCounts>> {
    let Some(source) = &manifest.description else { return Ok(None) };
    let target = source.target.telemetry_id()?;
    let scope = source.captured_range()?.scope_id;
    let mut cursor: Option<String> = None;
    for _ in 0..MAX_FLEET_TARGETS {
        let page = outbox.diagnostic_checkpoints(DIAGNOSTIC_CONTROLLER_VERSION, cursor.as_deref(), 128).await?;
        for checkpoint in page.checkpoints {
            if checkpoint.target.telemetry_id()? == target
                && checkpoint.input.capture == manifest.capture
                && checkpoint.input.source_scope_id == scope
            {
                let mut counts = EpisodeCounts { active: 0, uncertain: 0 };
                for episode in checkpoint.episodes.iter().filter(|episode| episode.scope.source == source.source) {
                    match episode.lifecycle {
                        DiagnosticLifecycle::Active => counts.active += 1,
                        DiagnosticLifecycle::Uncertain => counts.uncertain += 1,
                        DiagnosticLifecycle::Resolved => {}
                    }
                }
                return Ok(Some(counts));
            }
        }
        let Some(next) = page.next_cursor else { return Ok(None) };
        if cursor.as_ref().is_some_and(|before| next <= *before) {
            return Err(CollectionStop::InvalidPage.error());
        }
        cursor = Some(next);
    }
    Err(CollectionStop::InvalidPage.error())
}

fn delay(interval: u32, failures: u32) -> Duration {
    let multiplier = 1u64 << failures.saturating_sub(1).min(10);
    Duration::from_secs(
        u64::from(interval).saturating_mul(multiplier).min(MAX_FAILURE_DELAY_SECONDS.max(u64::from(interval))),
    )
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn failure_backoff_never_accelerates_a_configured_poll_or_exceeds_its_ceiling() {
        assert_eq!(delay(60, 0), Duration::from_secs(60));
        assert_eq!(delay(60, 1), Duration::from_secs(60));
        assert_eq!(delay(60, 2), Duration::from_secs(120));
        assert_eq!(delay(60, 99), Duration::from_secs(900));
        assert_eq!(delay(3600, 99), Duration::from_secs(3600));
    }
}
