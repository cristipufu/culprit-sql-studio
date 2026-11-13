//! A conservative single-target loop. No overlapping reads, catch-up bursts or incident resolution on failed polls.
use crate::Workspace;
use culprit_collector::{capture::CollectionCancellation, outbox::Outbox};
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
        self.connection(name)?;
        let mut summary = ActivityMonitorSummary::default();
        let mut failures = 0u32;
        loop {
            if cancellation.is_cancelled() {
                break;
            }
            summary.attempts = summary.attempts.checked_add(1).ok_or_else(|| CollectionStop::QueryLimit.error())?;
            let request = CollectionRequest {
                subject: culprit_core::telemetry::CollectionSubject::Database,
                selection: CollectionSelection::Activity { policy: options.policy.clone() },
                budget: options.budget.clone(),
            };
            let result = self.collect_into_outbox(name, request, outbox, cancellation).await;
            let event = match result {
                Ok(manifest) => event(summary.attempts, manifest),
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
                        active_incidents: None,
                        changes: None,
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
fn event(attempt: u32, manifest: CollectionManifest) -> ActivityMonitorEvent {
    let complete = manifest.status == CollectionStatus::Complete;
    ActivityMonitorEvent {
        attempt,
        status: manifest.status,
        error_code: None,
        capture: Some(manifest.capture),
        observed_at: manifest
            .description
            .as_ref()
            .and_then(|source| source.observation.activity())
            .map(|snapshot| snapshot.at.clone()),
        active_incidents: complete.then(|| count(&manifest.files, TelemetryStream::ActivityCheckpoints)),
        changes: complete.then(|| count(&manifest.files, TelemetryStream::ActivityIncidents)),
        source_queries: Some(manifest.source_queries),
    }
}
fn count(files: &[CollectedFile], stream: TelemetryStream) -> u32 {
    files.iter().find(|file| file.stream == stream).map_or(0, |file| file.rows)
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
