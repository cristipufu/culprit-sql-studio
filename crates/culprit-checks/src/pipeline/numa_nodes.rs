//! Independent original replay through the existing full-population identity/conflict policy.
use super::*;
pub(super) fn explain(snapshot: &Snapshot) -> Option<Vec<culprit_core::InstanceResourceReport>> {
    super::recorded_operational::explain(
        &snapshot.operations.numa_nodes,
        crate::instance_resources::explain_numa_recorded,
        crate::instance_resources::unavailable_numa_nodes,
        culprit_core::message!("view.numa_nodes.invalid"),
        |shown, total| {
            culprit_core::message!(
                "view.numa_nodes.limited",
                shown = culprit_core::format::digits(shown.to_string()),
                total = culprit_core::format::digits(total.to_string())
            )
        },
    )
}
