use super::streamed::SnapshotRecipe;
use crate::{read::NativeRead, triggers, value::Rec};
use culprit_core::{Result, Source, telemetry::*};
pub(super) struct Triggers;
#[async_trait::async_trait]
impl SnapshotRecipe for Triggers {
    type Layout = ();
    const DATASET: SnapshotDataset = SnapshotDataset::Triggers;
    const NAME: &'static str = "trigger-state";
    const SOURCE: &'static Source = &crate::sources::TRIGGERS;
    const REQUIRES_FULL_METADATA: bool = true;
    async fn layout<R: NativeRead>(&self, _: &mut R, _: &CollectionBudget) -> Result<Option<()>> {
        Ok(Some(()))
    }
    fn query(&self, _: &()) -> String {
        triggers::select_sql(triggers::Selection::AllDml)
    }
    fn decode(&self, row: &Rec, _: &()) -> Result<TelemetryRecord> {
        Ok(TelemetryRecord::ObjectDefinition(ObjectDefinitionRecord::new(
            Self::SOURCE.name.into(),
            ObjectDefinitionFact::MssqlDmlTrigger(Box::new(triggers::decode(row)?)),
        )?))
    }
    fn complete_record_scope(&self, record: &TelemetryRecord) -> bool {
        matches!(record,TelemetryRecord::ObjectDefinition(ObjectDefinitionRecord {fact:ObjectDefinitionFact::MssqlDmlTrigger(trigger),..}) if trigger.event_metadata_complete())
    }
}
