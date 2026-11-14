//! Admission updates share the existing owned SQLite journal; each native turn changes only its bounded rows.
use super::*;
use crate::fleet::scheduler::{
    Delay, FocusFamilyMutation, FocusFamilyState, FocusMutation, FocusState, FocusStatus, JobState,
    MAX_FOCUSED_FAMILIES, MAX_FOCUSED_JOBS, MAX_SCHEDULE_FAMILIES, MAX_SCHEDULED_JOBS, ResourceState, ScheduleClock,
    ScheduleState, ScheduleUpdate, WorkKey,
};
use serde::{Serialize, de::DeserializeOwned};
use std::collections::BTreeMap;

const MAX_ADMISSION_ROW_BYTES: usize = 4096;
const MAX_ADMISSION_BYTES: usize = 16 * 1024 * 1024;

impl FleetJournal {
    pub(in crate::fleet) async fn scheduler(&self) -> Result<Option<ScheduleState>> {
        self.files.run(|| |files| files.scheduler()).await
    }
    pub(in crate::fleet) async fn initialize_scheduler(&self, state: ScheduleState) -> Result<ScheduleClock> {
        self.files.run(|| move |files| files.initialize_scheduler(state)).await
    }
    pub(in crate::fleet) async fn update_scheduler(&self, state: ScheduleUpdate) -> Result<ScheduleClock> {
        self.files.run(|| move |files| files.update_scheduler(state)).await
    }
}
impl Files {
    fn scheduler(&self) -> Result<Option<ScheduleState>> {
        let clock = read_clock(&self.connection)?;
        let mut bytes = 0;
        let jobs: BTreeMap<String, JobState> = read_states(
            &self.connection,
            "SELECT substr(id,1,257),substr(state_json,1,4097) FROM admission_jobs LIMIT ?1",
            MAX_SCHEDULED_JOBS,
            &mut bytes,
        )?;
        let resources: BTreeMap<String, ResourceState> = read_states(
            &self.connection,
            "SELECT substr(id,1,257),substr(state_json,1,4097) FROM admission_resources LIMIT ?1",
            MAX_FLEET_TARGETS,
            &mut bytes,
        )?;
        let families: BTreeMap<String, Delay> = read_states(
            &self.connection,
            "SELECT substr(id,1,257),substr(state_json,1,4097) FROM admission_families LIMIT ?1",
            MAX_SCHEDULE_FAMILIES,
            &mut bytes,
        )?;
        let focused: BTreeMap<String, FocusState> = read_states(
            &self.connection,
            "SELECT substr(id,1,257),substr(state_json,1,4097) FROM admission_focused LIMIT ?1",
            MAX_FOCUSED_JOBS,
            &mut bytes,
        )?;
        let focused_families: BTreeMap<String, FocusFamilyState> = read_states(
            &self.connection,
            "SELECT substr(id,1,257),substr(state_json,1,4097) FROM admission_focus_families LIMIT ?1",
            MAX_FOCUSED_FAMILIES,
            &mut bytes,
        )?;
        validate_intents(&self.connection, &focused)?;
        match clock {
            Some(clock) => Ok(Some(ScheduleState { clock, jobs, resources, families, focused, focused_families })),
            None if jobs.is_empty()
                && resources.is_empty()
                && families.is_empty()
                && focused.is_empty()
                && focused_families.is_empty() =>
            {
                Ok(None)
            }
            None => Err(state_failed()),
        }
    }
    fn initialize_scheduler(&mut self, state: ScheduleState) -> Result<ScheduleClock> {
        if state.clock.revision != 0
            || state.jobs.len() > MAX_SCHEDULED_JOBS
            || state.resources.len() > MAX_FLEET_TARGETS
            || state.families.len() > MAX_SCHEDULE_FAMILIES
            || !state.focused.is_empty()
            || !state.focused_families.is_empty()
        {
            return Err(state_failed());
        }
        let transaction = self.connection.transaction().map_err(|_| state_failed())?;
        if read_clock(&transaction)?.is_some() {
            return Err(state_failed());
        }
        let rows: i64 = transaction.query_row("SELECT (SELECT count(*) FROM admission_jobs)+(SELECT count(*) FROM admission_resources)+(SELECT count(*) FROM admission_families)+(SELECT count(*) FROM admission_focused)+(SELECT count(*) FROM admission_focus_families)+(SELECT count(*) FROM investigation_intents)", [], |row| row.get(0)).map_err(|_| state_failed())?;
        if rows != 0 {
            return Err(state_failed());
        }
        let mut bytes = 0;
        insert_states(&transaction, "INSERT INTO admission_jobs VALUES(?1,?2)", state.jobs, &mut bytes)?;
        insert_states(&transaction, "INSERT INTO admission_resources VALUES(?1,?2)", state.resources, &mut bytes)?;
        insert_states(&transaction, "INSERT INTO admission_families VALUES(?1,?2)", state.families, &mut bytes)?;
        transaction
            .execute("INSERT INTO admission_clock VALUES(1,?1)", [encode(&state.clock)?])
            .map_err(|_| state_failed())?;
        transaction.commit().map_err(|_| state_failed())?;
        Ok(state.clock)
    }
    fn update_scheduler(&mut self, state: ScheduleUpdate) -> Result<ScheduleClock> {
        if matches!(state.focused, Some(FocusMutation::Insert { .. } | FocusMutation::Remove { .. })) {
            return Err(state_failed());
        }
        let transaction = self.connection.transaction().map_err(|_| state_failed())?;
        apply_update(&transaction, &state)?;
        transaction.commit().map_err(|_| state_failed())?;
        Ok(state.clock)
    }
}

pub(super) fn apply_update(connection: &Connection, state: &ScheduleUpdate) -> Result<()> {
    let previous = read_clock(connection)?.ok_or_else(state_failed)?;
    if previous.revision.checked_add(1) != Some(state.clock.revision)
        || state.clock.sequence < previous.sequence
        || state.clock.sequence > previous.sequence.saturating_add(1)
        || state.clock.wall < previous.wall
    {
        return Err(state_failed());
    }
    if let Some(pending) = &previous.pending_job {
        let touched = match (&state.job, &state.focused) {
            (Some((key, _)), None) => Some(WorkKey::Scheduled { key: key.clone() }),
            (None, Some(FocusMutation::Update { key, .. })) => Some(WorkKey::Focused { key: key.clone() }),
            _ => None,
        };
        if touched.as_ref() != Some(pending) || state.clock.pending_job.is_some() {
            return Err(state_failed());
        }
    }
    match (&state.job, &state.focused, &state.focused_family) {
        (Some(_), None, None) if state.family.is_some() => {}
        (None, Some(_), _) if state.family.is_none() => {}
        (None, None, Some(FocusFamilyMutation::Remove { .. }))
            if state.family.is_none() && previous.pending_job.is_none() && state.clock.pending_job.is_none() => {}
        _ => return Err(state_failed()),
    }
    if let Some((key, job)) = &state.job {
        if state.clock.pending_job.as_ref().is_some_and(|pending| pending != &WorkKey::Scheduled { key: key.clone() }) {
            return Err(state_failed());
        }
        update_state(connection, "UPDATE admission_jobs SET state_json=?1 WHERE id=?2", key, job)?;
    }
    if let Some(mutation) = &state.focused {
        apply_focus(connection, state, mutation)?;
    }
    update_state(
        connection,
        "UPDATE admission_resources SET state_json=?1 WHERE id=?2",
        &state.resource.0,
        &state.resource.1,
    )?;
    if let Some((key, family)) = &state.family {
        update_state(connection, "UPDATE admission_families SET state_json=?1 WHERE id=?2", key, family)?;
    }
    if let Some(mutation) = &state.focused_family {
        apply_family(connection, mutation)?;
    }
    connection
        .execute("UPDATE admission_clock SET state_json=?1 WHERE id=1", [encode(&state.clock)?])
        .map_err(|_| state_failed())?;
    Ok(())
}

fn apply_focus(connection: &Connection, update: &ScheduleUpdate, mutation: &FocusMutation) -> Result<()> {
    if let FocusMutation::Insert { state, .. } | FocusMutation::Update { state, .. } = mutation {
        let Some(FocusFamilyMutation::Upsert { key, state: family }) = &update.focused_family else {
            return Err(state_failed());
        };
        if *key != state.job.family
            || family.target != state.job.target
            || family.recipe_id != state.job.recipe_id
            || family.cooldown_seconds < state.job.cooldown_seconds
        {
            return Err(state_failed());
        }
    }
    match mutation {
        FocusMutation::Insert { key, state } => {
            if update.clock.pending_job.is_some()
                || state.status != FocusStatus::Pending
                || state.attempts != 0
                || state.failures != 0
                || state.last_attempt.is_some()
            {
                return Err(state_failed());
            }
            validate_focus(connection, key, state, &update.resource.0)?;
            let count: u32 = connection
                .query_row("SELECT count(*) FROM admission_focused", [], |row| row.get(0))
                .map_err(|_| state_failed())?;
            if count as usize >= MAX_FOCUSED_JOBS {
                return Err(DeliveryProblem::Capacity.error());
            }
            connection
                .execute("INSERT INTO admission_focused VALUES(?1,?2)", params![key, encode(state)?])
                .map_err(|_| state_failed())?;
        }
        FocusMutation::Update { key, state } => {
            validate_focus(connection, key, state, &update.resource.0)?;
            let before: String = connection
                .query_row("SELECT state_json FROM admission_focused WHERE id=?1", [key], |row| row.get(0))
                .map_err(|_| state_failed())?;
            let before: FocusState = decode(&before)?;
            if before.job != state.job
                || matches!(before.status, FocusStatus::Completed { .. }) && before.status != state.status
            {
                return Err(state_failed());
            }
            if (state.status == FocusStatus::Admitted)
                != (update.clock.pending_job.as_ref() == Some(&WorkKey::Focused { key: key.clone() }))
            {
                return Err(state_failed());
            }
            update_state(connection, "UPDATE admission_focused SET state_json=?1 WHERE id=?2", key, state)?;
        }
        FocusMutation::Remove { key } => {
            if update.clock.pending_job.is_some() {
                return Err(state_failed());
            }
            let changed =
                connection.execute("DELETE FROM admission_focused WHERE id=?1", [key]).map_err(|_| state_failed())?;
            if changed != 1 {
                return Err(state_failed());
            }
        }
    }
    Ok(())
}
fn validate_focus(connection: &Connection, key: &str, state: &FocusState, resource: &str) -> Result<()> {
    let intent = super::investigations::intent(connection, key)?.ok_or_else(state_failed)?;
    if !state.is_valid() || !state.job.matches_intent(&intent) || state.job.key != key || state.job.resource != resource
    {
        return Err(state_failed());
    }
    Ok(())
}
fn apply_family(connection: &Connection, mutation: &FocusFamilyMutation) -> Result<()> {
    match mutation {
        FocusFamilyMutation::Upsert { key, state } => {
            if !state.is_valid() || state.key()? != *key {
                return Err(state_failed());
            }
            let count: u32 = connection
                .query_row("SELECT count(*) FROM admission_focus_families", [], |row| row.get(0))
                .map_err(|_| state_failed())?;
            let exists: bool = connection
                .query_row("SELECT EXISTS(SELECT 1 FROM admission_focus_families WHERE id=?1)", [key], |row| row.get(0))
                .map_err(|_| state_failed())?;
            if !exists && count as usize >= MAX_FOCUSED_FAMILIES {
                return Err(DeliveryProblem::Capacity.error());
            }
            connection.execute("INSERT INTO admission_focus_families VALUES(?1,?2) ON CONFLICT(id) DO UPDATE SET state_json=excluded.state_json",params![key,encode(state)?]).map_err(|_|state_failed())?;
        }
        FocusFamilyMutation::Remove { key } => {
            let mut bytes = 0;
            let focused: BTreeMap<String, FocusState> = read_states(
                connection,
                "SELECT id,state_json FROM admission_focused LIMIT ?1",
                MAX_FOCUSED_JOBS,
                &mut bytes,
            )?;
            if focused.values().any(|state| state.job.family == *key) {
                return Err(state_failed());
            }
            let changed = connection
                .execute("DELETE FROM admission_focus_families WHERE id=?1", [key])
                .map_err(|_| state_failed())?;
            if changed != 1 {
                return Err(state_failed());
            }
        }
    }
    Ok(())
}
fn validate_intents(connection: &Connection, focused: &BTreeMap<String, FocusState>) -> Result<()> {
    let intents = super::investigations::intents(connection)?;
    if intents.keys().ne(focused.keys()) {
        return Err(state_failed());
    }
    for (key, state) in focused {
        if !state.is_valid() || !state.job.matches_intent(intents.get(key).ok_or_else(state_failed)?) {
            return Err(state_failed());
        }
    }
    Ok(())
}
fn insert_states<T: Serialize>(
    connection: &Connection,
    query: &str,
    states: BTreeMap<String, T>,
    bytes: &mut usize,
) -> Result<()> {
    for (key, state) in states {
        let json = encode(&state)?;
        *bytes = bytes.checked_add(key.len() + json.len()).ok_or_else(state_failed)?;
        if *bytes > MAX_ADMISSION_BYTES {
            return Err(state_failed());
        }
        connection.execute(query, params![key, json]).map_err(|_| state_failed())?;
    }
    Ok(())
}
fn update_state(connection: &Connection, query: &str, key: &str, state: &impl Serialize) -> Result<()> {
    let changed = connection.execute(query, params![encode(state)?, key]).map_err(|_| state_failed())?;
    if changed != 1 {
        return Err(state_failed());
    }
    Ok(())
}
pub(super) fn read_clock(connection: &Connection) -> Result<Option<ScheduleClock>> {
    let json: Option<String> = connection
        .query_row("SELECT substr(state_json,1,4097) FROM admission_clock WHERE id=1", [], |row| row.get(0))
        .optional()
        .map_err(|_| state_failed())?;
    json.map(|json| decode(&json)).transpose()
}
fn read_states<T: DeserializeOwned>(
    connection: &Connection,
    query: &str,
    maximum: usize,
    bytes: &mut usize,
) -> Result<BTreeMap<String, T>> {
    let mut statement = connection.prepare(query).map_err(|_| state_failed())?;
    let limit = maximum.checked_add(1).and_then(|value| i64::try_from(value).ok()).ok_or_else(state_failed)?;
    let mut rows = statement.query([limit]).map_err(|_| state_failed())?;
    let mut states = BTreeMap::new();
    while let Some(row) = rows.next().map_err(|_| state_failed())? {
        let key: String = row.get(0).map_err(|_| state_failed())?;
        let json: String = row.get(1).map_err(|_| state_failed())?;
        *bytes = bytes.checked_add(key.len() + json.len()).ok_or_else(state_failed)?;
        if states.len() == maximum || key.is_empty() || key.len() > 256 || *bytes > MAX_ADMISSION_BYTES {
            return Err(state_failed());
        }
        let state = decode(&json)?;
        if states.insert(key, state).is_some() {
            return Err(state_failed());
        }
    }
    Ok(states)
}
pub(super) fn encode(value: &impl Serialize) -> Result<String> {
    let json = serde_json::to_string(value).map_err(|_| state_failed())?;
    if json.len() > MAX_ADMISSION_ROW_BYTES {
        return Err(state_failed());
    }
    Ok(json)
}
pub(super) fn decode<T: DeserializeOwned>(json: &str) -> Result<T> {
    if json.len() > MAX_ADMISSION_ROW_BYTES {
        return Err(state_failed());
    }
    serde_json::from_str(json).map_err(|_| state_failed())
}
