use super::streamed::SnapshotRecipe;
use crate::{cached_definitions, read::NativeRead, value::Rec};
use culprit_core::{Result, Source, telemetry::*};
pub(super) struct CachedDefinitions;
#[async_trait::async_trait]
impl SnapshotRecipe for CachedDefinitions {
    type Layout = cached_definitions::Limits;
    const DATASET: SnapshotDataset = SnapshotDataset::CachedDefinitions;
    const NAME: &'static str = "cached-definitions-state";
    const SOURCE: &'static Source = &crate::sources::PLAN_CACHE;
    async fn layout<R: NativeRead>(&self, _: &mut R, budget: &CollectionBudget) -> Result<Option<Self::Layout>> {
        Ok(Some(cached_definitions::Limits::from_budget(budget)?))
    }
    fn query(&self, layout: &Self::Layout) -> String {
        cached_definitions::select_sql(layout)
    }
    fn decode(&self, row: &Rec, layout: &Self::Layout) -> Result<TelemetryRecord> {
        Ok(TelemetryRecord::ObjectDefinition(ObjectDefinitionRecord::new(
            Self::SOURCE.name.into(),
            ObjectDefinitionFact::MssqlCachedDefinition(Box::new(cached_definitions::decode(row, layout)?)),
        )?))
    }
    fn complete_record_scope(&self, record: &TelemetryRecord) -> bool {
        matches!(record,TelemetryRecord::ObjectDefinition(ObjectDefinitionRecord {fact:ObjectDefinitionFact::MssqlCachedDefinition(definition),..}) if definition.context.identity.has_plan_handle())
    }
}
