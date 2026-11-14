//! Admission updates share the existing owned SQLite journal; each native turn changes only its bounded rows.
use super::*;
use crate::fleet::scheduler::{
    Delay, JobState, MAX_SCHEDULE_FAMILIES, MAX_SCHEDULED_JOBS, ResourceState, ScheduleClock, ScheduleState,
    ScheduleUpdate,
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
        match clock {
            Some(clock) => Ok(Some(ScheduleState { clock, jobs, resources, families })),
            None if jobs.is_empty() && resources.is_empty() && families.is_empty() => Ok(None),
            None => Err(state_failed()),
        }
    }
    fn initialize_scheduler(&mut self, state: ScheduleState) -> Result<ScheduleClock> {
        if state.clock.revision != 0
            || state.jobs.len() > MAX_SCHEDULED_JOBS
            || state.resources.len() > MAX_FLEET_TARGETS
            || state.families.len() > MAX_SCHEDULE_FAMILIES
        {
            return Err(state_failed());
        }
        let transaction = self.connection.transaction().map_err(|_| state_failed())?;
        if read_clock(&transaction)?.is_some() {
            return Err(state_failed());
        }
        let rows: i64 = transaction.query_row("SELECT (SELECT count(*) FROM admission_jobs)+(SELECT count(*) FROM admission_resources)+(SELECT count(*) FROM admission_families)", [], |row| row.get(0)).map_err(|_| state_failed())?;
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
        let transaction = self.connection.transaction().map_err(|_| state_failed())?;
        let previous = read_clock(&transaction)?.ok_or_else(state_failed)?;
        if previous.revision.checked_add(1) != Some(state.clock.revision)
            || state.clock.sequence < previous.sequence
            || state.clock.sequence > previous.sequence.saturating_add(1)
            || state.clock.wall < previous.wall
        {
            return Err(state_failed());
        }
        update_state(&transaction, "UPDATE admission_jobs SET state_json=?1 WHERE id=?2", &state.job.0, &state.job.1)?;
        update_state(
            &transaction,
            "UPDATE admission_resources SET state_json=?1 WHERE id=?2",
            &state.resource.0,
            &state.resource.1,
        )?;
        update_state(
            &transaction,
            "UPDATE admission_families SET state_json=?1 WHERE id=?2",
            &state.family.0,
            &state.family.1,
        )?;
        transaction
            .execute("UPDATE admission_clock SET state_json=?1 WHERE id=1", [encode(&state.clock)?])
            .map_err(|_| state_failed())?;
        transaction.commit().map_err(|_| state_failed())?;
        Ok(state.clock)
    }
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
fn read_clock(connection: &Connection) -> Result<Option<ScheduleClock>> {
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
fn encode(value: &impl Serialize) -> Result<String> {
    let json = serde_json::to_string(value).map_err(|_| state_failed())?;
    if json.len() > MAX_ADMISSION_ROW_BYTES {
        return Err(state_failed());
    }
    Ok(json)
}
fn decode<T: DeserializeOwned>(json: &str) -> Result<T> {
    if json.len() > MAX_ADMISSION_ROW_BYTES {
        return Err(state_failed());
    }
    serde_json::from_str(json).map_err(|_| state_failed())
}
