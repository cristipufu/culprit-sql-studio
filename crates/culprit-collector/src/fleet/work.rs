use super::{
    FleetBackend, history,
    journal::FleetJournal,
    scheduler::{HistoryLane, Job, Outcome, state_failed},
    storage::CaptureStorage,
    windows,
};
use crate::capture::CollectionCancellation;
use chrono::{DateTime, Utc};
use culprit_core::{Error, ErrorCode, ErrorDetail, Result, telemetry::*};
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub(super) struct Checkpoint {
    pub target: Option<CollectionTarget>,
    pub last_until: Option<String>,
    pub pending: Option<CollectionRequest>,
    pub pending_gap: Option<TelemetryRange>,
    pub history: Option<history::Scan>,
}
pub(super) struct WorkResult {
    pub event: FleetCollectionEvent,
    pub outcome: Outcome,
    pub pause_all: bool,
}
pub(super) async fn execute(
    backend: &dyn FleetBackend,
    journal: &FleetJournal,
    storage: &mut CaptureStorage<'_>,
    job: &Job,
    cancellation: &CollectionCancellation,
    now: DateTime<Utc>,
) -> Result<WorkResult> {
    let mut checkpoint = journal.load(&job.key).await?;
    if (matches!(job.schedule.collection, ScheduledCollection::QueryHistory { .. }) && checkpoint.pending.is_some())
        || (!matches!(job.schedule.collection, ScheduledCollection::QueryHistory { .. })
            && checkpoint.history.is_some())
    {
        return Err(state_failed());
    }
    if let ScheduledCollection::QueryHistory { lookback_seconds, reconcile_seconds } = job.schedule.collection {
        return history::step(
            backend,
            journal,
            storage,
            job,
            cancellation,
            &mut checkpoint,
            history::Policy {
                now,
                lookback_seconds: if job.lane == HistoryLane::Current { reconcile_seconds } else { lookback_seconds },
                until_age_seconds: if job.lane == HistoryLane::Backfill { reconcile_seconds } else { 0 },
                reconcile_seconds,
            },
        )
        .await;
    }
    let (request, mut gap) = windows::request(&job.schedule, &job.subject, &checkpoint, now)?;
    checkpoint.pending = Some(request.clone());
    checkpoint.pending_gap = gap.clone();
    journal.save(&job.key, &checkpoint).await?;
    let manifest = match backend.capture(&job.connection, request.clone(), storage.outbox, cancellation).await {
        Ok(manifest) => manifest,
        Err(error) => return failure(job, FleetWork::Capture, error, gap),
    };
    if let Some(source) = &manifest.description {
        checkpoint.pin_target(&source.target)?;
    }
    if manifest.request != request
        || manifest.description.as_ref().is_some_and(|source| !source.matches_request(&request))
        || manifest.status == CollectionStatus::Complete && manifest.description.is_none()
    {
        return failure(job, FleetWork::Capture, CollectionStop::InvalidPage.error(), gap);
    }
    let retention_gap = manifest.status == CollectionStatus::Incomplete(CollectionStop::RetentionGap);
    if retention_gap
        && request.collection() == TelemetryCollection::ResourceHistory
        && manifest.description.as_ref().is_none_or(|description| {
            !matches!(&description.observation, SourceObservation::RetainedSamples(history)
                if history.coverage_stop() == Some(CollectionStop::RetentionGap))
        })
    {
        return failure(job, FleetWork::Capture, CollectionStop::InvalidPage.error(), gap);
    }
    if manifest.status == CollectionStatus::Complete || retention_gap {
        if let CollectionSelection::ResourceHistory { from, until }
        | CollectionSelection::DeadlockHistory { from, until } = &request.selection
        {
            checkpoint.last_until = Some(until.clone());
            if retention_gap {
                gap = Some(TelemetryRange::TimeWindow { from: from.clone(), until: until.clone() });
            }
        }
        checkpoint.pending = None;
        checkpoint.pending_gap = None;
    }
    let recorded_gap = gap
        .as_ref()
        .map(|range| {
            let reason = if retention_gap { FleetGapReason::NativeRetention } else { FleetGapReason::BackfillLimit };
            let range = range.canonical()?;
            let id = digest(&serde_json::to_vec(&(&job.key, &range, reason)).map_err(|_| state_failed())?);
            Ok::<_, Error>(FleetGap {
                id,
                target: job.target.clone(),
                connection: job.connection.clone(),
                schedule: job.schedule.id.clone(),
                range,
                reason,
                recorded_at: now.to_rfc3339(),
                capture: Some(manifest.capture.clone()),
            })
        })
        .transpose()?;
    journal.save_progress(&job.key, &checkpoint, recorded_gap).await?;
    completed(job, manifest, gap, false)
}

impl Checkpoint {
    pub fn pin_target(&mut self, target: &CollectionTarget) -> Result<()> {
        let current = target.telemetry_id()?;
        let previous = self.target.as_ref().map(CollectionTarget::telemetry_id).transpose()?;
        if previous.is_some_and(|before| before != current) {
            return Err(Error::invalid(culprit_core::t!("fleet.assignment_changed")));
        }
        self.target = Some(target.clone());
        Ok(())
    }
}

pub(super) fn completed(
    job: &Job,
    manifest: CollectionManifest,
    gap: Option<TelemetryRange>,
    more: bool,
) -> Result<WorkResult> {
    if manifest.status == CollectionStatus::Incomplete(CollectionStop::CancellationUnconfirmed) {
        return Err(CollectionStop::CancellationUnconfirmed.error());
    }
    let outcome = match manifest.status {
        CollectionStatus::Complete => {
            if more {
                Outcome::More
            } else {
                Outcome::Complete
            }
        }
        CollectionStatus::Incomplete(stopped) => retry_outcome(stopped),
    };
    Ok(WorkResult {
        event: FleetCollectionEvent {
            target: job.target.clone(),
            connection: job.connection.clone(),
            schedule: job.schedule.id.clone(),
            work: FleetWork::Capture,
            status: manifest.status,
            capture: Some(manifest.capture),
            error_code: None,
            gap,
        },
        outcome,
        pause_all: false,
    })
}
pub(super) fn failure(job: &Job, work: FleetWork, error: Error, gap: Option<TelemetryRange>) -> Result<WorkResult> {
    if matches!(error.detail, Some(ErrorDetail::CollectionStopped(CollectionStop::CancellationUnconfirmed)))
        || error.code == ErrorCode::Timeout
            && error.detail != Some(ErrorDetail::CollectionStopped(CollectionStop::TimedOut))
    {
        return Err(CollectionStop::CancellationUnconfirmed.error());
    }
    if let Some(ErrorDetail::Outbox(problem)) = error.detail
        && problem != OutboxProblem::Capacity
    {
        return Err(error);
    }
    let stopped = CollectionStop::from_error(&error);
    let outcome = retry_outcome(stopped);
    Ok(WorkResult {
        event: FleetCollectionEvent {
            target: job.target.clone(),
            connection: job.connection.clone(),
            schedule: job.schedule.id.clone(),
            work,
            status: CollectionStatus::Incomplete(stopped),
            capture: None,
            error_code: Some(error.code),
            gap,
        },
        outcome,
        pause_all: error.detail == Some(ErrorDetail::Outbox(OutboxProblem::Capacity)),
    })
}
fn retry_outcome(stopped: CollectionStop) -> Outcome {
    match stopped {
        CollectionStop::SourceNotReady => Outcome::More,
        CollectionStop::Pressure | CollectionStop::PressureUnavailable => Outcome::Pressure,
        _ => Outcome::Failed,
    }
}
pub(super) fn parse_time(value: &str) -> Result<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value).map(|at| at.with_timezone(&Utc)).map_err(|_| state_failed())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pool(namespace: &str) -> CollectionTarget {
        CollectionTarget::Shared(SharedResourceIdentity {
            engine: culprit_core::Engine::Mssql,
            kind: SharedResourceKind::Pool,
            namespace: NativeIdentity::new(namespace),
            native: NativeIdentity::new("pool"),
        })
    }

    #[test]
    fn a_resource_change_or_invalid_identity_cannot_replace_the_pinned_subject() {
        let mut checkpoint = Checkpoint::default();
        let original = pool("instance-a");
        checkpoint.pin_target(&original).unwrap();
        assert!(checkpoint.pin_target(&pool("instance-b")).is_err());
        assert!(checkpoint.pin_target(&pool("")).is_err());
        assert_eq!(checkpoint.target.as_ref(), Some(&original));
    }

    #[test]
    fn native_database_identity_survives_a_rename_without_permitting_a_recreated_database() {
        let mut checkpoint = Checkpoint::default();
        let mut database =
            culprit_core::DatabaseIdentity::server(culprit_core::Engine::Postgres, "app", "first.invalid", 5432, None);
        database.incarnation = Some("native-resource".into());
        checkpoint.pin_target(&database.clone().into()).unwrap();
        database.database = "renamed".into();
        checkpoint.pin_target(&database.clone().into()).unwrap();
        database.incarnation = Some("recreated".into());
        assert!(checkpoint.pin_target(&database.into()).is_err());
    }
}
