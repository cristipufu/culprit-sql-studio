use super::{
    admission::AdmittedBackend,
    history,
    journal::FleetJournal,
    scheduler::{FocusedJob, HistoryLane, Job, Outcome, state_failed},
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
    pub source: Option<CollectionDescription>,
    pub outcome: Outcome,
    pub pause_all: bool,
}
pub(super) struct WorkIdentity<'a> {
    target: &'a str,
    connection: &'a str,
    schedule: &'a str,
}
impl<'a> From<&'a Job> for WorkIdentity<'a> {
    fn from(job: &'a Job) -> Self {
        Self { target: &job.target, connection: &job.connection, schedule: &job.schedule.id }
    }
}
impl<'a> From<&'a FocusedJob> for WorkIdentity<'a> {
    fn from(job: &'a FocusedJob) -> Self {
        Self { target: &job.target, connection: &job.connection, schedule: &job.recipe_id }
    }
}
pub(super) async fn execute(
    backend: &AdmittedBackend<'_>,
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
        Err(error) => return capture_failure(job, error, gap, backend.source_work()),
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
    completed_for(job.into(), manifest, gap, more)
}
pub(super) fn completed_for(
    identity: WorkIdentity<'_>,
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
    let native_coverage = manifest
        .description
        .as_ref()
        .and_then(|description| description.observation.snapshot())
        .map(|snapshot| snapshot.coverage_state())
        .transpose()?
        .map(|coverage| {
            if manifest.status != CollectionStatus::Complete
                && matches!(coverage, culprit_core::SourceState::Collected | culprit_core::SourceState::Empty)
            {
                culprit_core::SourceState::Partial
            } else {
                coverage
            }
        });
    let error_code = manifest.files.iter().find(|file| file.status == manifest.status).and_then(|file| file.error_code);
    Ok(WorkResult {
        source: manifest.description,
        event: FleetCollectionEvent {
            target: identity.target.into(),
            connection: identity.connection.into(),
            schedule: identity.schedule.into(),
            work: FleetWork::Capture,
            status: manifest.status,
            native_coverage,
            capture: Some(manifest.capture),
            error_code,
            gap,
        },
        outcome,
        pause_all: false,
    })
}
pub(super) fn failure(job: &Job, work: FleetWork, error: Error, gap: Option<TelemetryRange>) -> Result<WorkResult> {
    failure_for(job.into(), work, error, gap, None)
}
pub(super) fn capture_failure(
    job: &Job,
    error: Error,
    gap: Option<TelemetryRange>,
    source_work: SourceWorkState,
) -> Result<WorkResult> {
    failure_for(job.into(), FleetWork::Capture, error, gap, Some(source_work))
}
pub(super) fn failure_for(
    identity: WorkIdentity<'_>,
    work: FleetWork,
    error: Error,
    gap: Option<TelemetryRange>,
    source_work: Option<SourceWorkState>,
) -> Result<WorkResult> {
    if matches!(source_work, Some(SourceWorkState::Active | SourceWorkState::Unconfirmed))
        || matches!(error.detail, Some(ErrorDetail::CollectionStopped(CollectionStop::CancellationUnconfirmed)))
        || error.code == ErrorCode::Timeout
            && error.detail != Some(ErrorDetail::CollectionStopped(CollectionStop::TimedOut))
            && !matches!(source_work, Some(SourceWorkState::NotStarted | SourceWorkState::Quiescent))
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
        source: None,
        event: FleetCollectionEvent {
            target: identity.target.into(),
            connection: identity.connection.into(),
            schedule: identity.schedule.into(),
            work,
            status: CollectionStatus::Incomplete(stopped),
            native_coverage: None,
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

    #[test]
    fn a_timeout_can_retry_only_after_an_explicit_capture_owner_proves_its_settlement() {
        for source_work in [
            None,
            Some(SourceWorkState::Active),
            Some(SourceWorkState::Unconfirmed),
            Some(SourceWorkState::NotStarted),
            Some(SourceWorkState::Quiescent),
        ] {
            let result = failure_for(
                WorkIdentity { target: "target", connection: "source", schedule: "recipe" },
                FleetWork::Capture,
                culprit_core::limits::timeout_error(),
                None,
                source_work,
            );
            if matches!(source_work, Some(SourceWorkState::NotStarted | SourceWorkState::Quiescent)) {
                let retry = result.unwrap();
                assert!(matches!(retry.outcome, Outcome::Failed));
                assert_ne!(retry.event.status, CollectionStatus::Complete);
                assert_eq!(retry.event.error_code, Some(ErrorCode::Timeout));
            } else {
                assert_eq!(CollectionStop::from_error(&result.err().unwrap()), CollectionStop::CancellationUnconfirmed);
            }
        }
    }

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
