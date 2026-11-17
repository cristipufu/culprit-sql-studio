use super::*;

pub(super) fn explain(snapshot: &mut Snapshot) -> Option<Box<culprit_core::CounterRatioReport>> {
    let samples = snapshot.pressure.as_ref()?.resource_counters.as_ref()?;
    let samples = samples.iter().filter(|sample| !sample.ratios.is_empty()).cloned().collect::<Vec<_>>();
    if samples.is_empty() {
        return None;
    }
    let source = snapshot.sources.iter().find(|source| source.source == culprit_core::source_id::PERFORMANCE_COUNTERS);
    let result = crate::counter_ratios::explain(&samples, source, true).map(Box::new);
    admit_explanation(
        snapshot,
        culprit_core::source_id::PERFORMANCE_COUNTERS.into(),
        culprit_core::message!("view.counter_ratios.invalid"),
        result,
    )
}
