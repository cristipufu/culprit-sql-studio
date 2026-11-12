use super::streamed::SnapshotRecipe;
use crate::{read::NativeRead, retained_definitions, value::Rec};
use culprit_core::{Result, Source, telemetry::*};
pub(super) struct RetainedPlans;
#[async_trait::async_trait]
impl SnapshotRecipe for RetainedPlans {
    type Layout = retained_definitions::Layout;
    const DATASET: SnapshotDataset = SnapshotDataset::RetainedPlans;
    const NAME: &'static str = "retained-plans-state";
    const SOURCE: &'static Source = &crate::sources::RETAINED_PLANS;
    async fn layout<R: NativeRead>(&self, reader: &mut R, budget: &CollectionBudget) -> Result<Option<Self::Layout>> {
        retained_definitions::layout(reader, budget).await
    }
    fn query(&self, layout: &Self::Layout) -> String {
        retained_definitions::select_sql(layout)
    }
    fn decode(&self, row: &Rec, layout: &Self::Layout) -> Result<TelemetryRecord> {
        Ok(TelemetryRecord::ObjectDefinition(ObjectDefinitionRecord::new(
            Self::SOURCE.name.into(),
            ObjectDefinitionFact::MssqlRetainedPlan(Box::new(retained_definitions::decode(row, layout)?)),
        )?))
    }
    fn complete_record_scope(&self, record: &TelemetryRecord) -> bool {
        matches!(record,TelemetryRecord::ObjectDefinition(ObjectDefinitionRecord {fact:ObjectDefinitionFact::MssqlRetainedPlan(definition),..}) if definition.context.identity.has_plan_handle())
    }
}
