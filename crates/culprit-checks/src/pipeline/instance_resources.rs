mod cpu_originals;
use super::*;

pub(super) fn explain(snapshot: &mut Snapshot) -> Option<Box<culprit_core::InstanceResourceReport>> {
    let current = match crate::instance_resources::explain_snapshot(snapshot) {
        Ok(report) => report.map(Box::new),
        Err(error) => admit_explanation(
            snapshot,
            culprit_core::telemetry::INSTANCE_RESOURCES_SOURCE.into(),
            culprit_core::message!("view.instance_resources.invalid"),
            Err(error),
        ),
    };
    cpu_originals::attach(snapshot, current)
}
