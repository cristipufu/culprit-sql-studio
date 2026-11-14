//! Restore the exact configured periodic membership and bounded one-off working set without earning uncertain downtime.
use super::checkpoint::{maximum_focus_delay, maximum_job_delay, valid_wall};
use super::*;
use chrono::{DateTime, Utc};
use std::collections::BTreeSet;

#[derive(Default)]
struct ResourceLimit {
    reservation: Duration,
    attempts: u64,
}
impl ResourceLimit {
    fn include(&mut self, attempts: u32, reservation: Duration) -> Result<()> {
        self.attempts = self.attempts.checked_add(u64::from(attempts)).ok_or_else(state_failed)?;
        if attempts > 0 {
            self.reservation = self.reservation.max(reservation);
        }
        Ok(())
    }
}

impl Scheduler {
    pub fn restore(&mut self, state: ScheduleState, now: Duration, wall: DateTime<Utc>) -> Result<()> {
        if self.clock.is_some()
            || self.active.is_some()
            || !valid_wall(wall)
            || !valid_wall(state.clock.wall)
            || state.jobs.len() != self.jobs.len()
            || state.jobs.len() > MAX_SCHEDULED_JOBS
            || state.resources.keys().ne(self.resources.keys())
            || state.resources.len() > MAX_FLEET_TARGETS
            || state.families.keys().ne(self.families.keys())
            || state.families.len() > MAX_SCHEDULE_FAMILIES
            || state.focused.len() > MAX_FOCUSED_JOBS
            || state.focused_families.len() > MAX_FOCUSED_FAMILIES
        {
            return Err(state_failed());
        }
        let interrupted = self.interrupted_work(&state)?;
        let mut turns = BTreeSet::new();
        let mut resource_limits: BTreeMap<String, ResourceLimit> =
            self.resources.keys().map(|key| (key.clone(), ResourceLimit::default())).collect();
        let mut family_limits: BTreeMap<String, Duration> = BTreeMap::new();
        let mut jobs = self.jobs.clone();
        for pending in &mut jobs {
            let saved = state.jobs.get(&pending.job.key).ok_or_else(state_failed)?;
            validate_turn(saved.turn, &state.clock, &mut turns)?;
            if saved.failures > saved.attempts {
                return Err(state_failed());
            }
            pending.due = saved.delay.restore(
                &state.clock,
                now,
                wall,
                maximum_job_delay(&pending.job),
                matches!(&interrupted, Some(Work::Scheduled(job)) if job.key == pending.job.key),
            )?;
            pending.attempts = saved.attempts;
            pending.failures = saved.failures;
            pending.turn = saved.turn;
            resource_limits
                .get_mut(&pending.job.resource)
                .ok_or_else(state_failed)?
                .include(saved.attempts, native_reservation(&pending.job.schedule.budget))?;
            family_limits
                .entry(pending.job.family.clone())
                .and_modify(|limit| *limit = (*limit).max(maximum_job_delay(&pending.job)))
                .or_insert_with(|| maximum_job_delay(&pending.job));
        }
        let focused_families = self.restore_focus_families(&state, interrupted.as_ref(), now, wall)?;
        let focused =
            self.restore_focus_jobs(&state, &focused_families, &mut resource_limits, &mut turns, now, wall)?;
        let resources = restore_resources(&state, &resource_limits, interrupted.as_ref(), now, wall)?;
        let mut families = BTreeMap::new();
        for (key, saved) in &state.families {
            let maximum = family_limits.get(key).copied().ok_or_else(state_failed)?;
            let pending = matches!(&interrupted, Some(Work::Scheduled(job)) if job.family == *key);
            families.insert(key.clone(), saved.restore(&state.clock, now, wall, maximum, pending)?);
        }
        self.jobs = jobs;
        self.resources = resources;
        self.families = families;
        self.focused = focused;
        self.focused_families = focused_families;
        self.sequence = state.clock.sequence;
        self.completed = state.clock.pending_job.clone();
        self.clock = Some(state.clock);
        Ok(())
    }

    fn interrupted_work(&self, state: &ScheduleState) -> Result<Option<Work>> {
        match &state.clock.pending_job {
            Some(WorkKey::Scheduled { key }) => {
                let index = self.static_index(key)?;
                let saved = state.jobs.get(key).ok_or_else(state_failed)?;
                if saved.attempts == 0 || saved.failures == 0 || saved.turn.checked_add(1) != Some(state.clock.sequence)
                {
                    return Err(state_failed());
                }
                Ok(Some(Work::Scheduled(self.jobs[index].job.clone())))
            }
            Some(WorkKey::Focused { key }) => {
                let saved = state.focused.get(key).ok_or_else(state_failed)?;
                if !saved.is_valid()
                    || saved.status != FocusStatus::Admitted
                    || saved.turn.checked_add(1) != Some(state.clock.sequence)
                {
                    return Err(state_failed());
                }
                Ok(Some(Work::Focused(saved.job.clone())))
            }
            None => Ok(None),
        }
    }

    fn restore_focus_families(
        &self,
        state: &ScheduleState,
        interrupted: Option<&Work>,
        now: Duration,
        wall: DateTime<Utc>,
    ) -> Result<BTreeMap<String, FocusFamilyAdmission>> {
        state
            .focused_families
            .iter()
            .map(|(key, saved)| {
                if !saved.is_valid() || saved.key()? != *key || !self.routes.contains_key(&saved.target) {
                    return Err(state_failed());
                }
                let pending = matches!(interrupted, Some(Work::Focused(job)) if job.family == *key);
                let due = saved.delay.restore(
                    &state.clock,
                    now,
                    wall,
                    maximum_focus_delay(saved.cooldown_seconds),
                    pending,
                )?;
                Ok((
                    key.clone(),
                    FocusFamilyAdmission {
                        target: saved.target.clone(),
                        recipe_id: saved.recipe_id.clone(),
                        cooldown_seconds: saved.cooldown_seconds,
                        due,
                    },
                ))
            })
            .collect()
    }

    fn restore_focus_jobs(
        &self,
        state: &ScheduleState,
        families: &BTreeMap<String, FocusFamilyAdmission>,
        resources: &mut BTreeMap<String, ResourceLimit>,
        turns: &mut BTreeSet<u64>,
        now: Duration,
        wall: DateTime<Utc>,
    ) -> Result<BTreeMap<String, FocusedAdmission>> {
        let mut episodes = BTreeSet::new();
        let mut stages = BTreeSet::new();
        let mut result = BTreeMap::new();
        for (key, saved) in &state.focused {
            if !saved.is_valid()
                || saved.job.key != *key
                || !self.matches_route(&saved.job)
                || !episodes.insert(saved.job.episode_id.clone())
                || !stages.insert(saved.job.episode_stage_key.clone())
            {
                return Err(state_failed());
            }
            validate_turn(saved.turn, &state.clock, turns)?;
            let pending = matches!(&state.clock.pending_job, Some(WorkKey::Focused { key: pending }) if pending == key);
            if (saved.status == FocusStatus::Admitted) != pending {
                return Err(state_failed());
            }
            let family = families.get(&saved.job.family).ok_or_else(state_failed)?;
            if family.target != saved.job.target
                || family.recipe_id != saved.job.recipe_id
                || family.cooldown_seconds < saved.job.cooldown_seconds
            {
                return Err(state_failed());
            }
            let due =
                saved.delay.restore(&state.clock, now, wall, maximum_focus_delay(family.cooldown_seconds), pending)?;
            resources
                .get_mut(&saved.job.resource)
                .ok_or_else(state_failed)?
                .include(saved.attempts, native_reservation(&saved.job.budget))?;
            result.insert(
                key.clone(),
                FocusedAdmission {
                    job: saved.job.clone(),
                    due,
                    attempts: saved.attempts,
                    failures: saved.failures,
                    turn: saved.turn,
                    status: if pending { FocusStatus::Pending } else { saved.status.clone() },
                    last_attempt: saved.last_attempt.clone(),
                    source_work: if pending { SourceWorkState::Unconfirmed } else { saved.source_work },
                },
            );
        }
        Ok(result)
    }
}

fn restore_resources(
    state: &ScheduleState,
    limits: &BTreeMap<String, ResourceLimit>,
    interrupted: Option<&Work>,
    now: Duration,
    wall: DateTime<Utc>,
) -> Result<BTreeMap<String, ResourceAdmission>> {
    state
        .resources
        .iter()
        .map(|(key, saved)| {
            let limit = limits.get(key).ok_or_else(state_failed)?;
            let reservation = Duration::from_millis(saved.max_reservation_ms);
            if saved.attempts < limit.attempts
                || u64::from(saved.pressure_failures) > saved.attempts
                || reservation < limit.reservation
                || reservation > native_reservation(&CollectionBudget::upper_limits())
                || (saved.attempts == 0) != reservation.is_zero()
            {
                return Err(state_failed());
            }
            let pending = interrupted.is_some_and(|work| work.resource() == key);
            let due = saved.delay.restore(&state.clock, now, wall, reservation + MAX_FAILURE_DELAY, pending)?;
            Ok((
                key.clone(),
                ResourceAdmission {
                    due,
                    pressure_failures: saved.pressure_failures,
                    attempts: saved.attempts,
                    max_reservation: reservation,
                },
            ))
        })
        .collect()
}

fn validate_turn(turn: u64, clock: &ScheduleClock, seen: &mut BTreeSet<u64>) -> Result<()> {
    if turn >= clock.sequence || !seen.insert(turn) {
        return Err(state_failed());
    }
    Ok(())
}
