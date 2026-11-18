use super::*;

pub(super) fn explain(snapshot: &mut Snapshot) -> Option<Box<culprit_core::PerformanceCounterReport>> {
    match crate::performance_counters::explain_snapshot(snapshot) {
        Ok(report) => report.map(Box::new),
        Err(_) => admit_explanation(
            snapshot,
            culprit_core::source_id::PERFORMANCE_COUNTERS.into(),
            culprit_core::message!("view.performance_counters.invalid"),
            Err(culprit_core::telemetry::CollectionStop::InvalidPage.error()),
        ),
    }
}
