//! Reuse complete, incarnation-qualified definitions already read for the workload. Never read native text again.
use super::Compilation;
use crate::query_store::{definitions::Definition, payload::Payload};
use culprit_core::telemetry::{DefinitionPayload, NativeQueryIdentity, PayloadUnavailable, StatementOrigin};

const MAX_RETAINED_STATEMENT_BYTES: usize = 16 * 1024;

#[derive(Clone, PartialEq, Eq, serde::Serialize)]
pub(crate) struct CompilationStatement {
    pub identity: NativeQueryIdentity,
    pub statement: DefinitionPayload,
    pub origin: StatementOrigin,
}

impl CompilationStatement {
    pub fn from_definition(definition: &Definition) -> Option<Self> {
        let identity = definition.query.clone()?;
        if definition.plan.is_some() || identity.query_id != definition.id {
            return None;
        }
        let statement = match &definition.payload {
            Payload::Text(text) if text.len() <= MAX_RETAINED_STATEMENT_BYTES => {
                DefinitionPayload::sql(text, culprit_core::Engine::Mssql)
            }
            Payload::Text(_) => DefinitionPayload::Unavailable { reason: PayloadUnavailable::Oversized },
            Payload::Unavailable(reason) => DefinitionPayload::Unavailable { reason: *reason },
        };
        let statement = match &statement {
            DefinitionPayload::Available { text, .. } if text.len() > MAX_RETAINED_STATEMENT_BYTES => {
                DefinitionPayload::Unavailable { reason: PayloadUnavailable::Oversized }
            }
            _ => statement,
        };
        Some(Self { identity, statement, origin: definition.origin })
    }
}

/// Missing optional definitions withhold policy without discarding compilation counters. Conflicting original
/// definitions cannot choose a representative even when repeated population copies share a numeric query ID.
pub(crate) fn bind_statement<'a>(
    compilation: &mut Compilation,
    definitions: impl IntoIterator<Item = Option<&'a CompilationStatement>>,
) -> bool {
    compilation.statement = DefinitionPayload::Unavailable { reason: PayloadUnavailable::NotCollected };
    compilation.origin = StatementOrigin::Unknown;
    let mut agreed: Option<&CompilationStatement> = None;
    let mut missing = false;
    let mut conflicting = false;
    for definition in definitions {
        match definition {
            None => missing = true,
            Some(definition) if definition.identity != compilation.identity => conflicting = true,
            Some(definition) => match agreed {
                Some(previous) if previous != definition => conflicting = true,
                None => agreed = Some(definition),
                _ => (),
            },
        }
    }
    if !missing
        && !conflicting
        && let Some(definition) = agreed
    {
        compilation.statement = definition.statement.clone();
        compilation.origin = definition.origin;
    }
    !conflicting
}

#[cfg(test)]
mod tests;
