//! Fair, serial source admission. A completed read schedules future work from completion, never from missed ticks.
use culprit_core::{Result, telemetry::*};
use std::{collections::BTreeMap, time::Duration};
mod checkpoint;
mod focus;
mod restore;
use checkpoint::native_reservation;
pub(super) use checkpoint::{
    Delay, FocusFamilyMutation, FocusFamilyState, FocusMutation, FocusState, JobState, MAX_SCHEDULE_FAMILIES,
    MAX_SCHEDULED_JOBS, ResourceState, ScheduleClock, ScheduleState, ScheduleUpdate,
};
pub(super) use focus::{
    FocusEnrollment, FocusStatus, FocusedJob, MAX_FOCUSED_FAMILIES, MAX_FOCUSED_JOBS, Work, WorkKey,
};
use focus::{FocusFamilyAdmission, FocusedAdmission, focus_family_key, identifier, valid_cooldown};

const RESOURCE_GAP: Duration = Duration::from_secs(60);
const MAX_FAILURE_DELAY: Duration = Duration::from_secs(900);
/// A slow durability write must not let native work start after its saved crash reservation has aged out.
pub(super) use crate::capture::SOURCE_ADMISSION_START_WINDOW as ADMISSION_START_WINDOW;

#[derive(Clone)]
pub(super) struct Job {
    pub target: String,
    pub connection: String,
    pub subject: CollectionSubject,
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
#[derive(Clone)]
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
    FocusedComplete { result: DiagnosticCaptureRef },
    FocusedRetry { reason: FocusRetry, attempt: Option<CaptureIdentity>, source_work: SourceWorkState },
}
#[derive(Clone, Copy)]
pub(super) enum FocusRetry {
    More,
    Failed,
    Pressure,
}
#[derive(Clone, Copy)]
enum OutcomeKind {
    Complete,
    More,
    Failed,
    Pressure,
    Skipped { more: bool },
}
impl Outcome {
    fn kind(&self) -> OutcomeKind {
        match self {
            Self::Complete | Self::FocusedComplete { .. } => OutcomeKind::Complete,
            Self::More | Self::FocusedRetry { reason: FocusRetry::More, .. } => OutcomeKind::More,
            Self::Failed | Self::FocusedRetry { reason: FocusRetry::Failed, .. } => OutcomeKind::Failed,
            Self::Pressure | Self::FocusedRetry { reason: FocusRetry::Pressure, .. } => OutcomeKind::Pressure,
            Self::Skipped { more } => OutcomeKind::Skipped { more: *more },
        }
    }
}
#[derive(Clone, Default)]
struct ResourceAdmission {
    due: Duration,
    pressure_failures: u32,
    attempts: u64,
    max_reservation: Duration,
}
pub(super) struct Scheduler {
    jobs: Vec<ScheduledJob>,
    routes: BTreeMap<String, Route>,
    focused: BTreeMap<String, FocusedAdmission>,
    focused_families: BTreeMap<String, FocusFamilyAdmission>,
    resources: BTreeMap<String, ResourceAdmission>,
    families: BTreeMap<String, Duration>,
    sequence: u64,
    active: Option<WorkKey>,
    completed: Option<WorkKey>,
    clock: Option<ScheduleClock>,
}
struct Completion {
    job: ScheduledJob,
    resource: ResourceAdmission,
    family_due: Option<Duration>,
}
#[derive(Clone)]
struct Route {
    connection: String,
    subject: CollectionSubject,
    resource: String,
}
impl Scheduler {
    pub fn new(inventory: &FleetInventory, shard: &str) -> Result<Self> {
        inventory.validate(shard)?;
        let targets: Vec<_> = inventory.targets.iter().filter(|target| target.shard == shard).collect();
        let routes = targets
            .iter()
            .map(|target| {
                (
                    target.id.clone(),
                    Route {
                        connection: target.connection.clone(),
                        subject: target.subject.clone(),
                        resource: target.admission_group.clone(),
                    },
                )
            })
            .collect();
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
                            &serde_json::to_vec(&(&target.id, &schedule.id, *lane == HistoryLane::Backfill))
                                .map_err(|_| state_failed())?,
                        );
                        let job = Job {
                            target: target.id.clone(),
                            connection: target.connection.clone(),
                            subject: target.subject.clone(),
                            resource: target.admission_group.clone(),
                            schedule: schedule.clone(),
                            key,
                            lane: *lane,
                            family: digest(
                                &serde_json::to_vec(&(&target.id, &schedule.id)).map_err(|_| state_failed())?,
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
        let resources =
            jobs.iter().map(|pending| (pending.job.resource.clone(), ResourceAdmission::default())).collect();
        let families = jobs.iter().map(|pending| (pending.job.family.clone(), Duration::ZERO)).collect();
        Ok(Self {
            jobs,
            routes,
            focused: BTreeMap::new(),
            focused_families: BTreeMap::new(),
            resources,
            families,
            sequence,
            active: None,
            completed: None,
            clock: None,
        })
    }
    #[cfg(test)]
    pub fn take(&mut self, now: Duration) -> Result<Option<Work>> {
        self.take_allowed(now, |_| true)
    }
    pub fn take_allowed(&mut self, now: Duration, allowed: impl Fn(&FocusedJob) -> bool) -> Result<Option<Work>> {
        if self.active.is_some()
            || self.clock.as_ref().is_some_and(|clock| clock.pending_job.is_some() || clock.sequence != self.sequence)
        {
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
        let next_sequence = self.sequence.checked_add(1).ok_or_else(state_failed)?;
        let work = if let Some(index) = selected {
            self.jobs[index].turn = self.sequence;
            Work::Scheduled(self.jobs[index].job.clone())
        } else {
            let deadlines = self.periodic_deadlines();
            let selected = self
                .focused
                .iter()
                .filter(|(_, pending)| {
                    pending.status == FocusStatus::Pending
                        && allowed(&pending.job)
                        && self.focus_ready_at(pending) <= now
                        && deadlines.permits(&pending.job, now)
                })
                .min_by(|(left_key, left), (right_key, right)| {
                    left.turn.cmp(&right.turn).then_with(|| left_key.cmp(right_key))
                })
                .map(|(key, _)| key.clone());
            let Some(key) = selected else { return Ok(None) };
            let pending = self.focused.get_mut(&key).ok_or_else(state_failed)?;
            pending.turn = self.sequence;
            pending.status = FocusStatus::Admitted;
            Work::Focused(pending.job.clone())
        };
        self.sequence = next_sequence;
        self.active = Some(work.key());
        Ok(Some(work))
    }
    pub fn finish(&mut self, now: Duration, outcome: Outcome) -> Result<()> {
        let active = self.active.clone().ok_or_else(state_failed)?;
        match &active {
            WorkKey::Scheduled { key } => {
                if matches!(outcome, Outcome::FocusedComplete { .. } | Outcome::FocusedRetry { .. }) {
                    return Err(state_failed());
                }
                let index = self.static_index(key)?;
                let completion = self.completion(index, now, &outcome)?;
                self.resources.insert(completion.job.job.resource.clone(), completion.resource);
                if let Some(due) = completion.family_due {
                    self.families.insert(completion.job.job.family.clone(), due);
                }
                self.jobs[index] = completion.job;
            }
            WorkKey::Focused { key } => self.finish_focus(key, now, &outcome)?,
        }
        self.active = None;
        self.completed = Some(active);
        Ok(())
    }
    fn completion(&self, index: usize, now: Duration, outcome: &Outcome) -> Result<Completion> {
        let mut pending = self.jobs[index].clone();
        pending.attempts = pending.attempts.checked_add(1).ok_or_else(state_failed)?;
        pending.failures = if matches!(outcome.kind(), OutcomeKind::Failed | OutcomeKind::Pressure) {
            pending.failures.saturating_add(1)
        } else {
            0
        };
        let delay = if matches!(outcome.kind(), OutcomeKind::Skipped { more: true }) {
            Duration::ZERO
        } else {
            failure_delay(pending.job.schedule.every_seconds, pending.failures) + jitter(&pending.job, pending.attempts)
        };
        pending.due = now.saturating_add(delay);
        let resource = self.resource_completion(
            &pending.job.resource,
            now,
            outcome.kind(),
            native_reservation(&pending.job.schedule.budget),
        )?;
        let family_due = (!matches!(outcome.kind(), OutcomeKind::Skipped { .. })).then_some(pending.due);
        Ok(Completion { job: pending, resource, family_due })
    }
    #[cfg(test)]
    pub fn wait(&self, now: Duration) -> Duration {
        self.wait_allowed(now, |_| true)
    }
    pub fn wait_allowed(&self, now: Duration, allowed: impl Fn(&FocusedJob) -> bool) -> Duration {
        let periodic = self
            .jobs
            .iter()
            .map(|pending| self.periodic_ready_at(pending))
            .min()
            .unwrap_or(now.saturating_add(RESOURCE_GAP));
        let deadlines = self.periodic_deadlines();
        self.focused
            .values()
            .filter(|pending| pending.status == FocusStatus::Pending && allowed(&pending.job))
            .filter_map(|pending| {
                let at = self.focus_ready_at(pending).max(now);
                deadlines.permits(&pending.job, at).then_some(at)
            })
            .fold(periodic, Duration::min)
            .saturating_sub(now)
    }
    fn static_index(&self, key: &str) -> Result<usize> {
        self.jobs.iter().position(|pending| pending.job.key == key).ok_or_else(state_failed)
    }
    fn work(&self, key: &WorkKey) -> Result<Work> {
        match key {
            WorkKey::Scheduled { key } => Ok(Work::Scheduled(self.jobs[self.static_index(key)?].job.clone())),
            WorkKey::Focused { key } => {
                self.focused.get(key).map(|pending| Work::Focused(pending.job.clone())).ok_or_else(state_failed)
            }
        }
    }
    fn resource_completion(
        &self,
        key: &str,
        now: Duration,
        outcome: OutcomeKind,
        reservation: Duration,
    ) -> Result<ResourceAdmission> {
        let mut resource = self.resources.get(key).cloned().ok_or_else(state_failed)?;
        resource.attempts = resource.attempts.checked_add(1).ok_or_else(state_failed)?;
        resource.max_reservation = resource.max_reservation.max(reservation);
        if !matches!(outcome, OutcomeKind::Skipped { .. }) {
            let delay = match outcome {
                OutcomeKind::Pressure => {
                    resource.pressure_failures = resource.pressure_failures.saturating_add(1);
                    failure_delay(RESOURCE_GAP.as_secs() as u32, resource.pressure_failures)
                }
                OutcomeKind::More | OutcomeKind::Complete => {
                    resource.pressure_failures = 0;
                    RESOURCE_GAP
                }
                _ => RESOURCE_GAP,
            };
            resource.due = now.checked_add(delay).ok_or_else(state_failed)?;
        }
        Ok(resource)
    }
    fn periodic_ready_at(&self, pending: &ScheduledJob) -> Duration {
        pending
            .due
            .max(self.resources.get(&pending.job.resource).map_or(Duration::ZERO, |state| state.due))
            .max(self.families.get(&pending.job.family).copied().unwrap_or_default())
    }
    fn periodic_deadlines(&self) -> PeriodicDeadlines {
        let mut deadlines = PeriodicDeadlines::default();
        for pending in &self.jobs {
            let due = self.periodic_ready_at(pending);
            deadlines.global = Some(deadlines.global.map_or(due, |previous| previous.min(due)));
            deadlines
                .resources
                .entry(pending.job.resource.clone())
                .and_modify(|previous| *previous = (*previous).min(due))
                .or_insert(due);
        }
        deadlines
    }
}

#[derive(Default)]
struct PeriodicDeadlines {
    global: Option<Duration>,
    resources: BTreeMap<String, Duration>,
}
impl PeriodicDeadlines {
    fn permits(&self, job: &FocusedJob, start: Duration) -> bool {
        let Some(end) = start.checked_add(native_reservation(&job.budget)) else { return false };
        if self.global.is_some_and(|due| end > due) {
            return false;
        }
        self.resources
            .get(&job.resource)
            .is_none_or(|due| end.checked_add(RESOURCE_GAP).is_some_and(|ready| ready <= *due))
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
mod focused_tests;
#[cfg(test)]
mod tests;
