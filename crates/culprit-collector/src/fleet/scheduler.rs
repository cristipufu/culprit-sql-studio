//! Fair, serial source admission. A completed read schedules future work from completion, never from missed ticks.
use culprit_core::{Result, telemetry::*};
use std::{collections::BTreeMap, time::Duration};

const RESOURCE_GAP: Duration = Duration::from_secs(60);
const MAX_FAILURE_DELAY: Duration = Duration::from_secs(900);

#[derive(Clone)]
pub(super) struct Job {
    pub connection: String,
    pub resource: String,
    pub schedule: FleetSchedule,
    pub key: String,
    pub lane: HistoryLane,
    family: String,
}
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum HistoryLane {
    Current,
    Backfill,
}
struct ScheduledJob {
    job: Job,
    due: Duration,
    attempts: u32,
    failures: u32,
    turn: u64,
}
pub(super) enum Outcome {
    Complete,
    More,
    Failed,
    Pressure,
    Skipped { more: bool },
}
#[derive(Default)]
struct ResourceAdmission {
    due: Duration,
    pressure_failures: u32,
}
pub(super) struct Scheduler {
    jobs: Vec<ScheduledJob>,
    resources: BTreeMap<String, ResourceAdmission>,
    families: BTreeMap<String, Duration>,
    sequence: u64,
    active: Option<usize>,
}
impl Scheduler {
    pub fn new(inventory: &FleetInventory, shard: &str) -> Result<Self> {
        inventory.validate(shard)?;
        let targets: Vec<_> = inventory.targets.iter().filter(|target| target.shard == shard).collect();
        let mut jobs = Vec::new();
        // Interleave targets before taking their second source, so catalog-heavy databases cannot monopolize a turn.
        for index in 0..MAX_TARGET_SCHEDULES {
            for target in &targets {
                if let Some(schedule) = target.schedules.get(index) {
                    let lanes = if matches!(schedule.collection, ScheduledCollection::QueryHistory {lookback_seconds,reconcile_seconds} if lookback_seconds>reconcile_seconds)
                    {
                        &[HistoryLane::Current, HistoryLane::Backfill][..]
                    } else {
                        &[HistoryLane::Current][..]
                    };
                    for lane in lanes {
                        let key = digest(
                            &serde_json::to_vec(&(&target.connection, &schedule.id, *lane == HistoryLane::Backfill))
                                .map_err(|_| state_failed())?,
                        );
                        let job = Job {
                            connection: target.connection.clone(),
                            resource: target.admission_group.clone(),
                            schedule: schedule.clone(),
                            key,
                            lane: *lane,
                            family: digest(
                                &serde_json::to_vec(&(&target.connection, &schedule.id)).map_err(|_| state_failed())?,
                            ),
                        };
                        let due = jitter(&job, 0);
                        let turn = jobs.len() as u64;
                        jobs.push(ScheduledJob { job, due, attempts: 0, failures: 0, turn });
                    }
                }
            }
        }
        let sequence = jobs.len() as u64;
        Ok(Self { jobs, resources: BTreeMap::new(), families: BTreeMap::new(), sequence, active: None })
    }
    pub fn take(&mut self, now: Duration) -> Result<Option<Job>> {
        if self.active.is_some() {
            return Err(state_failed());
        }
        let selected = self
            .jobs
            .iter()
            .enumerate()
            .filter(|(_, pending)| {
                pending.due <= now
                    && self.resources.get(&pending.job.resource).is_none_or(|state| state.due <= now)
                    && self.families.get(&pending.job.family).is_none_or(|due| *due <= now)
            })
            .max_by(|(_, left), (_, right)| urgency(left, right, now))
            .map(|(index, _)| index);
        let Some(index) = selected else { return Ok(None) };
        let next_sequence = self.sequence.checked_add(1).ok_or_else(state_failed)?;
        self.jobs[index].turn = self.sequence;
        self.sequence = next_sequence;
        self.active = Some(index);
        Ok(Some(self.jobs[index].job.clone()))
    }
    pub fn finish(&mut self, now: Duration, outcome: Outcome) -> Result<()> {
        let index = self.active.take().ok_or_else(state_failed)?;
        let pending = &mut self.jobs[index];
        pending.attempts = pending.attempts.saturating_add(1);
        pending.failures =
            if matches!(outcome, Outcome::Failed | Outcome::Pressure) { pending.failures.saturating_add(1) } else { 0 };
        let delay = if matches!(outcome, Outcome::Skipped { more: true }) {
            Duration::ZERO
        } else {
            failure_delay(pending.job.schedule.every_seconds, pending.failures) + jitter(&pending.job, pending.attempts)
        };
        pending.due = now.saturating_add(delay);
        if !matches!(outcome, Outcome::Skipped { .. }) {
            self.families.insert(pending.job.family.clone(), pending.due);
            let resource = self.resources.entry(pending.job.resource.clone()).or_default();
            let resource_delay = match outcome {
                Outcome::Pressure => {
                    resource.pressure_failures = resource.pressure_failures.saturating_add(1);
                    failure_delay(RESOURCE_GAP.as_secs() as u32, resource.pressure_failures)
                }
                Outcome::More | Outcome::Complete => {
                    resource.pressure_failures = 0;
                    RESOURCE_GAP
                }
                _ => RESOURCE_GAP,
            };
            resource.due = now.saturating_add(resource_delay);
        }
        Ok(())
    }
    pub fn wait(&self, now: Duration) -> Duration {
        self.jobs
            .iter()
            .map(|pending| {
                pending
                    .due
                    .max(self.resources.get(&pending.job.resource).map_or(Duration::ZERO, |state| state.due))
                    .max(self.families.get(&pending.job.family).copied().unwrap_or_default())
            })
            .min()
            .unwrap_or(now.saturating_add(RESOURCE_GAP))
            .saturating_sub(now)
    }
}
fn urgency(left: &ScheduledJob, right: &ScheduledJob, now: Duration) -> std::cmp::Ordering {
    // Relative lateness serves frequent observations before infrequent catalog work without changing
    // source/resource admission. Cross multiplication keeps the ordering exact and deterministic.
    let left_lateness = now.saturating_sub(left.due).as_nanos() * u128::from(right.job.schedule.every_seconds);
    let right_lateness = now.saturating_sub(right.due).as_nanos() * u128::from(left.job.schedule.every_seconds);
    left_lateness.cmp(&right_lateness).then_with(|| right.turn.cmp(&left.turn))
}
fn jitter(job: &Job, attempt: u32) -> Duration {
    let hash = digest(format!("{}:{attempt}", job.key).as_bytes());
    let random = u64::from_str_radix(&hash[..16], 16).expect("SHA-256 is hexadecimal");
    Duration::from_secs(random % (u64::from(job.schedule.jitter_seconds) + 1))
}
fn failure_delay(period_seconds: u32, failures: u32) -> Duration {
    let period = Duration::from_secs(u64::from(period_seconds));
    let multiplier = 1u32 << failures.saturating_sub(1).min(10);
    period.saturating_mul(multiplier).min(MAX_FAILURE_DELAY.max(period))
}
pub(super) fn state_failed() -> culprit_core::Error {
    culprit_core::Error::internal(culprit_core::t!("fleet.state_failed"))
}

#[cfg(test)]
mod tests;
