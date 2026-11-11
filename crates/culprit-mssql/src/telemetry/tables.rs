use super::streamed::SnapshotRecipe;
use crate::{read::NativeRead, tables, value::Rec};
use culprit_core::{Result, Source, telemetry::*};
pub(super) struct Tables;
#[async_trait::async_trait]
impl SnapshotRecipe for Tables {
    type Layout = tables::Layout;
    const DATASET: SnapshotDataset = SnapshotDataset::Tables;
    const NAME: &'static str = "table-state";
    const SOURCE: &'static Source = &crate::sources::TABLES;
    const REQUIRES_FULL_METADATA: bool = true;
    async fn layout<R: NativeRead>(&self, _reader: &mut R, _budget: &CollectionBudget) -> Result<Option<Self::Layout>> {
        Ok(Some(tables::Layout::new(true)))
    }
    fn query(&self, layout: &Self::Layout) -> String {
        tables::select_sql(layout)
    }
    fn decode(&self, row: &Rec, layout: &Self::Layout) -> Result<TelemetryRecord> {
        let table = tables::decode(row, layout)?;
        Ok(TelemetryRecord::Operational(OperationalObservation {
            source: Self::SOURCE.name.into(),
            key: table.identity.key()?,
            fact: OperationalFact::MssqlTable(Box::new(table)),
        }))
    }
}
