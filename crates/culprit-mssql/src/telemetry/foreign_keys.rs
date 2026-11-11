use super::streamed::SnapshotRecipe;
use crate::{foreign_keys, read::NativeRead, value::Rec};
use culprit_core::{Result, Source, telemetry::*};
pub(super) struct ForeignKeys;
#[async_trait::async_trait]
impl SnapshotRecipe for ForeignKeys {
    type Layout = ();
    const DATASET: SnapshotDataset = SnapshotDataset::ForeignKeys;
    const NAME: &'static str = "foreign-key-state";
    const SOURCE: &'static Source = &crate::sources::FOREIGN_KEYS;
    const REQUIRES_FULL_METADATA: bool = true;
    async fn layout<R: NativeRead>(&self, _reader: &mut R, _budget: &CollectionBudget) -> Result<Option<()>> {
        Ok(Some(()))
    }
    fn query(&self, _: &()) -> String {
        foreign_keys::select_sql()
    }
    fn decode(&self, row: &Rec, _: &()) -> Result<TelemetryRecord> {
        Ok(TelemetryRecord::ObjectDefinition(ObjectDefinitionRecord::new(
            Self::SOURCE.name.into(),
            ObjectDefinitionFact::MssqlForeignKey(Box::new(foreign_keys::decode(row)?)),
        )?))
    }
}
