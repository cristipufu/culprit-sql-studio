use super::streamed::SnapshotRecipe;
use crate::{read::NativeRead, recorded, value::Rec};
use culprit_core::{Result, Source, telemetry::*};
pub(super) struct Objects;
#[async_trait::async_trait]
impl SnapshotRecipe for Objects {
    type Layout = ();
    const DATASET: SnapshotDataset = SnapshotDataset::Objects;
    const NAME: &'static str = "object-metadata-state";
    const SOURCE: &'static Source = &crate::sources::OBJECTS;
    const REQUIRES_FULL_METADATA: bool = true;
    async fn layout<R: NativeRead>(&self, _reader: &mut R, _budget: &CollectionBudget) -> Result<Option<()>> {
        Ok(Some(()))
    }
    fn query(&self, _: &()) -> String {
        recorded::select_sql(false)
    }
    fn decode(&self, row: &Rec, _: &()) -> Result<TelemetryRecord> {
        Ok(TelemetryRecord::ObjectDefinition(ObjectDefinitionRecord::new(
            Self::SOURCE.name.into(),
            ObjectDefinitionFact::MssqlObject(Box::new(recorded::decode(row)?)),
        )?))
    }
}
