//! A current, source-qualified statement can establish maintenance syntax. Generic command labels cannot.
use super::{MaintenanceClassification, MaintenanceClassificationIssue};
use crate::{
    Engine,
    telemetry::{ActivityObservation, DefinitionPayload},
};

pub const MAX_MAINTENANCE_STATEMENT_BYTES: usize = 64 * 1024;

pub fn classify_maintenance(engine: Engine, observation: &ActivityObservation) -> MaintenanceClassification {
    use MaintenanceClassification::{Maintenance, OtherStatement, Unresolved};
    use MaintenanceClassificationIssue::*;
    if !observation.is_valid() {
        return Unresolved { reason: InvalidObservation };
    }
    if observation.idle || observation.key.request.is_none() {
        return Unresolved { reason: NoCurrentRequest };
    }
    if observation.statement_identity.is_none() {
        return Unresolved { reason: StatementIdentityUnavailable };
    }
    let DefinitionPayload::Available { text, .. } = &observation.statement else {
        return Unresolved { reason: StatementUnavailable };
    };
    if text.len() > MAX_MAINTENANCE_STATEMENT_BYTES {
        return Unresolved { reason: StatementTooLarge };
    }
    match crate::sql::maintenance_statement(text, engine) {
        Ok(Some(kind)) => Maintenance { kind },
        Ok(None) => OtherStatement,
        Err(_) => Unresolved { reason: InvalidStatement },
    }
}

#[cfg(test)]
mod tests;
