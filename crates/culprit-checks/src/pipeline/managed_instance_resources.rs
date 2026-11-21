//! Saved native history is replayed only from its original typed carrier, never the current pressure point.
use culprit_core::{ManagedInstanceResourceReport, Snapshot, telemetry::RecordedManagedInstanceResourceRead};
pub(super) fn explain(snapshot: &Snapshot) -> Option<Box<ManagedInstanceResourceReport>> {
    snapshot.operations.managed_instance_resources.as_deref().map(|original| {
        Box::new(match original {
            RecordedManagedInstanceResourceRead::Captured { read } => {
                crate::managed_instance_resources::explain_captured(read).unwrap_or_else(|_| {
                    crate::managed_instance_resources::unavailable(&culprit_core::message!("view.mi_resources.invalid"))
                })
            }
            RecordedManagedInstanceResourceRead::Unavailable { reason } => {
                crate::managed_instance_resources::unavailable(reason)
            }
        })
    })
}
