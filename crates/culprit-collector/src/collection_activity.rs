//! Closure of one immutable activity baseline. Selection and episode policy run only after its Ready commit.
use culprit_core::telemetry::*;

#[derive(Default)]
pub(crate) struct ActivityRecords {
    pub observations: Vec<ActivityObservation>,
}
impl ActivityRecords {
    pub fn record(&mut self, record: &TelemetryRecord) {
        if let TelemetryRecord::Activity(row) = record {
            self.observations.push(row.as_ref().clone());
        }
    }
    pub fn coverage(&self, snapshot: &ActivitySnapshotDescription) -> ActivityReferenceCoverage {
        ActivityReferenceCoverage {
            observations: self.observations.len() as u32,
            complete: snapshot.is_valid()
                && self.observations.len() == snapshot.observations as usize
                && self.observations.iter().all(ActivityObservation::is_valid)
                && ActivityEvidence::validate(&self.observations).is_ok(),
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn row() -> ActivityObservation {
        serde_json::from_value(serde_json::json!({"key":{"session":{"id":"61","generation":"login"},"request":{"id":"0","generation":"request"}},"blocking_status":"unknown","idle":false,"scope":"request-database","state":"running","open_transactions":0,"elapsed_ms":"120000","session_counters":{},"request_counters":{},"statement":{"state":"unavailable","reason":"not-collected"}})).unwrap()
    }
    fn snapshot(records: u32, complete: bool) -> ActivitySnapshotDescription {
        ActivitySnapshotDescription {
            epoch: Some("native".into()),
            observed_from: "2026-11-14T08:00:00Z".into(),
            observed_until: "2026-11-14T08:00:01Z".into(),
            at: "2026-11-14T08:00:00Z".into(),
            scope: culprit_core::MeasurementScope::Database,
            population: ActivityPopulation::ActiveRequestsOpenTransactionsAndBlockers,
            native_rows: records,
            observations: records,
            coverage: StateSourceCoverage::observed("activity".into(), records, complete),
        }
    }
    #[test]
    fn partial_native_visibility_and_unknown_lock_details_do_not_discard_positive_baseline_rows() {
        let records = ActivityRecords { observations: vec![row()] };
        assert!(records.coverage(&snapshot(1, false)).complete);
        assert!(records.coverage(&snapshot(1, true)).complete);
    }
    #[test]
    fn duplicate_generation_or_missing_rows_cannot_close_a_baseline() {
        let records = ActivityRecords { observations: vec![row(), row()] };
        assert!(!records.coverage(&snapshot(2, true)).complete);
        assert!(!ActivityRecords { observations: vec![row()] }.coverage(&snapshot(2, true)).complete);
        assert!(ActivityRecords::default().coverage(&snapshot(0, true)).complete);
    }
}
