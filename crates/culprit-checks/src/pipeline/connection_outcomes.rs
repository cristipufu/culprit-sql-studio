use super::*;

pub(super) fn explain(snapshot: &mut Snapshot) -> Option<Box<culprit_core::ConnectionOutcomeReport>> {
    snapshot.pressure.as_ref()?.connection_outcomes.as_ref()?;
    let result = crate::connection_outcomes::explain_snapshot(snapshot).and_then(|report| {
        report.map(Box::new).ok_or_else(|| culprit_core::Error::invalid(t!("view.connection_outcomes.invalid")))
    });
    admit_explanation(
        snapshot,
        culprit_core::source_id::SESSIONS.into(),
        culprit_core::message!("view.connection_outcomes.invalid"),
        result,
    )
}

#[cfg(test)]
mod tests;
