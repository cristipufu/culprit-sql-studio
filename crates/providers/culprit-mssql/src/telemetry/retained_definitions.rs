use super::streamed::SnapshotRecipe;
use crate::{read::NativeRead, retained_definitions, value::Rec};
use culprit_core::{Result, Source, telemetry::*};
pub(super) struct RetainedPlans;
pub(super) struct Layout {
    native: retained_definitions::Layout,
    bindings: crate::relational_inputs::Bindings,
}
#[async_trait::async_trait]
impl SnapshotRecipe for RetainedPlans {
    type Layout = Layout;
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
    fn decode(&self, row: &Rec, layout: &Self::Layout) -> Result<TelemetryRecord> {
        let mut definition = retained_definitions::decode(row, &layout.native)?;
        if !layout.bindings.contains(&definition.context) {
            definition.batch_plan.payload = DefinitionPayload::Unavailable { reason: PayloadUnavailable::NotCollected };
        }
        Ok(TelemetryRecord::ObjectDefinition(ObjectDefinitionRecord::new(
            Self::SOURCE.name.into(),
            ObjectDefinitionFact::MssqlRetainedPlan(Box::new(definition)),
        )?))
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
