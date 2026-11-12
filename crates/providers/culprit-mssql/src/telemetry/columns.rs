use super::streamed::SnapshotRecipe;
use crate::{columns, read::NativeRead, value::Rec};
use culprit_core::{Result, Source, telemetry::*};
pub(super) struct Columns;
#[async_trait::async_trait]
impl SnapshotRecipe for Columns {
    type Layout = usize;
    const DATASET: SnapshotDataset = SnapshotDataset::Columns;
    const NAME: &'static str = "column-state";
    const SOURCE: &'static Source = &crate::sources::COLUMNS;
    const REQUIRES_FULL_METADATA: bool = true;
    async fn layout<R: NativeRead>(&self, _reader: &mut R, budget: &CollectionBudget) -> Result<Option<usize>> {
        Ok(Some(MAX_CATALOG_EXPRESSION_BYTES.min(budget.max_sql_bytes as usize)))
    }
    fn query(&self, limit: &usize) -> String {
        columns::select_sql(*limit)
    }
    fn decode(&self, row: &Rec, limit: &usize) -> Result<TelemetryRecord> {
        Ok(TelemetryRecord::ObjectDefinition(ObjectDefinitionRecord::new(
            Self::SOURCE.name.into(),
            ObjectDefinitionFact::MssqlColumn(Box::new(columns::decode(row, *limit)?.definition)),
        )?))
    }
}
