use super::streamed::SnapshotRecipe;
use crate::{cached_runtime, read::NativeRead, value::Rec};
use culprit_core::{Result, Source, telemetry::*};
pub(super) struct CachedRuntime;
#[async_trait::async_trait]
impl SnapshotRecipe for CachedRuntime {
    type Layout = cached_runtime::Layout;
    const DATASET: SnapshotDataset = SnapshotDataset::CachedRuntime;
    const NAME: &'static str = "statement-runtime-cache";
    const SOURCE: &'static Source = &crate::sources::PLAN_CACHE;
    async fn layout<R: NativeRead>(&self, reader: &mut R, _budget: &CollectionBudget) -> Result<Option<Self::Layout>> {
        cached_runtime::Layout::read(reader).await.map(Some)
    }
    fn query(&self, layout: &Self::Layout) -> String {
        cached_runtime::select_sql(layout)
    }
    fn decode(&self, row: &Rec, _: &Self::Layout) -> Result<TelemetryRecord> {
        let runtime = cached_runtime::decode(row)?;
        Ok(TelemetryRecord::Operational(OperationalObservation {
            source: Self::SOURCE.name.into(),
            key: runtime.context.identity.key()?,
            fact: OperationalFact::MssqlCachedRuntime(Box::new(runtime)),
        }))
    }
    fn complete_record_scope(&self, record: &TelemetryRecord) -> bool {
        matches!(record,TelemetryRecord::Operational(OperationalObservation { fact: OperationalFact::MssqlCachedRuntime(runtime),.. }) if runtime.measured())
    }
}
