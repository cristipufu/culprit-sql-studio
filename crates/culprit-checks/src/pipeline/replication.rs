use super::*;

pub(super) fn explain(snapshot: &mut Snapshot) -> Option<Box<culprit_core::ReplicationReport>> {
    let captured = snapshot.pressure.as_ref()?.replication.as_ref()?;
    let result = crate::replication::explain(captured, true).map(Box::new);
    admit_explanation(
        snapshot,
        culprit_core::source_id::REPLICATION.into(),
        culprit_core::message!("view.replication.invalid"),
        result,
    )
}
