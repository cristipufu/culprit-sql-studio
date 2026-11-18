use super::streamed::{SnapshotAcquisition, SnapshotRecipe};
use crate::{
    index_operations::{Layout, decode, select_sql},
    read::NativeRead,
    value::Rec,
};
use culprit_core::{Result, Source, telemetry::*};
pub(super) struct IndexOperations;
#[async_trait::async_trait]
impl SnapshotRecipe for IndexOperations {
    type Layout = Layout;
    const DATASET: SnapshotDataset = SnapshotDataset::IndexOperations;
    const NAME: &'static str = "index-operational-state";
    const SOURCE: &'static Source = &crate::sources::INDEX_OPERATIONS;
    const ACQUISITION: SnapshotAcquisition = SnapshotAcquisition::AzureDatabase;
    async fn layout<R: NativeRead>(&self, reader: &mut R, _budget: &CollectionBudget) -> Result<Option<Layout>> {
        Layout::read(reader).await.map(Some)
    }
    fn query(&self, layout: &Layout) -> String {
        select_sql(layout)
    }
    fn decode(&self, row: &Rec, layout: &Layout, _source: &CollectionDescription) -> Result<TelemetryRecord> {
        let index = decode(row, layout)?;
        Ok(TelemetryRecord::Operational(OperationalObservation {
            source: Self::SOURCE.name.into(),
            key: index.identity.key()?,
            fact: OperationalFact::MssqlIndexOperations(Box::new(index)),
        }))
    }
}
