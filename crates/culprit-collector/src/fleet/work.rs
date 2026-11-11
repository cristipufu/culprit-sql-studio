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
    pub database: Option<culprit_core::DatabaseIdentity>,
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
    let (request, mut gap) = windows::request(&job.schedule, &checkpoint, now)?;
    checkpoint.pending = Some(request.clone());
    checkpoint.pending_gap = gap.clone();
    journal.save(&job.key, &checkpoint).await?;
    let manifest = match backend.capture(&job.connection, request.clone(), storage.outbox, cancellation).await {
        Ok(manifest) => manifest,
        Err(error) => return failure(job, FleetWork::Capture, error, gap),
    };
    if let Some(source) = &manifest.description {
        checkpoint.pin_database(&source.database)?;
    }
    if manifest.request != request
        || manifest.description.as_ref().is_some_and(|source| !source.matches_request(&request))
        || manifest.status == CollectionStatus::Complete && manifest.description.is_none()
    {
        return failure(job, FleetWork::Capture, CollectionStop::InvalidPage.error(), gap);
    }
    let retention_gap = manifest.status == CollectionStatus::Incomplete(CollectionStop::RetentionGap);
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
    pub fn pin_database(&mut self, database: &culprit_core::DatabaseIdentity) -> Result<()> {
        if self.database.as_ref().is_some_and(|before| before != database) {
            return Err(Error::invalid(culprit_core::t!("fleet.assignment_changed")));
        }
        self.database = Some(database.clone());
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
        CollectionStatus::Incomplete(CollectionStop::Pressure | CollectionStop::PressureUnavailable) => {
            Outcome::Pressure
        }
        _ => Outcome::Failed,
    };
    Ok(WorkResult {
        event: FleetCollectionEvent {
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
    let outcome = if matches!(stopped, CollectionStop::Pressure | CollectionStop::PressureUnavailable) {
        Outcome::Pressure
    } else {
        Outcome::Failed
    };
    Ok(WorkResult {
        event: FleetCollectionEvent {
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
pub(super) fn parse_time(value: &str) -> Result<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value).map(|at| at.with_timezone(&Utc)).map_err(|_| state_failed())
}
