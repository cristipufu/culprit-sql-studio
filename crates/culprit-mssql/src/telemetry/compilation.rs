use super::streamed::SnapshotRecipe;
use crate::{query_store::costs, read::NativeRead, value::Rec};
use culprit_core::{Result, Source, telemetry::*};

pub(super) struct QueryCompilation;
#[async_trait::async_trait]
impl SnapshotRecipe for QueryCompilation {
    type Layout = ();
    const DATASET: SnapshotDataset = SnapshotDataset::QueryCompilation;
    const NAME: &'static str = "query-compilation-state";
    const SOURCE: &'static Source = &crate::sources::QUERY_COMPILATION;
    const REQUIRES_FULL_METADATA: bool = true;

    async fn layout<R: NativeRead>(&self, _reader: &mut R, _budget: &CollectionBudget) -> Result<Option<()>> {
        Ok(Some(()))
    }
    fn query(&self, _: &()) -> String {
        costs::compilation_snapshot_sql()
    }
    fn decode(&self, row: &Rec, _: &()) -> Result<TelemetryRecord> {
        let compilation = costs::Compilation::decode(row)?.observation();
        Ok(TelemetryRecord::Operational(OperationalObservation {
            source: Self::SOURCE.name.into(),
            key: compilation.key()?,
            fact: OperationalFact::MssqlQueryCompilation(Box::new(compilation)),
        }))
    }
}
