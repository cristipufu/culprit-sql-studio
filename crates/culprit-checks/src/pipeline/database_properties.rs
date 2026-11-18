use super::*;

pub(super) fn explain(snapshot: &mut Snapshot) -> Option<Box<culprit_core::DatabasePropertiesReport>> {
    match crate::database_properties::explain_snapshot(snapshot) {
        Ok(report) => report.map(Box::new),
        Err(error) => admit_explanation(
            snapshot,
            culprit_core::telemetry::DATABASE_PROPERTIES_SOURCE.into(),
            culprit_core::message!("view.database_properties.invalid"),
            Err(error),
        ),
    }
}
