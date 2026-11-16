//! Retained native progress uses its own coverage and cannot acquire a workload or another database's identity.
use super::*;

pub(super) fn explain(snapshot: &mut Snapshot) -> Option<culprit_core::MaintenanceProgressReport> {
    let captured = snapshot.operations.maintenance_progress.as_ref()?;
    let source_id = captured.source.clone();
    let result = if captured.context.target.engine() == snapshot.server.engine
        && captured.context.target.database().is_some_and(|database| database.database == snapshot.server.database)
    {
        crate::maintenance_progress::explain(captured, true)
    } else {
        Err(culprit_core::Error::invalid(t!("view.maintenance_progress.invalid")))
    };
    admit_explanation(snapshot, source_id, culprit_core::message!("view.maintenance_progress.invalid"), result)
}
