//! Original scalar facts are content-bound to one selected retained statement, never to a cache execution count.
use super::{binding::*, *};
use crate::plan_facts::{PlanParameterContext, PlanParameterIssue};

pub const MAX_MSSQL_PARAMETER_CONTEXT_BYTES: usize = 16 * 1024;
const MAX_PARAMETER_FACT_BYTES: usize = 8 * 1024;
const SCOPE_DOMAIN: &str = "mssql-parameter-context-scope-v1";

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[ts(export)]
/// Hashes bind source-owned facts to native content. The authenticated row supplies provenance; these fields
/// establish no execution identity, reuse, completion, timing or counterfactual benefit.
pub struct MssqlParameterContextProof {
    pub source_scope: String,
    pub input_sha256: String,
    pub context: PlanParameterContext,
}

impl MssqlParameterContextProof {
    pub fn supports_source(source: &CollectionDescription) -> bool {
        definition_dataset(source) == Some(SnapshotDataset::RetainedPlans)
            && definition_scope(source, SCOPE_DOMAIN).is_ok()
    }

    pub fn for_retained(
        source: &CollectionDescription,
        definition: &MssqlRetainedPlanDefinition,
        mut context: PlanParameterContext,
    ) -> Result<Self> {
        if !Self::supports_source(source) || !definition.is_valid() || !context.is_valid() {
            return Err(invalid());
        }
        crate::redact::redact_parameter_context(&mut context, crate::Engine::Mssql);
        if crate::serialization::json_size(&context, MAX_PARAMETER_FACT_BYTES).is_err() {
            context = PlanParameterContext {
                parameters: vec![],
                uses: vec![],
                issues: vec![PlanParameterIssue::MetadataLimit],
            };
        }
        let source_scope = definition_scope(source, SCOPE_DOMAIN)?;
        let value = Self { input_sha256: input(&source_scope, definition, &context)?, source_scope, context };
        if !value.matches_retained(source, definition) {
            return Err(invalid());
        }
        Ok(value)
    }

    pub fn is_valid(&self) -> bool {
        valid_digest(&self.source_scope)
            && valid_digest(&self.input_sha256)
            && self.context.is_valid()
            && crate::serialization::json_size(self, MAX_MSSQL_PARAMETER_CONTEXT_BYTES).is_ok()
            && {
                let mut safe = self.context.clone();
                crate::redact::redact_parameter_context(&mut safe, crate::Engine::Mssql);
                safe == self.context
            }
    }

    pub fn matches_retained(&self, source: &CollectionDescription, definition: &MssqlRetainedPlanDefinition) -> bool {
        Self::supports_source(source)
            && definition_scope(source, SCOPE_DOMAIN).is_ok_and(|scope| scope == self.source_scope)
            && self.matches_retained_contents(definition)
    }

    pub(super) fn matches_retained_contents(&self, definition: &MssqlRetainedPlanDefinition) -> bool {
        self.is_valid()
            && definition.batch_plan.is_valid()
            && definition.batch_plan.payload.is_available()
            && definition.batch_plan.native_sha256.is_some()
            && input(&self.source_scope, definition, &self.context).is_ok_and(|expected| expected == self.input_sha256)
    }
}

fn input(scope: &str, value: &MssqlRetainedPlanDefinition, context: &PlanParameterContext) -> Result<String> {
    serde_json::to_vec(&(
        "mssql-retained-parameter-context-v1",
        scope,
        &value.context,
        &value.module_parameters,
        PayloadBinding::from(&value.statement),
        PayloadBinding::from(&value.batch_plan),
        context,
    ))
    .map(|bytes| digest(&bytes))
    .map_err(|_| invalid())
}

fn invalid() -> crate::Error {
    CollectionStop::InvalidPage.error()
}

#[cfg(test)]
mod tests;
