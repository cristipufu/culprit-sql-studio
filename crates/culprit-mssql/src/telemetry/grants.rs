use super::streamed::SnapshotRecipe;
use crate::{
    grants::{Layout, decode, select_sql},
    read::NativeRead,
    value::Rec,
};
use culprit_core::{Result, Source, telemetry::*};
pub(super) struct Grants;
#[async_trait::async_trait]
impl SnapshotRecipe for Grants {
    type Layout = Layout;
    const DATASET: SnapshotDataset = SnapshotDataset::StatementGrants;
    const NAME: &'static str = "statement-grant-cache";
    const SOURCE: &'static Source = &crate::sources::GRANTS;
    async fn layout<R: NativeRead>(&self, reader: &mut R, _budget: &CollectionBudget) -> Result<Option<Layout>> {
        Layout::read(reader).await
    }
    fn query(&self, layout: &Layout) -> String {
        select_sql(layout, false)
    }
    fn complete_record_scope(&self, record: &TelemetryRecord) -> bool {
        matches!(record,TelemetryRecord::Operational(OperationalObservation {fact:OperationalFact::MssqlStatementGrants(grants),..}) if grants.context.identity.has_plan_handle())
    }
    fn decode(&self, row: &Rec, layout: &Layout) -> Result<TelemetryRecord> {
        let grants = decode(row, layout)?;
        Ok(TelemetryRecord::Operational(OperationalObservation {
            source: Self::SOURCE.name.into(),
            key: grants.context.identity.key()?,
            fact: OperationalFact::MssqlStatementGrants(Box::new(grants)),
        }))
    }
}
