//! Pure native-definition adapters share statement selection and kind qualification with interactive reads.
use culprit_core::{Plan, Result, telemetry::*};

pub fn parse_cached_definition(definition: &MssqlCachedDefinition) -> Result<Plan> {
    if !definition.is_valid() {
        return Err(crate::read::invalid_data());
    }
    let DefinitionPayload::Available { text: xml, .. } = &definition.estimated_plan.payload else {
        return Err(crate::read::invalid_data());
    };
    let statement = match &definition.statement.payload {
        DefinitionPayload::Available { text, .. } => Some(text.as_str()),
        DefinitionPayload::Unavailable { .. } => None,
    };
    super::parse_cached_statement(
        xml,
        super::CachedInputContext {
            statement: &definition.context,
            module_parameters: definition.module_parameters.as_ref(),
            text: statement,
            origin: super::PlanInputOrigin::StoredRedacted,
        },
    )
}

pub fn parse_retained_definition(definition: &MssqlRetainedPlanDefinition) -> Result<Plan> {
    if !definition.is_valid() {
        return Err(crate::read::invalid_data());
    }
    let DefinitionPayload::Available { text: xml, .. } = &definition.batch_plan.payload else {
        return Err(crate::read::invalid_data());
    };
    let statement = match &definition.statement.payload {
        DefinitionPayload::Available { text, .. } => Some(text.as_str()),
        DefinitionPayload::Unavailable { .. } => None,
    };
    let mut plan = super::parse_cached_statement(
        xml,
        super::CachedInputContext {
            statement: &definition.context,
            module_parameters: definition.module_parameters.as_ref(),
            text: statement,
            origin: super::PlanInputOrigin::StoredRedacted,
        },
    )?;
    crate::actual::clear_unrecorded_times(&mut plan);
    Ok(plan)
}

#[cfg(test)]
mod tests;
