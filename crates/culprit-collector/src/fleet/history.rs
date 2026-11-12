//! Closed intervals are discovered once per bounded scan; cursors and unconsumed intervals survive restarts.
use super::{
    FleetBackend,
    journal::FleetJournal,
    scheduler::{Job, Outcome, state_failed},
    storage::CaptureStorage,
    work::{self, Checkpoint, WorkResult, parse_time},
};
use crate::capture::CollectionCancellation;
use chrono::{DateTime, TimeDelta, Utc};
use culprit_core::{Result, telemetry::*};
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;

const DISCOVERY_PAGE_ROWS: u32 = 8;
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Scan {
    request: IntervalDiscoveryRequest,
    source: Option<String>,
    observed_at: Option<String>,
    pending: VecDeque<TelemetryInterval>,
    discovered: bool,
}
pub(super) struct Policy {
    pub now: DateTime<Utc>,
    pub lookback_seconds: u32,
    pub until_age_seconds: u32,
    pub reconcile_seconds: u32,
}
pub(super) async fn step(
    backend: &dyn FleetBackend,
    journal: &FleetJournal,
    storage: &mut CaptureStorage<'_>,
    job: &Job,
    cancellation: &CollectionCancellation,
    checkpoint: &mut Checkpoint,
    policy: Policy,
) -> Result<WorkResult> {
    if checkpoint.history.is_none() {
        checkpoint.history = Some(Scan {
            request: IntervalDiscoveryRequest {
                from: (policy.now - TimeDelta::seconds(i64::from(policy.lookback_seconds))).to_rfc3339(),
                until: (policy.now - TimeDelta::seconds(i64::from(policy.until_age_seconds))).to_rfc3339(),
                cursor: None,
                limit: DISCOVERY_PAGE_ROWS.min(job.schedule.budget.page_rows),
                budget: job.schedule.budget.clone(),
            },
            source: None,
            observed_at: None,
            pending: VecDeque::new(),
            discovered: false,
        });
        journal.save(&job.key, checkpoint).await?;
    }
    let scan = checkpoint.history.as_ref().ok_or_else(state_failed)?;
    scan.validate(job, checkpoint.target.as_ref().and_then(CollectionTarget::database), &policy)?;
    if scan.pending.is_empty() {
        if cancellation.is_cancelled() {
            return work::failure(job, FleetWork::DiscoverIntervals, CollectionStop::Cancelled.error(), None);
        }
        return discover(backend, journal, job, checkpoint).await;
    }
    let interval = scan.pending.front().ok_or_else(state_failed)?.clone();
    let database = checkpoint.target.as_ref().and_then(CollectionTarget::database).ok_or_else(state_failed)?;
    let source = scan.source.as_ref().ok_or_else(state_failed)?.clone();
    let cutoff = parse_time(scan.observed_at.as_deref().ok_or_else(state_failed)?)?
        - TimeDelta::seconds(i64::from(policy.reconcile_seconds));
    let captured = if parse_time(&interval.until)? < cutoff {
        match storage.captured(&CapturedRange::native_interval(database, &source, &interval)?).await {
            Ok(captured) => captured,
            Err(error) => return work::failure(job, FleetWork::SkipCommitted, error, None),
        }
    } else {
        false
    };
    if captured {
        let more = advance(checkpoint)?;
        journal.save(&job.key, checkpoint).await?;
        return Ok(metadata_result(job, FleetWork::SkipCommitted, more));
    }
    let request = CollectionRequest {
        selection: CollectionSelection::QueryHistory { from: interval.from.clone(), until: interval.until.clone() },
        budget: job.schedule.budget.clone(),
    };
    let manifest = match backend.capture(&job.connection, request.clone(), storage.outbox, cancellation).await {
        Ok(manifest) => manifest,
        Err(error) => return work::failure(job, FleetWork::Capture, error, None),
    };
    if let Some(description) = &manifest.description {
        checkpoint.pin_target(&description.target)?;
        if description.source != source
            || !description.matches_request(&request)
            || description.query_history().is_none_or(|history| history.interval.native_id != interval.native_id)
        {
            return work::failure(job, FleetWork::Capture, CollectionStop::SourceChanged.error(), None);
        }
    }
    if manifest.request != request || manifest.status == CollectionStatus::Complete && manifest.description.is_none() {
        return work::failure(job, FleetWork::Capture, CollectionStop::InvalidPage.error(), None);
    }
    let more = if manifest.status == CollectionStatus::Complete { advance(checkpoint)? } else { true };
    journal.save(&job.key, checkpoint).await?;
    work::completed(job, manifest, None, more)
}
impl Scan {
    fn validate(&self, job: &Job, database: Option<&culprit_core::DatabaseIdentity>, policy: &Policy) -> Result<()> {
        let (from, until) = self.request.bounds()?;
        if self.request.budget != job.schedule.budget
            || self.request.limit != DISCOVERY_PAGE_ROWS.min(job.schedule.budget.page_rows)
            || until - from != TimeDelta::seconds(i64::from(policy.lookback_seconds - policy.until_age_seconds))
        {
            return Err(state_failed());
        }
        if !self.discovered {
            if !self.pending.is_empty()
                || self.request.cursor.is_some()
                || self.source.is_some()
                || self.observed_at.is_some()
            {
                return Err(state_failed());
            }
        } else {
            let page = IntervalDiscoveryPage {
                database: database.ok_or_else(state_failed)?.clone(),
                source: self.source.clone().ok_or_else(state_failed)?,
                observed_at: self.observed_at.clone().ok_or_else(state_failed)?,
                metadata_from: None,
                intervals: self.pending.iter().cloned().collect(),
                next_cursor: None,
            };
            if !page.matches_request(&self.request) {
                return Err(state_failed());
            }
        }
        Ok(())
    }
}
async fn discover(
    backend: &dyn FleetBackend,
    journal: &FleetJournal,
    job: &Job,
    checkpoint: &mut Checkpoint,
) -> Result<WorkResult> {
    let scan = checkpoint.history.as_ref().ok_or_else(state_failed)?;
    if scan.discovered && scan.request.cursor.is_none() {
        return Err(state_failed());
    }
    let page = match backend.discover(&job.connection, &scan.request).await {
        Ok(page) => page,
        Err(error) => return work::failure(job, FleetWork::DiscoverIntervals, error, None),
    };
    if !page.matches_request(&scan.request)
        || scan.source.as_ref().is_some_and(|source| source != &page.source)
        || scan.observed_at.as_ref().is_some_and(|at| at != &page.observed_at)
    {
        return work::failure(job, FleetWork::DiscoverIntervals, CollectionStop::InvalidPage.error(), None);
    }
    checkpoint.pin_target(&CollectionTarget::Database(page.database.clone()))?;
    let scan = checkpoint.history.as_mut().ok_or_else(state_failed)?;
    scan.source = Some(page.source);
    scan.observed_at = Some(page.observed_at);
    scan.pending = page.intervals.into();
    scan.request.cursor = page.next_cursor;
    scan.discovered = true;
    let more = !scan.pending.is_empty() || scan.request.cursor.is_some();
    if !more {
        checkpoint.history = None;
    }
    journal.save(&job.key, checkpoint).await?;
    Ok(metadata_result(job, FleetWork::DiscoverIntervals, more))
}
fn advance(checkpoint: &mut Checkpoint) -> Result<bool> {
    let scan = checkpoint.history.as_mut().ok_or_else(state_failed)?;
    scan.pending.pop_front().ok_or_else(state_failed)?;
    let more = !scan.pending.is_empty() || scan.request.cursor.is_some();
    if !more {
        checkpoint.history = None;
    }
    Ok(more)
}
fn metadata_result(job: &Job, work: FleetWork, more: bool) -> WorkResult {
    WorkResult {
        event: FleetCollectionEvent {
            connection: job.connection.clone(),
            schedule: job.schedule.id.clone(),
            work,
            status: CollectionStatus::Complete,
            capture: None,
            error_code: None,
            gap: None,
        },
        outcome: if work == FleetWork::SkipCommitted {
            Outcome::Skipped { more }
        } else if more {
            Outcome::More
        } else {
            Outcome::Complete
        },
        pause_all: false,
    }
}
