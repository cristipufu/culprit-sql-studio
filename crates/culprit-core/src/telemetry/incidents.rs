//! Deterministic incident transitions. Call only for a complete successful poll; failed polls cannot clear state.
use super::*;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Serialize, Deserialize, TS, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum ActivityIncidentStatus {
    Current,
    Uncertain,
}
#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[ts(export)]
pub struct ActivityIncidentState {
    pub status: ActivityIncidentStatus,
    pub id: String,
    pub first_seen_at: String,
    pub last_seen_at: String,
    pub last_emitted_at: String,
    pub conditions: Vec<ActivityCondition>,
    pub progress: ActivityProgress,
    pub observation: ActivityObservation,
}
impl ActivityIncidentState {
    pub fn is_valid_at(&self, at: &str) -> bool {
        if !self.is_valid() {
            return false;
        }
        let time = |value: &str| DateTime::parse_from_rfc3339(value).ok();
        matches!((time(at),time(&self.last_seen_at),time(&self.last_emitted_at)),(Some(at),Some(seen),Some(emitted))
            if emitted<=at && match self.status {ActivityIncidentStatus::Current=>seen==at,ActivityIncidentStatus::Uncertain=>seen<=at})
    }
    pub fn is_valid(&self) -> bool {
        valid_digest(&self.id)
            && valid_time(&self.first_seen_at)
            && valid_time(&self.last_seen_at)
            && valid_time(&self.last_emitted_at)
            && !self.conditions.is_empty()
            && self.observation.is_valid()
    }
}
#[derive(Serialize, Deserialize, TS, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum ActivityIncidentTransition {
    Started,
    Updated,
    Resolved,
}
#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[ts(export)]
pub struct ActivityIncidentChange {
    pub id: String,
    pub at: String,
    pub transition: ActivityIncidentTransition,
    pub state: ActivityIncidentState,
}
impl ActivityIncidentChange {
    pub fn is_valid(&self) -> bool {
        valid_digest(&self.id) && valid_time(&self.at) && self.state.is_valid()
    }
}

pub struct ActivityIncidentUpdate {
    pub active: Vec<ActivityIncidentState>,
    pub changes: Vec<ActivityIncidentChange>,
}
impl ActivityIncidentUpdate {
    pub fn observe(
        source: &CollectionDescription,
        capture: &CaptureIdentity,
        rows: &[ActivityObservation],
        before: &[ActivityIncidentState],
    ) -> Result<Self> {
        let snapshot = source.observation.activity().ok_or_else(|| CollectionStop::InvalidPage.error())?;
        if !snapshot.complete_scope {
            return Err(CollectionStop::SourceUnavailable.error());
        }
        let evidence = ActivityEvidence::select(rows, &snapshot.policy)?;
        if evidence.conditions.len() != snapshot.qualifying as usize
            || evidence.uncertain.len() != snapshot.uncertain as usize
        {
            return Err(CollectionStop::InvalidPage.error());
        }
        let previous: BTreeMap<_, _> = before.iter().map(|state| (state.observation.key.clone(), state)).collect();
        if previous.len() != before.len() || before.iter().any(|state| !state.is_valid()) {
            return Err(CollectionStop::InvalidPage.error());
        }
        let prior_rows = before.iter().map(|state| state.observation.clone()).collect::<Vec<_>>();
        let uncertain = ActivityEvidence::context_for(&prior_rows, &evidence.uncertain)?;
        let mut active = Vec::new();
        let mut changes = Vec::new();
        let mut current = BTreeSet::new();
        for observation in evidence.observations {
            let prior = previous.get(&observation.key).copied();
            if uncertain.contains(&observation.key)
                && let Some(prior) = prior
            {
                current.insert(observation.key.clone());
                let (state, change) = uncertain_state(source, capture, &snapshot.at, prior)?;
                changes.extend(change);
                active.push(state);
                continue;
            }
            let Some(conditions) = evidence.conditions.get(&observation.key) else {
                continue;
            };
            current.insert(observation.key.clone());
            let mut state = ActivityIncidentState {
                status: ActivityIncidentStatus::Current,
                id: match prior {
                    Some(prior) => prior.id.clone(),
                    None => source.scoped_id("activity-incident-v1", &(&capture.observation_id, &observation.key))?,
                },
                first_seen_at: prior.map(|prior| prior.first_seen_at.clone()).unwrap_or_else(|| snapshot.at.clone()),
                last_seen_at: snapshot.at.clone(),
                last_emitted_at: prior
                    .map(|prior| prior.last_emitted_at.clone())
                    .unwrap_or_else(|| snapshot.at.clone()),
                conditions: conditions.clone(),
                progress: prior
                    .map(|prior| observation.counters().progress(prior.observation.counters()))
                    .unwrap_or(ActivityProgress::FirstObservation),
                observation,
            };
            let transition = match prior {
                None => Some(ActivityIncidentTransition::Started),
                Some(prior)
                    if meaningful_change(prior, &state)
                        || update_due(&snapshot.at, &prior.last_emitted_at, snapshot.policy.update_every_ms) =>
                {
                    Some(ActivityIncidentTransition::Updated)
                }
                _ => None,
            };
            if let Some(transition) = transition {
                state.last_emitted_at = snapshot.at.clone();
                changes.push(change(source, capture, &snapshot.at, transition, &state)?);
            }
            active.push(state);
        }
        for prior in before {
            if !current.contains(&prior.observation.key) && uncertain.contains(&prior.observation.key) {
                let (state, change) = uncertain_state(source, capture, &snapshot.at, prior)?;
                changes.extend(change);
                active.push(state);
            } else if !current.contains(&prior.observation.key) {
                changes.push(change(source, capture, &snapshot.at, ActivityIncidentTransition::Resolved, prior)?);
            }
        }
        if active.iter().any(|state| !state.is_valid_at(&snapshot.at)) {
            return Err(CollectionStop::SourceChanged.error());
        }
        Ok(Self { active, changes })
    }
}
fn uncertain_state(
    source: &CollectionDescription,
    capture: &CaptureIdentity,
    at: &str,
    prior: &ActivityIncidentState,
) -> Result<(ActivityIncidentState, Option<ActivityIncidentChange>)> {
    let mut state = prior.clone();
    state.status = ActivityIncidentStatus::Uncertain;
    state.progress = ActivityProgress::Unknown;
    let change = if prior.status != state.status {
        state.last_emitted_at = at.into();
        Some(change(source, capture, at, ActivityIncidentTransition::Updated, &state)?)
    } else {
        None
    };
    Ok((state, change))
}
fn meaningful_change(before: &ActivityIncidentState, now: &ActivityIncidentState) -> bool {
    before.status != now.status
        || before.observation.query_hash != now.observation.query_hash
        || before.conditions != now.conditions
        || before.observation.wait.as_ref().map(|wait| (&wait.native_type, &wait.owners))
            != now.observation.wait.as_ref().map(|wait| (&wait.native_type, &wait.owners))
        || (now.progress == ActivityProgress::NoObservedCounterChange && before.progress != now.progress)
        || (before.progress == ActivityProgress::NoObservedCounterChange
            && now.progress == ActivityProgress::ChangedCounters)
}
fn update_due(now: &str, before: &str, interval_ms: u32) -> bool {
    match (chrono::DateTime::parse_from_rfc3339(now), chrono::DateTime::parse_from_rfc3339(before)) {
        (Ok(now), Ok(before)) => (now - before).num_milliseconds() >= i64::from(interval_ms),
        _ => false,
    }
}
fn change(
    source: &CollectionDescription,
    capture: &CaptureIdentity,
    at: &str,
    transition: ActivityIncidentTransition,
    state: &ActivityIncidentState,
) -> Result<ActivityIncidentChange> {
    Ok(ActivityIncidentChange {
        id: source.scoped_id("activity-change-v1", &(&capture.observation_id, &state.id, transition))?,
        at: at.into(),
        transition,
        state: state.clone(),
    })
}

#[cfg(test)]
#[path = "incidents_tests.rs"]
mod tests;
