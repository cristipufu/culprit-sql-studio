use super::streamed::SnapshotRecipe;
use crate::{partitions, read::NativeRead, value::Rec};
use culprit_core::{Result, Source, telemetry::*};
pub(super) struct Partitions;
#[async_trait::async_trait]
impl SnapshotRecipe for Partitions {
    type Layout = usize;
    const DATASET: SnapshotDataset = SnapshotDataset::Partitions;
    const NAME: &'static str = "partition-boundary-state";
    const SOURCE: &'static Source = &crate::sources::PARTITIONS;
    const REQUIRES_FULL_METADATA: bool = true;
    async fn layout<R: NativeRead>(&self, _: &mut R, budget: &CollectionBudget) -> Result<Option<usize>> {
        Ok(Some(MAX_CATALOG_EXPRESSION_BYTES.min(budget.max_sql_bytes as usize)))
    }
    fn query(&self, maximum_bytes: &usize) -> String {
        partitions::select_sql(partitions::Selection::AllIndexes, *maximum_bytes)
    }
    fn decode(&self, row: &Rec, maximum_bytes: &usize) -> Result<TelemetryRecord> {
        Ok(TelemetryRecord::ObjectDefinition(ObjectDefinitionRecord::new(
            Self::SOURCE.name.into(),
            ObjectDefinitionFact::MssqlPartitionBoundary(Box::new(partitions::decode(row, *maximum_bytes)?.definition)),
        )?))
    }
    fn complete_record_scope(&self, record: &TelemetryRecord) -> bool {
        matches!(record,TelemetryRecord::ObjectDefinition(ObjectDefinitionRecord {fact:ObjectDefinitionFact::MssqlPartitionBoundary(boundary),..})
            if boundary.value_is_null!=Some(false) || boundary.native_value_sha256.is_some() && boundary.native_value_type.is_some())
    }
}
