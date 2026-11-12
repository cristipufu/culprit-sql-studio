use super::streamed::SnapshotRecipe;
use crate::{indexes, read::NativeRead, value::Rec};
use culprit_core::{Result, Source, telemetry::*};
pub(super) struct Indexes;
#[async_trait::async_trait]
impl SnapshotRecipe for Indexes {
    type Layout = indexes::Layout;
    const DATASET: SnapshotDataset = SnapshotDataset::Indexes;
    const NAME: &'static str = "index-state";
    const SOURCE: &'static Source = &crate::sources::INDEXES;
    const REQUIRES_FULL_METADATA: bool = true;
    async fn layout<R: NativeRead>(&self, _reader: &mut R, budget: &CollectionBudget) -> Result<Option<Self::Layout>> {
        Ok(Some(indexes::Layout {
            with_dmvs: true,
            expression_limit: MAX_CATALOG_EXPRESSION_BYTES.min(budget.max_sql_bytes as usize),
        }))
    }
    fn query(&self, layout: &Self::Layout) -> String {
        indexes::select_sql(layout)
    }
    fn decode(&self, row: &Rec, layout: &Self::Layout) -> Result<TelemetryRecord> {
        let index = indexes::decode(row, layout)?.snapshot;
        Ok(TelemetryRecord::Operational(OperationalObservation {
            source: Self::SOURCE.name.into(),
            key: index.key()?,
            fact: OperationalFact::MssqlIndex(Box::new(index)),
        }))
    }
}
