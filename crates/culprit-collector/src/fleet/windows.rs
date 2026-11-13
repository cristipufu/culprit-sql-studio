//! Rolling history catches up in bounded windows. Clock rollback cannot move a confirmed checkpoint backwards.
use super::{
    scheduler::state_failed,
    work::{Checkpoint, parse_time},
};
use chrono::{DateTime, TimeDelta, Utc};
use culprit_core::{Result, telemetry::*};

pub(super) fn request(
    schedule: &FleetSchedule,
    subject: &CollectionSubject,
    checkpoint: &Checkpoint,
    now: DateTime<Utc>,
) -> Result<(CollectionRequest, Option<TelemetryRange>)> {
    if let Some(request) = &checkpoint.pending {
        request.validate()?;
        if request.collection() != schedule.collection.collection()
            || request.budget != schedule.budget
            || &request.subject != subject
        {
            return Err(state_failed());
        }
        match &schedule.collection {
            ScheduledCollection::Activity { policy } if request.activity_policy() != Some(policy) => {
                return Err(state_failed());
            }
            ScheduledCollection::ResourceHistory { window_seconds, overlap_seconds }
            | ScheduledCollection::DeadlockHistory { window_seconds, overlap_seconds } => {
                let (from, until) = request.bounds()?;
                if until - from > TimeDelta::seconds(i64::from(window_seconds + overlap_seconds)) {
                    return Err(state_failed());
                }
            }
            _ => {}
        }
        return Ok((request.clone(), checkpoint.pending_gap.clone()));
    }
    let mut gap = None;
    let selection = match &schedule.collection {
        ScheduledCollection::Activity { policy } => CollectionSelection::Activity { policy: policy.clone() },
        ScheduledCollection::Snapshot { dataset } => CollectionSelection::Snapshot { dataset: *dataset },
        ScheduledCollection::ResourceHistory { window_seconds, overlap_seconds }
        | ScheduledCollection::DeadlockHistory { window_seconds, overlap_seconds } => {
            let window = TimeDelta::seconds(i64::from(*window_seconds));
            let overlap = TimeDelta::seconds(i64::from(*overlap_seconds));
            let earliest = now - TimeDelta::seconds(i64::from(MAX_HISTORY_LOOKBACK_SECONDS)) + overlap;
            let mut boundary = checkpoint.last_until.as_deref().map(parse_time).transpose()?.unwrap_or(now - window);
            if boundary < earliest {
                gap = Some(TelemetryRange::TimeWindow { from: boundary.to_rfc3339(), until: earliest.to_rfc3339() });
                boundary = earliest;
            }
            if boundary >= now {
                return Err(CollectionStop::InvalidPage.error());
            }
            let from = (boundary - overlap).to_rfc3339();
            let until = (boundary + window).min(now).to_rfc3339();
            if matches!(schedule.collection, ScheduledCollection::ResourceHistory { .. }) {
                CollectionSelection::ResourceHistory { from, until }
            } else {
                CollectionSelection::DeadlockHistory { from, until }
            }
        }
        ScheduledCollection::QueryHistory { .. } => return Err(state_failed()),
    };
    let request = CollectionRequest { subject: subject.clone(), selection, budget: schedule.budget.clone() };
    request.validate()?;
    Ok((request, gap))
}

#[cfg(test)]
mod tests;
