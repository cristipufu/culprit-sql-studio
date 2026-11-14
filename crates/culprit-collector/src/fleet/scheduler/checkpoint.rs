//! Durable admission stores remaining monotonic delays, anchored to a bounded wall clock for restart only.
use super::*;
use chrono::{DateTime, Datelike, Utc};
use serde::{Deserialize, Serialize};

pub(in crate::fleet) const MAX_SCHEDULED_JOBS: usize = MAX_FLEET_TARGETS * MAX_TARGET_SCHEDULES * 2;
pub(in crate::fleet) const MAX_SCHEDULE_FAMILIES: usize = MAX_FLEET_TARGETS * MAX_TARGET_SCHEDULES;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(in crate::fleet) struct ScheduleClock {
    pub revision: u64,
    pub sequence: u64,
    pub wall: DateTime<Utc>,
    /// An uncompleted backend call has no established native end, regardless of elapsed wall time.
    pub pending_job: Option<WorkKey>,
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
    pub(super) fn restore(
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
    pub(super) delay: Delay,
    pub(super) attempts: u32,
    pub(super) failures: u32,
    pub(super) turn: u64,
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
    pub(super) delay: Delay,
    pub(super) pressure_failures: u32,
    pub(super) attempts: u64,
    pub(super) max_reservation_ms: u64,
}
impl ResourceState {
    fn new(resource: &ResourceAdmission, now: Duration, wall: DateTime<Utc>) -> Result<Self> {
        Ok(Self {
            delay: Delay::new(resource.due, now, wall)?,
            pressure_failures: resource.pressure_failures,
            attempts: resource.attempts,
            max_reservation_ms: resource.max_reservation.as_millis().try_into().map_err(|_| state_failed())?,
        })
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(in crate::fleet) struct FocusState {
    pub job: FocusedJob,
    pub delay: Delay,
    pub attempts: u32,
    pub failures: u32,
    pub turn: u64,
    pub status: FocusStatus,
    pub last_attempt: Option<CaptureIdentity>,
    pub source_work: SourceWorkState,
}
impl FocusState {
    pub fn is_valid(&self) -> bool {
        self.job.is_valid()
            && self.failures <= self.attempts
            && valid_wall(self.delay.observed_at)
            && self.last_attempt.as_ref().is_none_or(valid_attempt)
            && (self.attempts > 0
                || (self.status == FocusStatus::Pending
                    && self.last_attempt.is_none()
                    && self.source_work == SourceWorkState::NotStarted))
            && match &self.status {
                FocusStatus::Pending => self.source_work != SourceWorkState::Active,
                FocusStatus::Admitted => {
                    self.attempts > 0 && self.failures > 0 && self.source_work == SourceWorkState::Active
                }
                FocusStatus::Completed { result } => {
                    result.is_valid()
                        && self.attempts > 0
                        && self.failures == 0
                        && self.last_attempt.as_ref() == Some(&result.capture)
                        && self.source_work == SourceWorkState::Quiescent
                }
            }
    }
    pub(super) fn new(focus: &FocusedAdmission, now: Duration, wall: DateTime<Utc>) -> Result<Self> {
        let state = Self {
            job: focus.job.clone(),
            delay: Delay::new(focus.due, now, wall)?,
            attempts: focus.attempts,
            failures: focus.failures,
            turn: focus.turn,
            status: focus.status.clone(),
            last_attempt: focus.last_attempt.clone(),
            source_work: focus.source_work,
        };
        if !state.is_valid() {
            return Err(state_failed());
        }
        Ok(state)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(in crate::fleet) struct FocusFamilyState {
    pub target: String,
    pub recipe_id: String,
    pub cooldown_seconds: u32,
    pub delay: Delay,
}
impl FocusFamilyState {
    pub fn key(&self) -> Result<String> {
        if !self.is_valid() {
            return Err(state_failed());
        }
        focus_family_key(&self.target, &self.recipe_id)
    }
    pub fn is_valid(&self) -> bool {
        identifier(&self.target)
            && identifier(&self.recipe_id)
            && valid_cooldown(self.cooldown_seconds)
            && valid_wall(self.delay.observed_at)
    }
    pub(super) fn new(family: &FocusFamilyAdmission, now: Duration, wall: DateTime<Utc>) -> Result<Self> {
        let state = Self {
            target: family.target.clone(),
            recipe_id: family.recipe_id.clone(),
            cooldown_seconds: family.cooldown_seconds,
            delay: Delay::new(family.due, now, wall)?,
        };
        if !state.is_valid() {
            return Err(state_failed());
        }
        Ok(state)
    }
}

pub(in crate::fleet) enum FocusMutation {
    Insert { key: String, state: FocusState },
    Update { key: String, state: FocusState },
    Remove { key: String },
}
pub(in crate::fleet) enum FocusFamilyMutation {
    Upsert { key: String, state: FocusFamilyState },
    Remove { key: String },
}

#[derive(Clone)]
pub(in crate::fleet) struct ScheduleState {
    pub clock: ScheduleClock,
    pub jobs: BTreeMap<String, JobState>,
    pub resources: BTreeMap<String, ResourceState>,
    pub families: BTreeMap<String, Delay>,
    pub focused: BTreeMap<String, FocusState>,
    pub focused_families: BTreeMap<String, FocusFamilyState>,
}
pub(in crate::fleet) struct ScheduleUpdate {
    pub clock: ScheduleClock,
    pub job: Option<(String, JobState)>,
    pub resource: (String, ResourceState),
    pub family: Option<(String, Delay)>,
    pub focused: Option<FocusMutation>,
    pub focused_family: Option<FocusFamilyMutation>,
}

pub(super) fn valid_attempt(capture: &CaptureIdentity) -> bool {
    identifier(&capture.observation_id)
        && capture.revision.as_ref().is_some_and(|revision| {
            identifier(&revision.outbox_id)
                && [&revision.owner_epoch, &revision.sequence]
                    .into_iter()
                    .all(|value| value.parse::<u64>().is_ok_and(|parsed| parsed > 0 && parsed.to_string() == *value))
        })
}
impl Scheduler {
    pub fn initial_state(&mut self, now: Duration, wall: DateTime<Utc>) -> Result<ScheduleState> {
        if self.clock.is_some()
            || self.active.is_some()
            || !self.focused.is_empty()
            || !self.focused_families.is_empty()
        {
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
            focused: BTreeMap::new(),
            focused_families: BTreeMap::new(),
            clock,
        })
    }
    pub fn admission_checkpoint(&self, now: Duration, wall: DateTime<Utc>) -> Result<ScheduleUpdate> {
        let active = self.active.as_ref().ok_or_else(state_failed)?;
        let completion_at = now.checked_add(self.work(active)?.reservation()).ok_or_else(state_failed)?;
        let mut update = match active {
            WorkKey::Scheduled { key } => {
                let completion = self.completion(self.static_index(key)?, completion_at, &Outcome::Failed)?;
                self.static_update(&completion.job, &completion.resource, completion.family_due, now, wall)?
            }
            WorkKey::Focused { key } => {
                let mut completion = self.focus_completion(key, completion_at, &Outcome::Failed)?;
                completion.pending.status = FocusStatus::Admitted;
                completion.pending.source_work = SourceWorkState::Active;
                self.focus_update(&completion.pending, &completion.family, &completion.resource, now, wall)?
            }
        };
        update.clock.pending_job = Some(active.clone());
        Ok(update)
    }
    /// Rebase the interrupted reservation durably before another admission can replace its marker.
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
        match self.completed.as_ref().ok_or_else(state_failed)? {
            WorkKey::Scheduled { key } => {
                let pending = &self.jobs[self.static_index(key)?];
                self.static_update(
                    pending,
                    self.resources.get(&pending.job.resource).ok_or_else(state_failed)?,
                    self.families.get(&pending.job.family).copied(),
                    now,
                    wall,
                )
            }
            WorkKey::Focused { key } => {
                let pending = self.focused.get(key).ok_or_else(state_failed)?;
                self.focus_update(
                    pending,
                    self.focused_families.get(&pending.job.family).ok_or_else(state_failed)?,
                    self.resources.get(&pending.job.resource).ok_or_else(state_failed)?,
                    now,
                    wall,
                )
            }
        }
    }
    fn static_update(
        &self,
        pending: &ScheduledJob,
        resource: &ResourceAdmission,
        family_due: Option<Duration>,
        now: Duration,
        wall: DateTime<Utc>,
    ) -> Result<ScheduleUpdate> {
        let mut update = self.metadata_update(&pending.job.resource, now, wall)?;
        update.job = Some((pending.job.key.clone(), JobState::new(pending, now, update.clock.wall)?));
        update.resource.1 = ResourceState::new(resource, now, update.clock.wall)?;
        let due = family_due.or_else(|| self.families.get(&pending.job.family).copied()).ok_or_else(state_failed)?;
        update.family = Some((pending.job.family.clone(), Delay::new(due, now, update.clock.wall)?));
        Ok(update)
    }
    pub(super) fn focus_update(
        &self,
        pending: &FocusedAdmission,
        family: &FocusFamilyAdmission,
        resource: &ResourceAdmission,
        now: Duration,
        wall: DateTime<Utc>,
    ) -> Result<ScheduleUpdate> {
        let mut update = self.metadata_update(&pending.job.resource, now, wall)?;
        update.resource.1 = ResourceState::new(resource, now, update.clock.wall)?;
        update.focused = Some(FocusMutation::Update {
            key: pending.job.key.clone(),
            state: FocusState::new(pending, now, update.clock.wall)?,
        });
        update.focused_family = Some(FocusFamilyMutation::Upsert {
            key: pending.job.family.clone(),
            state: FocusFamilyState::new(family, now, update.clock.wall)?,
        });
        Ok(update)
    }
    pub(super) fn metadata_update(&self, resource: &str, now: Duration, wall: DateTime<Utc>) -> Result<ScheduleUpdate> {
        let previous = self.clock.as_ref().ok_or_else(state_failed)?;
        let clock = self.checkpoint_clock(wall, previous.revision.checked_add(1).ok_or_else(state_failed)?)?;
        Ok(ScheduleUpdate {
            job: None,
            resource: (
                resource.into(),
                ResourceState::new(self.resources.get(resource).ok_or_else(state_failed)?, now, clock.wall)?,
            ),
            family: None,
            focused: None,
            focused_family: None,
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
        if wanted != Some(clock.revision)
            || clock.sequence != self.sequence
            || !valid_wall(clock.wall)
            || self.clock.as_ref().is_some_and(|previous| clock.wall < previous.wall)
            || clock.pending_job.as_ref() != self.active.as_ref()
        {
            return Err(state_failed());
        }
        self.clock = Some(clock);
        Ok(())
    }
}
pub(super) fn valid_wall(wall: DateTime<Utc>) -> bool {
    (1970..=9999).contains(&wall.year())
}
pub(super) fn native_reservation(budget: &CollectionBudget) -> Duration {
    ADMISSION_START_WINDOW
        + Duration::from_millis(u64::from(budget.total_timeout_ms))
        + crate::capture::SOURCE_SHUTDOWN_TIMEOUT
}
pub(super) fn maximum_job_delay(job: &Job) -> Duration {
    native_reservation(&job.schedule.budget)
        + failure_delay(job.schedule.every_seconds, u32::MAX)
        + Duration::from_secs(u64::from(job.schedule.jitter_seconds))
}
pub(super) fn maximum_focus_delay(cooldown_seconds: u32) -> Duration {
    native_reservation(&CollectionBudget::upper_limits()) + failure_delay(cooldown_seconds, u32::MAX)
}
