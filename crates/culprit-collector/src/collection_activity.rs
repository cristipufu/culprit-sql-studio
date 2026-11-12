//! Activity reference closure and derived incident records share one canonical evidence model.
use culprit_core::telemetry::*;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Default)]
pub(crate) struct ActivityRecords {
    pub observations: Vec<ActivityObservation>,
    pub blocking: Vec<BlockingObservation>,
    pub checkpoints: Vec<ActivityIncidentState>,
    pub changes: Vec<ActivityIncidentChange>,
}
impl ActivityRecords {
    pub fn record(&mut self, record: &TelemetryRecord) {
        match record {
            TelemetryRecord::Activity(row) => self.observations.push(row.clone()),
            TelemetryRecord::Blocking(row) => self.blocking.push(row.clone()),
            TelemetryRecord::ActivityCheckpoint(row) => self.checkpoints.push(row.clone()),
            TelemetryRecord::Incident(row) => self.changes.push(row.clone()),
            _ => {}
        }
    }
    pub fn coverage(&self, snapshot: &ActivitySnapshotDescription) -> ActivityReferenceCoverage {
        let complete = self.verify(snapshot);
        ActivityReferenceCoverage {
            observations: self.observations.len() as u32,
            blocking_edges: self.blocking.len() as u32,
            unresolved_blockers: self
                .blocking
                .iter()
                .filter(|edge| !edge.owner_observed && matches!(edge.owner, ActivityWaitOwner::Session { .. }))
                .count() as u32,
            complete,
        }
    }
    fn verify(&self, snapshot: &ActivitySnapshotDescription) -> bool {
        if !snapshot.complete_scope {
            return false;
        }
        let Ok(expected) = ActivityEvidence::select(&self.observations, &snapshot.policy) else {
            return false;
        };
        if self.observations.len() != snapshot.observations as usize
            || self.blocking.len() != snapshot.blocking_edges as usize
            || expected.conditions.len() != snapshot.qualifying as usize
            || expected.uncertain.len() != snapshot.uncertain as usize
            || expected.observations.len() != self.observations.len()
            || expected.blocking.iter().collect::<BTreeSet<_>>() != self.blocking.iter().collect::<BTreeSet<_>>()
        {
            return false;
        }
        let current: BTreeMap<_, _> = self.checkpoints.iter().map(|state| (&state.observation.key, state)).collect();
        if current.len() != self.checkpoints.len() || expected.conditions.keys().any(|key| !current.contains_key(key)) {
            return false;
        }
        let prior_rows = self.checkpoints.iter().map(|state| state.observation.clone()).collect::<Vec<_>>();
        let Ok(uncertain) = ActivityEvidence::context_for(&prior_rows, &expected.uncertain) else {
            return false;
        };
        for state in &self.checkpoints {
            if !state.is_valid_at(&snapshot.at) {
                return false;
            }
            if state.status == ActivityIncidentStatus::Uncertain {
                if !uncertain.contains(&state.observation.key) {
                    return false;
                }
            } else {
                let Some(conditions) = expected.conditions.get(&state.observation.key) else {
                    return false;
                };
                if state.conditions != *conditions || !self.observations.contains(&state.observation) {
                    return false;
                }
            }
        }
        self.changes.iter().all(|change| {
            change.is_valid()
                && change.at == snapshot.at
                && match change.transition {
                    ActivityIncidentTransition::Resolved => {
                        !current.contains_key(&change.state.observation.key)
                            && !uncertain.contains(&change.state.observation.key)
                    }
                    _ => current.get(&change.state.observation.key).is_some_and(|state| **state == change.state),
                }
        })
    }
}

#[derive(Clone)]
pub(crate) struct ActivityBaseline {
    pub source: culprit_core::telemetry::CollectionDescription,
    pub capture: CaptureIdentity,
    pub states: Vec<ActivityIncidentState>,
}
impl ActivityBaseline {
    pub fn matching(&self, source: &culprit_core::telemetry::CollectionDescription) -> &[ActivityIncidentState] {
        let comparable = self
            .source
            .observation
            .activity()
            .zip(source.observation.activity())
            .is_some_and(|(before, now)| before.policy == now.policy);
        if !comparable {
            return &[];
        }
        match (self.source.scoped_id("activity-state-scope-v1", &()), source.scoped_id("activity-state-scope-v1", &()))
        {
            (Ok(previous), Ok(current)) if previous == current => &self.states,
            _ => &[],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn baseline() -> ActivityBaseline {
        let observation: ActivityObservation = serde_json::from_value(serde_json::json!({
            "key":{"session":{"id":"61","generation":"login-1"},"request":{"id":"0","generation":"request-1"}},
            "blocking_status":"known","idle":false,"scope":"request-database","state":"running","open_transactions":0,"elapsed_ms":"120000",
            "session_counters":{},"request_counters":{},"statement":{"state":"unavailable","reason":"not-collected"}
        }))
        .unwrap();
        assert!(observation.is_valid());
        let source = CollectionDescription {
            target: culprit_core::DatabaseIdentity::server(
                culprit_core::Engine::Mssql,
                "db",
                "fixture.invalid",
                1433,
                None,
            )
            .into(),
            source: "activity".into(),
            continuity: SourceContinuity::Unverified,
            observation: SourceObservation::Activity(ActivitySnapshotDescription {
                epoch: "epoch-1".into(),
                at: "2020-01-01T06:00:00Z".into(),
                scope: culprit_core::MeasurementScope::Database,
                complete_scope: true,
                policy: ActivityPolicy::default(),
                native_rows: 1,
                qualifying: 1,
                uncertain: 0,
                observations: 1,
                blocking_edges: 0,
                details: ActivityDetailStatus::Collected,
            }),
        };
        let mut source = source;
        let CollectionTarget::Database(database) = &mut source.target else { panic!("database fixture") };
        database.incarnation = Some("native-database".into());
        let state = ActivityIncidentState {
            status: culprit_core::telemetry::ActivityIncidentStatus::Current,
            id: digest(b"incident"),
            first_seen_at: "2020-01-01T06:00:00Z".into(),
            last_seen_at: "2020-01-01T06:00:00Z".into(),
            last_emitted_at: "2020-01-01T06:00:00Z".into(),
            conditions: vec![ActivityCondition::LongRunning],
            progress: ActivityProgress::FirstObservation,
            observation,
        };
        assert!(state.is_valid());
        ActivityBaseline {
            source,
            capture: CaptureIdentity { observation_id: "fixture".into(), revision: None },
            states: vec![state],
        }
    }
    #[test]
    fn native_epoch_or_policy_changes_cannot_reuse_a_prior_incident_even_when_session_tokens_match() {
        let before = baseline();
        assert_eq!(before.matching(&before.source).len(), 1);
        let mut source = before.source.clone();
        let SourceObservation::Activity(snapshot) = &mut source.observation else { unreachable!() };
        snapshot.epoch = "epoch-2".into();
        assert!(before.matching(&source).is_empty());
        let mut source = before.source.clone();
        let SourceObservation::Activity(snapshot) = &mut source.observation else { unreachable!() };
        snapshot.policy.running_after_ms += 1000;
        assert!(before.matching(&source).is_empty());
    }
    #[test]
    fn a_native_incarnation_survives_an_alias_change_but_a_different_database_cannot_inherit_state() {
        let before = baseline();
        let mut source = before.source.clone();
        let CollectionTarget::Database(database) = &mut source.target else { panic!("database fixture") };
        database.database = "renamed".into();
        assert_eq!(before.matching(&source).len(), 1);
        let CollectionTarget::Database(database) = &mut source.target else { panic!("database fixture") };
        database.incarnation = Some("different-native-database".into());
        assert!(before.matching(&source).is_empty());
    }
    #[test]
    fn incomplete_lock_details_keep_a_closed_reference_set_without_freshening_an_old_incident() {
        let baseline = baseline();
        let mut source = baseline.source.clone();
        let SourceObservation::Activity(snapshot) = &mut source.observation else { panic!() };
        snapshot.at = "2020-01-01T06:01:00Z".into();
        snapshot.uncertain = 1;
        let mut row = baseline.states[0].observation.clone();
        row.blocking_status = ActivityBlockingStatus::Unknown;
        let update =
            ActivityIncidentUpdate::observe(&source, &baseline.capture, &[row.clone()], &baseline.states).unwrap();
        let snapshot = source.observation.activity().unwrap();
        let mut records = ActivityRecords {
            observations: vec![row],
            checkpoints: update.active,
            changes: update.changes,
            ..Default::default()
        };
        assert!(records.coverage(snapshot).complete);
        assert_eq!(records.checkpoints[0].status, ActivityIncidentStatus::Uncertain);
        assert_eq!(records.checkpoints[0].last_seen_at, "2020-01-01T06:00:00Z");
        records.checkpoints[0].status = ActivityIncidentStatus::Current;
        assert!(!records.coverage(snapshot).complete, "stale proof cannot masquerade as a fresh observation");
    }
    #[test]
    fn a_node_scoped_poll_cannot_inherit_or_resolve_a_different_nodes_incidents() {
        let mut baseline = baseline();
        baseline.source.source = "ysql-activity-node-a".into();
        let SourceObservation::Activity(snapshot) = &mut baseline.source.observation else { panic!() };
        snapshot.scope = culprit_core::MeasurementScope::Node;
        snapshot.epoch = "node-a-epoch".into();
        let mut other = baseline.source.clone();
        other.source = "ysql-activity-node-b".into();
        let SourceObservation::Activity(snapshot) = &mut other.observation else { panic!() };
        snapshot.epoch = "node-b-epoch".into();
        assert!(baseline.matching(&other).is_empty());
        assert_eq!(baseline.matching(&baseline.source).len(), 1);
    }
}
