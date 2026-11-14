//! Compact scheduler metadata references one immutable, independently validated diagnostic intent.
use super::*;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

pub(in crate::fleet) const MAX_FOCUSED_JOBS: usize = 256;
pub(in crate::fleet) const MAX_FOCUSED_FAMILIES: usize = 1024;
pub(in crate::fleet) const MAX_FOCUS_COOLDOWN_SECONDS: u32 = 86_400;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(in crate::fleet) struct FocusedJob {
    pub key: String,
    pub episode_id: String,
    pub episode_stage_key: String,
    pub recipe_id: String,
    pub recipe_version: u32,
    pub target: String,
    pub connection: String,
    pub subject: CollectionSubject,
    pub resource: String,
    pub family: String,
    pub budget: CollectionBudget,
    pub cooldown_seconds: u32,
}
impl FocusedJob {
    pub fn new(intent: &DiagnosticInvestigationIntent, target: &FleetTarget, cooldown_seconds: u32) -> Result<Self> {
        if !intent.is_valid() || !target.subject.accepts(&intent.focus.target) {
            return Err(state_failed());
        }
        let job = Self {
            key: intent.key()?,
            episode_id: intent.episode_id.clone(),
            episode_stage_key: intent.episode_stage_key()?,
            recipe_id: intent.recipe_id.clone(),
            recipe_version: intent.recipe_version,
            target: target.id.clone(),
            connection: target.connection.clone(),
            subject: target.subject.clone(),
            resource: target.admission_group.clone(),
            family: focus_family_key(&target.id, &intent.recipe_id)?,
            budget: intent.budget.clone(),
            cooldown_seconds,
        };
        if !job.is_valid() {
            return Err(state_failed());
        }
        Ok(job)
    }
    pub fn is_valid(&self) -> bool {
        valid_digest(&self.key)
            && valid_digest(&self.episode_id)
            && valid_digest(&self.episode_stage_key)
            && identifier(&self.target)
            && identifier(&self.connection)
            && identifier(&self.resource)
            && identifier(&self.recipe_id)
            && self.recipe_version > 0
            && self.subject.validate().is_ok()
            && self.budget.validate().is_ok()
            && valid_cooldown(self.cooldown_seconds)
            && focus_family_key(&self.target, &self.recipe_id).is_ok_and(|key| key == self.family)
    }
    pub fn matches_intent(&self, intent: &DiagnosticInvestigationIntent) -> bool {
        self.is_valid()
            && intent.is_valid()
            && intent.key().is_ok_and(|key| key == self.key)
            && intent.episode_stage_key().is_ok_and(|key| key == self.episode_stage_key)
            && self.episode_id == intent.episode_id
            && self.recipe_id == intent.recipe_id
            && self.recipe_version == intent.recipe_version
            && self.budget == intent.budget
            && self.subject.accepts(&intent.focus.target)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
pub(in crate::fleet) enum WorkKey {
    Scheduled { key: String },
    Focused { key: String },
}

#[derive(Clone)]
pub(in crate::fleet) enum Work {
    Scheduled(Job),
    Focused(FocusedJob),
}
impl Work {
    pub fn key(&self) -> WorkKey {
        match self {
            Self::Scheduled(job) => WorkKey::Scheduled { key: job.key.clone() },
            Self::Focused(job) => WorkKey::Focused { key: job.key.clone() },
        }
    }
    pub(super) fn resource(&self) -> &str {
        match self {
            Self::Scheduled(job) => &job.resource,
            Self::Focused(job) => &job.resource,
        }
    }
    pub(super) fn reservation(&self) -> Duration {
        let budget = match self {
            Self::Scheduled(job) => &job.schedule.budget,
            Self::Focused(job) => &job.budget,
        };
        native_reservation(budget)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
pub(in crate::fleet) enum FocusStatus {
    Pending,
    Admitted,
    Completed { result: DiagnosticCaptureRef },
}

#[derive(Clone)]
pub(super) struct FocusedAdmission {
    pub job: FocusedJob,
    pub due: Duration,
    pub attempts: u32,
    pub failures: u32,
    pub turn: u64,
    pub status: FocusStatus,
    pub last_attempt: Option<CaptureIdentity>,
    pub source_work: SourceWorkState,
}

#[derive(Clone)]
pub(super) struct FocusFamilyAdmission {
    pub target: String,
    pub recipe_id: String,
    pub cooldown_seconds: u32,
    pub due: Duration,
}

pub(in crate::fleet) enum FocusEnrollment {
    Existing { key: String },
    Capacity,
    Enqueued(Box<ScheduleUpdate>),
}

pub(in crate::fleet) struct FocusedRuntimeState {
    pub job: FocusedJob,
    pub status: FocusStatus,
    pub source_work: SourceWorkState,
}

pub(super) struct FocusCompletion {
    pub pending: FocusedAdmission,
    pub resource: ResourceAdmission,
    pub family: FocusFamilyAdmission,
}

impl Scheduler {
    pub fn focused_state(&self, key: &str) -> Option<FocusedRuntimeState> {
        self.focused.get(key).map(|pending| FocusedRuntimeState {
            job: pending.job.clone(),
            status: pending.status.clone(),
            source_work: if pending.status == FocusStatus::Admitted {
                SourceWorkState::Active
            } else {
                pending.source_work
            },
        })
    }
    pub fn focused_keys(&self) -> Vec<String> {
        self.focused.keys().cloned().collect()
    }
    pub fn focused_family_keys(&self) -> Vec<String> {
        self.focused_families.keys().cloned().collect()
    }

    /// Enrollment changes only admission metadata. The caller atomically stores the canonical intent with this update.
    pub fn enqueue_focus(&mut self, job: FocusedJob, now: Duration, wall: DateTime<Utc>) -> Result<FocusEnrollment> {
        self.require_idle()?;
        if !job.is_valid() || !self.matches_route(&job) {
            return Err(state_failed());
        }
        if let Some(existing) = self.focused.get(&job.key) {
            return if existing.job == job {
                Ok(FocusEnrollment::Existing { key: job.key })
            } else {
                Err(state_failed())
            };
        }
        if let Some(existing) = self.focused.values().find(|pending| {
            pending.job.episode_id == job.episode_id || pending.job.episode_stage_key == job.episode_stage_key
        }) {
            return Ok(FocusEnrollment::Existing { key: existing.job.key.clone() });
        }
        if self.focused.len() >= MAX_FOCUSED_JOBS
            || (!self.focused_families.contains_key(&job.family) && self.focused_families.len() >= MAX_FOCUSED_FAMILIES)
        {
            return Ok(FocusEnrollment::Capacity);
        }
        let next_sequence = self.sequence.checked_add(1).ok_or_else(state_failed)?;
        let mut family = self.focused_families.get(&job.family).cloned().unwrap_or_else(|| FocusFamilyAdmission {
            target: job.target.clone(),
            recipe_id: job.recipe_id.clone(),
            cooldown_seconds: job.cooldown_seconds,
            due: now,
        });
        family.cooldown_seconds = family.cooldown_seconds.max(job.cooldown_seconds);
        let pending = FocusedAdmission {
            job,
            due: now,
            attempts: 0,
            failures: 0,
            turn: self.sequence,
            status: FocusStatus::Pending,
            last_attempt: None,
            source_work: SourceWorkState::NotStarted,
        };
        // Build before mutating; an invalid wall clock or overflowing revision cannot leave half an enrollment.
        let mut update = self.focus_update(&pending, &family, self.resource_for(&pending.job.resource)?, now, wall)?;
        update.clock.sequence = next_sequence;
        update.focused = Some(FocusMutation::Insert {
            key: pending.job.key.clone(),
            state: FocusState::new(&pending, now, update.clock.wall)?,
        });
        self.sequence = next_sequence;
        self.focused_families.insert(pending.job.family.clone(), family);
        self.focused.insert(pending.job.key.clone(), pending);
        Ok(FocusEnrollment::Enqueued(Box::new(update)))
    }

    /// The caller proves this stage is no longer needed; its source owner must already have settled native work.
    pub fn cancel_focused(&mut self, key: &str, now: Duration, wall: DateTime<Utc>) -> Result<ScheduleUpdate> {
        self.require_idle()?;
        let pending = self.focused.get(key).ok_or_else(state_failed)?;
        if pending.status != FocusStatus::Pending
            || !matches!(pending.source_work, SourceWorkState::NotStarted | SourceWorkState::Quiescent)
        {
            return Err(state_failed());
        }
        self.remove_focus(key, now, wall)
    }

    /// The caller authenticates result consumption and parent-pin settlement before retirement.
    pub fn retire_focused(&mut self, key: &str, now: Duration, wall: DateTime<Utc>) -> Result<ScheduleUpdate> {
        self.require_idle()?;
        if !matches!(self.focused.get(key).map(|pending| &pending.status), Some(FocusStatus::Completed { .. })) {
            return Err(state_failed());
        }
        self.remove_focus(key, now, wall)
    }

    /// Family cooldown survives job removal; only an expired, unreferenced family can be collected.
    pub fn retire_focused_family(
        &mut self,
        key: &str,
        now: Duration,
        wall: DateTime<Utc>,
    ) -> Result<Option<ScheduleUpdate>> {
        self.require_idle()?;
        let Some(family) = self.focused_families.get(key) else { return Ok(None) };
        if family.due > now || self.focused.values().any(|pending| pending.job.family == key) {
            return Ok(None);
        }
        let route = self.routes.get(&family.target).ok_or_else(state_failed)?;
        let mut update = self.metadata_update(&route.resource, now, wall)?;
        update.focused_family = Some(FocusFamilyMutation::Remove { key: key.into() });
        self.advance_metadata_sequence(&mut update)?;
        self.focused_families.remove(key);
        Ok(Some(update))
    }

    fn remove_focus(&mut self, key: &str, now: Duration, wall: DateTime<Utc>) -> Result<ScheduleUpdate> {
        let pending = self.focused.get(key).ok_or_else(state_failed)?;
        let mut update = self.metadata_update(&pending.job.resource, now, wall)?;
        update.focused = Some(FocusMutation::Remove { key: key.into() });
        self.advance_metadata_sequence(&mut update)?;
        self.focused.remove(key);
        Ok(update)
    }

    fn advance_metadata_sequence(&mut self, update: &mut ScheduleUpdate) -> Result<()> {
        let next = self.sequence.checked_add(1).ok_or_else(state_failed)?;
        update.clock.sequence = next;
        self.sequence = next;
        Ok(())
    }

    fn require_idle(&self) -> Result<()> {
        if self.active.is_some()
            || self.clock.as_ref().is_none_or(|clock| clock.pending_job.is_some() || clock.sequence != self.sequence)
        {
            return Err(state_failed());
        }
        Ok(())
    }
    pub(super) fn matches_route(&self, job: &FocusedJob) -> bool {
        self.routes.get(&job.target).is_some_and(|route| {
            route.connection == job.connection && route.subject == job.subject && route.resource == job.resource
        })
    }
    pub(super) fn focus_ready_at(&self, pending: &FocusedAdmission) -> Duration {
        pending
            .due
            .max(self.resources.get(&pending.job.resource).map_or(Duration::ZERO, |state| state.due))
            .max(self.focused_families.get(&pending.job.family).map_or(Duration::ZERO, |family| family.due))
    }
    pub(super) fn focus_completion(&self, key: &str, now: Duration, outcome: &Outcome) -> Result<FocusCompletion> {
        let mut pending = self.focused.get(key).cloned().ok_or_else(state_failed)?;
        match outcome {
            Outcome::FocusedComplete { result } if result.is_valid() => {
                pending.status = FocusStatus::Completed { result: result.clone() };
                pending.last_attempt = Some(result.capture.clone());
                pending.source_work = SourceWorkState::Quiescent;
            }
            Outcome::FocusedRetry { attempt, source_work, .. }
                if *source_work != SourceWorkState::Active
                    && attempt.as_ref().is_none_or(checkpoint::valid_attempt) =>
            {
                pending.status = FocusStatus::Pending;
                pending.last_attempt = attempt.clone();
                // A later metadata-only rejection does not establish closure of an older uncertain native owner.
                if *source_work != SourceWorkState::NotStarted || pending.source_work != SourceWorkState::Unconfirmed {
                    pending.source_work = *source_work;
                }
            }
            Outcome::Failed | Outcome::Pressure | Outcome::More => {
                pending.status = FocusStatus::Pending;
                pending.last_attempt = None;
                pending.source_work = SourceWorkState::Unconfirmed;
            }
            _ => return Err(state_failed()),
        }
        pending.attempts = pending.attempts.checked_add(1).ok_or_else(state_failed)?;
        pending.failures = if matches!(outcome.kind(), OutcomeKind::Failed | OutcomeKind::Pressure) {
            pending.failures.checked_add(1).ok_or_else(state_failed)?
        } else {
            0
        };
        let mut family = self.focused_families.get(&pending.job.family).cloned().ok_or_else(state_failed)?;
        family.cooldown_seconds = family.cooldown_seconds.max(pending.job.cooldown_seconds);
        pending.due =
            now.checked_add(failure_delay(family.cooldown_seconds, pending.failures)).ok_or_else(state_failed)?;
        family.due = pending.due;
        let resource = self.resource_completion(
            &pending.job.resource,
            now,
            outcome.kind(),
            native_reservation(&pending.job.budget),
        )?;
        Ok(FocusCompletion { pending, resource, family })
    }
    pub(super) fn finish_focus(&mut self, key: &str, now: Duration, outcome: &Outcome) -> Result<()> {
        let completion = self.focus_completion(key, now, outcome)?;
        self.resources.insert(completion.pending.job.resource.clone(), completion.resource);
        self.focused_families.insert(completion.pending.job.family.clone(), completion.family);
        self.focused.insert(key.into(), completion.pending);
        Ok(())
    }
    fn resource_for(&self, key: &str) -> Result<&ResourceAdmission> {
        self.resources.get(key).ok_or_else(state_failed)
    }
}

pub(super) fn focus_family_key(target: &str, recipe: &str) -> Result<String> {
    serde_json::to_vec(&("diagnostic-focus-family-v1", target, recipe))
        .map(|bytes| digest(&bytes))
        .map_err(|_| state_failed())
}
pub(super) fn identifier(value: &str) -> bool {
    !value.is_empty() && value.len() <= 256 && !value.chars().any(char::is_control)
}
pub(super) fn valid_cooldown(seconds: u32) -> bool {
    (RESOURCE_GAP.as_secs() as u32..=MAX_FOCUS_COOLDOWN_SECONDS).contains(&seconds)
}
