//! Durable admission stores remaining monotonic delays, anchored to a bounded wall clock for restart only.
use super::*;
use chrono::{DateTime, Datelike, Utc};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

pub(in crate::fleet) const MAX_SCHEDULED_JOBS: usize = MAX_FLEET_TARGETS * MAX_TARGET_SCHEDULES * 2;
pub(in crate::fleet) const MAX_SCHEDULE_FAMILIES: usize = MAX_FLEET_TARGETS * MAX_TARGET_SCHEDULES;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(in crate::fleet) struct ScheduleClock {
    pub revision: u64,
    pub sequence: u64,
    pub wall: DateTime<Utc>,
    /// An uncompleted backend call has no established native end, regardless of elapsed wall time.
    pub pending_job: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(in crate::fleet) struct Delay {
    observed_at: DateTime<Utc>,
    remaining_ms: u64,
}
impl Delay {
    fn new(due: Duration, now: Duration, wall: DateTime<Utc>) -> Result<Self> {
        let remaining = due.saturating_sub(now);
        // Truncating a sub-millisecond delay would make the persisted admission earlier than the live one.
        let millis = remaining.as_millis() + u128::from(!remaining.subsec_nanos().is_multiple_of(1_000_000));
        Ok(Self { observed_at: wall, remaining_ms: millis.try_into().map_err(|_| state_failed())? })
    }
    fn restore(
        &self,
        clock: &ScheduleClock,
        now: Duration,
        wall: DateTime<Utc>,
        maximum: Duration,
        pending: bool,
    ) -> Result<Duration> {
        if !valid_wall(self.observed_at)
            || self.observed_at > clock.wall
            || Duration::from_millis(self.remaining_ms) > maximum
        {
            return Err(state_failed());
        }
        // A rollback cannot earn elapsed cooldown, even for a row written before the most recent admission.
        let elapsed = if !pending && wall >= clock.wall {
            (wall - self.observed_at).to_std().map_err(|_| state_failed())?
        } else {
            Duration::ZERO
        };
        now.checked_add(Duration::from_millis(self.remaining_ms).saturating_sub(elapsed)).ok_or_else(state_failed)
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(in crate::fleet) struct JobState {
    delay: Delay,
    attempts: u32,
    failures: u32,
    turn: u64,
}
impl JobState {
    fn new(job: &ScheduledJob, now: Duration, wall: DateTime<Utc>) -> Result<Self> {
        Ok(Self {
            delay: Delay::new(job.due, now, wall)?,
            attempts: job.attempts,
            failures: job.failures,
            turn: job.turn,
        })
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(in crate::fleet) struct ResourceState {
    delay: Delay,
    pressure_failures: u32,
}
impl ResourceState {
    fn new(resource: &ResourceAdmission, now: Duration, wall: DateTime<Utc>) -> Result<Self> {
        Ok(Self { delay: Delay::new(resource.due, now, wall)?, pressure_failures: resource.pressure_failures })
    }
}
pub(in crate::fleet) struct ScheduleState {
    pub clock: ScheduleClock,
    pub jobs: BTreeMap<String, JobState>,
    pub resources: BTreeMap<String, ResourceState>,
    pub families: BTreeMap<String, Delay>,
}
pub(in crate::fleet) struct ScheduleUpdate {
    pub clock: ScheduleClock,
    pub job: (String, JobState),
    pub resource: (String, ResourceState),
    pub family: (String, Delay),
}
#[derive(Default)]
struct ResourceLimit {
    native_delay: Duration,
    attempts: u64,
}
impl Scheduler {
    pub fn initial_state(&mut self, now: Duration, wall: DateTime<Utc>) -> Result<ScheduleState> {
        if self.clock.is_some() || self.active.is_some() {
            return Err(state_failed());
        }
        let clock = self.checkpoint_clock(wall, 0)?;
        Ok(ScheduleState {
            jobs: self
                .jobs
                .iter()
                .map(|pending| Ok((pending.job.key.clone(), JobState::new(pending, now, clock.wall)?)))
                .collect::<Result<_>>()?,
            resources: self
                .resources
                .iter()
                .map(|(key, resource)| Ok((key.clone(), ResourceState::new(resource, now, clock.wall)?)))
                .collect::<Result<_>>()?,
            families: self
                .families
                .iter()
                .map(|(key, due)| Ok((key.clone(), Delay::new(*due, now, clock.wall)?)))
                .collect::<Result<_>>()?,
            clock,
        })
    }
    pub fn restore(&mut self, state: ScheduleState, now: Duration, wall: DateTime<Utc>) -> Result<()> {
        if self.clock.is_some()
            || self.active.is_some()
            || !valid_wall(wall)
            || !valid_wall(state.clock.wall)
            || state.jobs.len() != self.jobs.len()
            || state.jobs.len() > MAX_SCHEDULED_JOBS
            || state.resources.len() > MAX_FLEET_TARGETS
            || state.families.len() > MAX_SCHEDULE_FAMILIES
        {
            return Err(state_failed());
        }
        let mut jobs = self.jobs.clone();
        let interrupted = state
            .clock
            .pending_job
            .as_ref()
            .map(|key| self.jobs.iter().position(|pending| pending.job.key == *key).ok_or_else(state_failed))
            .transpose()?;
        let interrupted_job = interrupted.map(|index| &self.jobs[index].job);
        if let Some(job) = interrupted_job {
            let saved = state.jobs.get(&job.key).ok_or_else(state_failed)?;
            if saved.attempts == 0 || saved.failures == 0 || saved.turn.checked_add(1) != Some(state.clock.sequence) {
                return Err(state_failed());
            }
        }
        let mut turns = BTreeSet::new();
        let mut resource_limits: BTreeMap<String, ResourceLimit> = BTreeMap::new();
        let mut family_limits: BTreeMap<String, Duration> = BTreeMap::new();
        for pending in &mut jobs {
            let saved = state.jobs.get(&pending.job.key).ok_or_else(state_failed)?;
            if saved.failures > saved.attempts || saved.turn >= state.clock.sequence || !turns.insert(saved.turn) {
                return Err(state_failed());
            }
            pending.due = saved.delay.restore(
                &state.clock,
                now,
                wall,
                maximum_job_delay(&pending.job),
                interrupted_job.is_some_and(|job| job.key == pending.job.key),
            )?;
            pending.attempts = saved.attempts;
            pending.failures = saved.failures;
            pending.turn = saved.turn;
            let resource = resource_limits.entry(pending.job.resource.clone()).or_default();
            resource.native_delay = resource.native_delay.max(native_reservation(&pending.job));
            resource.attempts += u64::from(pending.attempts);
            let family = family_limits.entry(pending.job.family.clone()).or_default();
            *family = (*family).max(maximum_job_delay(&pending.job));
        }
        if state.resources.keys().ne(resource_limits.keys()) || state.families.keys().ne(family_limits.keys()) {
            return Err(state_failed());
        }
        let mut resources = BTreeMap::new();
        for (key, saved) in state.resources {
            let limit = resource_limits.get(&key).ok_or_else(state_failed)?;
            if u64::from(saved.pressure_failures) > limit.attempts {
                return Err(state_failed());
            }
            let pending = interrupted_job.is_some_and(|job| job.resource == key);
            resources.insert(
                key,
                ResourceAdmission {
                    due: saved.delay.restore(
                        &state.clock,
                        now,
                        wall,
                        limit.native_delay + MAX_FAILURE_DELAY,
                        pending,
                    )?,
                    pressure_failures: saved.pressure_failures,
                },
            );
        }
        let mut families = BTreeMap::new();
        for (key, saved) in state.families {
            let maximum = family_limits.get(&key).copied().ok_or_else(state_failed)?;
            let pending = interrupted_job.is_some_and(|job| job.family == key);
            families.insert(key, saved.restore(&state.clock, now, wall, maximum, pending)?);
        }
        self.jobs = jobs;
        self.resources = resources;
        self.families = families;
        self.sequence = state.clock.sequence;
        self.clock = Some(state.clock);
        self.completed = interrupted;
        Ok(())
    }
    pub fn admission_checkpoint(&self, now: Duration, wall: DateTime<Utc>) -> Result<ScheduleUpdate> {
        let index = self.active.ok_or_else(state_failed)?;
        let completion_at = now.checked_add(native_reservation(&self.jobs[index].job)).ok_or_else(state_failed)?;
        let completion = self.completion(index, completion_at, Outcome::Failed);
        let mut update =
            self.update(&completion.job, completion.resource.as_ref(), completion.family_due, now, wall)?;
        update.clock.pending_job = Some(completion.job.job.key);
        Ok(update)
    }
    /// Rebase the one interrupted reservation durably before another admission can replace its marker.
    pub fn resumed_admission_checkpoint(&self, now: Duration, wall: DateTime<Utc>) -> Result<Option<ScheduleUpdate>> {
        match &self.clock.as_ref().ok_or_else(state_failed)?.pending_job {
            Some(_) => self.completion_checkpoint(now, wall).map(Some),
            None => Ok(None),
        }
    }
    pub fn completion_checkpoint(&self, now: Duration, wall: DateTime<Utc>) -> Result<ScheduleUpdate> {
        if self.active.is_some() {
            return Err(state_failed());
        }
        let pending = &self.jobs[self.completed.ok_or_else(state_failed)?];
        self.update(
            pending,
            self.resources.get(&pending.job.resource),
            self.families.get(&pending.job.family).copied(),
            now,
            wall,
        )
    }
    fn update(
        &self,
        pending: &ScheduledJob,
        resource: Option<&ResourceAdmission>,
        family: Option<Duration>,
        now: Duration,
        wall: DateTime<Utc>,
    ) -> Result<ScheduleUpdate> {
        let previous = self.clock.as_ref().ok_or_else(state_failed)?;
        let clock = self.checkpoint_clock(wall, previous.revision.checked_add(1).ok_or_else(state_failed)?)?;
        Ok(ScheduleUpdate {
            job: (pending.job.key.clone(), JobState::new(pending, now, clock.wall)?),
            resource: (
                pending.job.resource.clone(),
                ResourceState::new(resource.ok_or_else(state_failed)?, now, clock.wall)?,
            ),
            family: (pending.job.family.clone(), Delay::new(family.ok_or_else(state_failed)?, now, clock.wall)?),
            clock,
        })
    }
    fn checkpoint_clock(&self, wall: DateTime<Utc>, revision: u64) -> Result<ScheduleClock> {
        if !valid_wall(wall) {
            return Err(state_failed());
        }
        Ok(ScheduleClock {
            revision,
            sequence: self.sequence,
            wall: self.clock.as_ref().map_or(wall, |previous| wall.max(previous.wall)),
            pending_job: None,
        })
    }
    pub fn persisted(&mut self, clock: ScheduleClock) -> Result<()> {
        let wanted = self.clock.as_ref().map_or(Some(0), |previous| previous.revision.checked_add(1));
        if wanted != Some(clock.revision) || clock.sequence != self.sequence || !valid_wall(clock.wall) {
            return Err(state_failed());
        }
        if clock.pending_job.as_ref() != self.active.map(|index| &self.jobs[index].job.key) {
            return Err(state_failed());
        }
        self.clock = Some(clock);
        Ok(())
    }
}
fn valid_wall(wall: DateTime<Utc>) -> bool {
    (1970..=9999).contains(&wall.year())
}
fn native_reservation(job: &Job) -> Duration {
    ADMISSION_START_WINDOW
        + Duration::from_millis(u64::from(job.schedule.budget.total_timeout_ms))
        + crate::capture::SOURCE_SHUTDOWN_TIMEOUT
}
fn maximum_job_delay(job: &Job) -> Duration {
    native_reservation(job)
        + failure_delay(job.schedule.every_seconds, u32::MAX)
        + Duration::from_secs(u64::from(job.schedule.jitter_seconds))
}
