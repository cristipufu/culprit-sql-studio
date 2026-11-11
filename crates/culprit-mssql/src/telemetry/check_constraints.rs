use super::streamed::SnapshotRecipe;
use crate::{check_constraints, read::NativeRead, value::Rec};
use culprit_core::{Result, Source, telemetry::*};
pub(super) struct CheckConstraints;
#[async_trait::async_trait]
impl SnapshotRecipe for CheckConstraints {
    type Layout = usize;
    const DATASET: SnapshotDataset = SnapshotDataset::CheckConstraints;
    const NAME: &'static str = "check-constraint-state";
    const SOURCE: &'static Source = &crate::sources::CHECK_CONSTRAINTS;
    const REQUIRES_FULL_METADATA: bool = true;
    async fn layout<R: NativeRead>(&self, _reader: &mut R, budget: &CollectionBudget) -> Result<Option<usize>> {
        Ok(Some(MAX_CATALOG_EXPRESSION_BYTES.min(budget.max_sql_bytes as usize)))
    }
    fn query(&self, limit: &usize) -> String {
        check_constraints::select_sql(*limit)
    }
    fn decode(&self, row: &Rec, limit: &usize) -> Result<TelemetryRecord> {
        Ok(TelemetryRecord::ObjectDefinition(ObjectDefinitionRecord::new(
            Self::SOURCE.name.into(),
            ObjectDefinitionFact::MssqlCheckConstraint(Box::new(check_constraints::decode(row, *limit)?)),
        )?))
    }
}
