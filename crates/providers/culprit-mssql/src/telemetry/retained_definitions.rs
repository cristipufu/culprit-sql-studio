use super::streamed::{SnapshotAcquisition, SnapshotRecipe};
use crate::{read::NativeRead, retained_definitions, value::Rec};
use culprit_core::{Result, Source, telemetry::*};
pub(super) struct RetainedPlans;
const MAX_RETAINED_STATEMENTS: u32 = 64;
// A boxed/MI read uses three initial and three final authority projections, versus two and one on Azure SQL.
// Layout, two admission checks, the one payload stream and its binding check complete the twelve-call ceiling.
const MAX_SOURCE_QUERIES: u32 = 12;
const MAX_STATEMENT_MS: u32 = 1_000;
const MAX_SOURCE_MS: u32 = 5_000;
const MAX_PAGE_BYTES: u32 = 4 * 1024 * 1024;
const MAX_TOTAL_BYTES: u32 = 64 * 1024 * 1024;
const MAX_PLAN_BYTES: u32 = 2 * 1024 * 1024;

/// Provider ceilings are admission limits, not measured native cost or permission to run automatically.
pub(super) fn bounded_budget(caller: &CollectionBudget) -> CollectionBudget {
    caller.intersection(&CollectionBudget {
        page_rows: MAX_RETAINED_STATEMENTS,
        max_snapshot_rows: MAX_RETAINED_STATEMENTS,
        max_queries: MAX_SOURCE_QUERIES,
        statement_timeout_ms: MAX_STATEMENT_MS,
        total_timeout_ms: MAX_SOURCE_MS,
        page_bytes: MAX_PAGE_BYTES,
        total_bytes: MAX_TOTAL_BYTES,
        max_plan_bytes: MAX_PLAN_BYTES,
        max_sql_bytes: crate::actual::retained::MAX_STATEMENT_BYTES as u32,
        ..CollectionBudget::upper_limits()
    })
}
pub(super) struct Layout {
    native: retained_definitions::Layout,
    bindings: crate::relational_inputs::Bindings,
}
#[async_trait::async_trait]
impl SnapshotRecipe for RetainedPlans {
    type Layout = Layout;
    const ACQUISITION: SnapshotAcquisition = SnapshotAcquisition::ConnectedDatabaseCache;
    const DATASET: SnapshotDataset = SnapshotDataset::RetainedPlans;
    const NAME: &'static str = "retained-plans-state";
    const SOURCE: &'static Source = &crate::sources::RETAINED_PLANS;
    async fn layout<R: NativeRead>(&self, reader: &mut R, budget: &CollectionBudget) -> Result<Option<Self::Layout>> {
        Ok(retained_definitions::layout(reader, budget)
            .await?
            .map(|native| Layout { native, bindings: Default::default() }))
    }
    fn query(&self, layout: &Self::Layout) -> String {
        retained_definitions::select_sql(&layout.native)
    }
    fn decode(&self, row: &Rec, layout: &Self::Layout, source: &CollectionDescription) -> Result<TelemetryRecord> {
        let admitted = layout.bindings.contains(&crate::cached_statements::context(row)?);
        let mut definition = retained_definitions::decode(row, &layout.native, admitted.then_some(source))?;
        if !admitted {
            definition.batch_plan.payload = DefinitionPayload::Unavailable { reason: PayloadUnavailable::NotCollected };
            definition.module_parameters = None;
            definition.current_type_schema = None;
            definition.relational_input_proof = None;
            definition.parameter_context_proof = None;
        }
        let record = ObjectDefinitionRecord::new(
            Self::SOURCE.name.into(),
            ObjectDefinitionFact::MssqlRetainedPlan(Box::new(definition)),
        )
        .inspect_err(|_error| {
            #[cfg(test)]
            retained_definitions::diagnostics::report("record", row, Some(source), None, _error);
        })?;
        Ok(TelemetryRecord::ObjectDefinition(record))
    }
    fn complete_record_scope(&self, record: &TelemetryRecord) -> bool {
        matches!(record,TelemetryRecord::ObjectDefinition(ObjectDefinitionRecord {fact:ObjectDefinitionFact::MssqlRetainedPlan(definition),..}) if definition.context.identity.has_plan_handle() && !matches!(definition.batch_plan.payload,DefinitionPayload::Unavailable{reason:PayloadUnavailable::NotCollected}))
    }
    fn observe_batch(&self, rows: &[Rec], layout: &mut Self::Layout) -> Result<()> {
        layout.bindings.observe(rows)
    }
    async fn recheck<R: NativeRead>(&self, reader: &mut R, layout: &Self::Layout) -> Result<()> {
        layout.bindings.require_verified(reader).await
    }
}
