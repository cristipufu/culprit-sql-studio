use culprit_core::{InstanceResourceReport, Snapshot, telemetry::*};
pub(super) fn attach(
    snapshot: &Snapshot,
    current: Option<Box<InstanceResourceReport>>,
) -> Option<Box<InstanceResourceReport>> {
    let originals = &snapshot.operations.process_cpu_originals;
    let positions = RecordedOperationalRead::replay_positions(originals);
    let selected = positions
        .iter()
        .filter_map(|position| position.as_ref().ok().map(|index| &originals[*index]))
        .collect::<Vec<_>>();
    let mut interval = crate::instance_resources::unavailable_cpu_interval(
        crate::instance_resources::CpuIntervalUnavailableReason::Selection,
    );
    // Exactly two independent originals is deliberate selection, never a plausible surviving/latest pair.
    if positions.len() == 2 && positions.iter().all(Result::is_ok) {
        if let [RecordedOperationalRead::Captured { read: first }, RecordedOperationalRead::Captured { read: second }] =
            selected.as_slice()
        {
            interval = crate::instance_resources::compare_cpu_originals(first, second).unwrap_or_else(|_| {
                crate::instance_resources::unavailable_cpu_interval(
                    crate::instance_resources::CpuIntervalUnavailableReason::Incomplete,
                )
            });
        }
    }
    let reports = super::super::recorded_operational::explain_positions(
        originals,
        positions,
        crate::instance_resources::explain_cpu_recorded,
        crate::instance_resources::unavailable_cpu_original,
        culprit_core::message!("view.process_cpu.invalid"),
        |shown, total| culprit_core::message!("view.process_cpu.limited", shown = shown, total = total),
    );
    match reports {
        Some(reports) => Some(crate::instance_resources::with_cpu_originals(current, reports, interval)),
        None => current,
    }
}
