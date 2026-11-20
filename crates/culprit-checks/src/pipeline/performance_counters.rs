use super::*;
use culprit_core::{PerformanceCounterPopulation, PerformanceCounterReport, telemetry::RecordedPerformanceCounters};

pub(super) fn explain(snapshot: &mut Snapshot) -> Vec<PerformanceCounterReport> {
    let shared = match crate::performance_counters::explain_snapshot(snapshot) {
        Ok(report) => report,
        Err(_) => {
            let reason = culprit_core::message!("view.performance_counters.invalid");
            snapshot.record_unavailable_evidence(culprit_core::source_id::PERFORMANCE_COUNTERS, reason.clone());
            Some(crate::performance_counters::unavailable(PerformanceCounterPopulation::SharedServer, &reason))
        }
    };
    let mut reports = shared.into_iter().collect::<Vec<_>>();
    if let Some(recorded) = snapshot.operations.database_performance_counters.as_mut() {
        let report = match crate::performance_counters::explain_recorded(recorded) {
            Ok(report) => report,
            Err(_) => {
                // A foreign/edited original cannot change the separate shared source outcome.
                let reason = culprit_core::message!("view.performance_counters.invalid");
                let report =
                    crate::performance_counters::unavailable(PerformanceCounterPopulation::SelectedDatabase, &reason);
                *recorded = RecordedPerformanceCounters::Unavailable { reason };
                report
            }
        };
        reports.push(report);
    }
    reports
}
