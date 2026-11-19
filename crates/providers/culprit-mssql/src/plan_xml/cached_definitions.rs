//! Pure native-definition adapters share statement selection and kind qualification with interactive reads.
use culprit_core::{Plan, Result, telemetry::*};

pub fn parse_cached_definition(definition: &MssqlCachedDefinition, source: &CollectionDescription) -> Result<Plan> {
    if !definition.native_plan_proofs_match(source) {
        return Err(DeliveryProblem::Integrity.error());
    }
    require_source(source, SnapshotDataset::CachedDefinitions)?;
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
    let mut plan = super::parse_cached_statement(
        xml,
        super::CachedInputContext {
            statement: &definition.context,
            module_parameters: definition.module_parameters.as_ref(),
            text: statement,
            origin: super::PlanInputOrigin::StoredRedacted { proof: definition.relational_input_proof.as_ref() },
        },
    )?;
    plan.facts_mut().current_type_schema = definition.current_type_schema.clone().map(Box::new);
    Ok(plan)
}

pub fn parse_retained_definition(
    definition: &MssqlRetainedPlanDefinition,
    source: &CollectionDescription,
) -> Result<Plan> {
    if !definition.native_plan_proofs_match(source) {
        return Err(DeliveryProblem::Integrity.error());
    }
    require_source(source, SnapshotDataset::RetainedPlans)?;
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
            origin: super::PlanInputOrigin::StoredRedacted { proof: definition.relational_input_proof.as_ref() },
        },
    )?;
    if let Some(proof) = &definition.parameter_context_proof {
        super::parameters::restore(&mut plan, &proof.context)?;
    }
    plan.facts_mut().current_type_schema = definition.current_type_schema.clone().map(Box::new);
    crate::actual::clear_unrecorded_times(&mut plan);
    Ok(plan)
}

fn require_source(source: &CollectionDescription, dataset: SnapshotDataset) -> Result<()> {
    let matching = match &source.observation {
        SourceObservation::Snapshot(snapshot) => snapshot.dataset == dataset,
        SourceObservation::PagedSnapshot(snapshot) => snapshot.dataset == dataset,
        _ => false,
    };
    if !matching
        || source.target.engine() != culprit_core::Engine::Mssql
        || source.target.database().is_none()
        || !source.partition.is_valid_for(&source.target)
    {
        return Err(crate::read::invalid_data());
    }
    Ok(())
}

#[cfg(test)]
mod tests;
