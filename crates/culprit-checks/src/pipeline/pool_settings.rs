//! A current scan supplies no replacement pool membership or values for an independently saved original.
use culprit_core::{PoolSettingsReport, Snapshot, telemetry::RecordedPoolSettingsRead};
pub(super) fn explain(snapshot: &Snapshot) -> Option<Box<PoolSettingsReport>> {
    let original = snapshot.operations.pool_settings.as_deref()?;
    Some(Box::new(match original {
        RecordedPoolSettingsRead::Captured { read } => {
            crate::pool_settings::explain_captured(read).unwrap_or_else(|_| {
                crate::pool_settings::unavailable(&culprit_core::message!("view.pool_settings.invalid"))
            })
        }
        RecordedPoolSettingsRead::Unavailable { reason } => crate::pool_settings::unavailable(reason),
    }))
}
