use super::streamed::SnapshotRecipe;
use crate::{cached_definitions, read::NativeRead, value::Rec};
use culprit_core::{Result, Source, telemetry::*};
pub(super) struct CachedDefinitions;
pub(super) struct Layout {
    limits: cached_definitions::Limits,
    bindings: crate::relational_inputs::Bindings,
}

#[async_trait::async_trait]
impl SnapshotRecipe for CachedDefinitions {
    type Layout = Layout;
    const DATASET: SnapshotDataset = SnapshotDataset::CachedDefinitions;
    const NAME: &'static str = "cached-definitions-state";
    const SOURCE: &'static Source = &crate::sources::PLAN_CACHE;
    async fn layout<R: NativeRead>(&self, _: &mut R, budget: &CollectionBudget) -> Result<Option<Self::Layout>> {
        Ok(Some(Layout { limits: cached_definitions::Limits::from_budget(budget)?, bindings: Default::default() }))
    }
    fn query(&self, layout: &Self::Layout) -> String {
        cached_definitions::select_sql(&layout.limits)
    }
    fn decode(&self, row: &Rec, layout: &Self::Layout) -> Result<TelemetryRecord> {
        let mut definition = cached_definitions::decode(row, &layout.limits)?;
        if !layout.bindings.contains(&definition.context) {
            definition.estimated_plan.payload =
                DefinitionPayload::Unavailable { reason: PayloadUnavailable::NotCollected };
            definition.module_parameters = None;
        }
        Ok(TelemetryRecord::ObjectDefinition(ObjectDefinitionRecord::new(
            Self::SOURCE.name.into(),
            ObjectDefinitionFact::MssqlCachedDefinition(Box::new(definition)),
        )?))
    }
    fn complete_record_scope(&self, record: &TelemetryRecord) -> bool {
        matches!(record,TelemetryRecord::ObjectDefinition(ObjectDefinitionRecord {fact:ObjectDefinitionFact::MssqlCachedDefinition(definition),..}) if definition.context.identity.has_plan_handle() && !matches!(definition.estimated_plan.payload,DefinitionPayload::Unavailable{reason:PayloadUnavailable::NotCollected}))
    }
    fn observe_batch(&self, rows: &[Rec], layout: &mut Self::Layout) -> Result<()> {
        layout.bindings.observe(rows)
    }
    async fn recheck<R: NativeRead>(&self, reader: &mut R, layout: &Self::Layout) -> Result<()> {
        layout.bindings.require_verified(reader).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        resources::test_support::{FakeRead, replace},
        value::Val,
    };

    #[tokio::test]
    async fn records_beyond_the_native_proof_budget_keep_their_statement_and_explicit_incomplete_plan_scope() {
        let recipe = CachedDefinitions;
        let mut reader = FakeRead::new(vec![]);
        let mut layout = recipe.layout(&mut reader, &CollectionBudget::default()).await.unwrap().unwrap();
        let rows = (0..=crate::relational_inputs::MAX_BINDINGS)
            .map(|index| {
                replace(
                    &crate::cached_definitions::tests::row(),
                    &[("plan_handle", Val::Text(format!("0x{index:016x}01")))],
                )
            })
            .collect::<Vec<_>>();
        recipe.observe_batch(&rows, &mut layout).unwrap();
        let first = recipe.decode(&rows[0], &layout).unwrap();
        assert!(recipe.complete_record_scope(&first));
        let overflow = recipe.decode(rows.last().unwrap(), &layout).unwrap();
        assert!(!recipe.complete_record_scope(&overflow));
        let TelemetryRecord::ObjectDefinition(record) = overflow else { panic!("definition") };
        let ObjectDefinitionFact::MssqlCachedDefinition(definition) = record.fact else { panic!("cache definition") };
        assert!(definition.statement.payload.is_available());
        assert!(matches!(
            definition.estimated_plan.payload,
            DefinitionPayload::Unavailable { reason: PayloadUnavailable::NotCollected }
        ));
        assert!(definition.module_parameters.is_none());
        assert!(reader.calls.is_empty(), "witness admission is local while the native stream is active");
    }
}
