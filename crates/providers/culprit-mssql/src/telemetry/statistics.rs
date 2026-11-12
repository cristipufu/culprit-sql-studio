use super::streamed::SnapshotRecipe;
use crate::{read::NativeRead, statistics, value::Rec};
use culprit_core::{Result, Source, telemetry::*};
pub(super) struct Statistics;
#[async_trait::async_trait]
impl SnapshotRecipe for Statistics {
    type Layout = ();
    const DATASET: SnapshotDataset = SnapshotDataset::Statistics;
    const NAME: &'static str = "statistics-state";
    const SOURCE: &'static Source = &crate::sources::STATISTICS;
    const REQUIRES_FULL_METADATA: bool = true;
    async fn layout<R: NativeRead>(&self, _reader: &mut R, _budget: &CollectionBudget) -> Result<Option<()>> {
        Ok(Some(()))
    }
    fn query(&self, _: &()) -> String {
        statistics::select_sql()
    }
    fn complete_record_scope(&self, record: &TelemetryRecord) -> bool {
        matches!(record,TelemetryRecord::Operational(OperationalObservation { fact:OperationalFact::MssqlStatistics(statistics),.. }) if statistics.properties_available)
    }
    fn decode(&self, row: &Rec, _: &()) -> Result<TelemetryRecord> {
        let statistics = statistics::decode(row)?;
        Ok(TelemetryRecord::Operational(OperationalObservation {
            source: Self::SOURCE.name.into(),
            key: statistics.key()?,
            fact: OperationalFact::MssqlStatistics(Box::new(statistics)),
        }))
    }
}
