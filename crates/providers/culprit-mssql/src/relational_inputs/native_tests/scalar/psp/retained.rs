//! Both independent native cache incarnations share one existing source/capture owner and original manifest.
use super::*;
use crate::test_support::owned_captures::{Captured, OwnedCaptures};
use culprit_core::telemetry::*;

pub(super) struct SavedVariants {
    owner: OwnedCaptures,
    capture: Captured,
    selected: Vec<SelectedVariant>,
}
struct SelectedVariant {
    position: usize,
    identity: CachedStatementIdentity,
    variant: u32,
    original_parameter_context: culprit_core::plan_facts::PlanParameterContext,
}
impl SavedVariants {
    pub async fn capture(
        probe: &probe::Probe<'_>,
        port: u16,
        runs: &[&probe::Run],
        cancellation: &culprit_collector::capture::CollectionCancellation,
    ) -> Self {
        let mut owner = OwnedCaptures::from_pool(probe.pool.clone(), port).unwrap();
        let capture = owner
            .capture_with_cancellation(
                CollectionSelection::Snapshot { dataset: SnapshotDataset::RetainedPlans },
                cancellation,
            )
            .await
            .unwrap();
        let mut selected = Vec::new();
        for run in runs {
            let expected = &run.recorded.context.identity;
            let positions = capture
                .records
                .iter()
                .enumerate()
                .filter_map(|(position, row)| {
                    let TelemetryRecord::ObjectDefinition(record) = &row.record else {
                        return None;
                    };
                    let ObjectDefinitionFact::MssqlRetainedPlan(definition) = &record.fact else {
                        return None;
                    };
                    (definition.context.database_id == run.recorded.context.database_id
                        && definition.context.identity.key().unwrap() == expected.key().unwrap())
                    .then_some(position)
                })
                .collect::<Vec<_>>();
            let [position] = positions.as_slice() else {
                panic!(
                    "one real retained original per native variant; a missing/oversized/unreadable original is not success"
                );
            };
            let plan = super::super::retained::explain(&capture, *position, expected);
            let original_parameter_context = plan.facts().parameter_context.as_ref().unwrap().clone();
            selected.push(SelectedVariant {
                position: *position,
                identity: expected.clone(),
                variant: run.variant.unwrap(),
                original_parameter_context,
            });
        }
        let saved = Self { owner, capture, selected };
        saved.explain();
        saved
    }
    fn explain(&self) {
        for original in &self.selected {
            let plan = super::super::retained::explain(&self.capture, original.position, &original.identity);
            assert_eq!(
                plan.facts().parameter_context.as_ref(),
                Some(&original.original_parameter_context),
                "retain the same native source facts without borrowing executed values"
            );
            let TelemetryRecord::ObjectDefinition(record) = &self.capture.records[original.position].record else {
                panic!()
            };
            let ObjectDefinitionFact::MssqlRetainedPlan(definition) = &record.fact else { panic!() };
            let DefinitionPayload::Available { text: xml, .. } = &definition.batch_plan.payload else {
                panic!("original native variant XML is unavailable")
            };
            assert_eq!(super::identity::variant_id(xml), Some(original.variant));
            assert!(plan.facts().parameter_context.as_ref().is_some_and(|context| context.is_valid()));
            assert!(
                culprit_core::insights::insights(&plan)
                    .iter()
                    .filter(|insight| insight.rule == "parameter-context")
                    .all(|insight| insight.severity == culprit_core::Severity::Low
                        && insight.impact == 0.0
                        && insight.fix_sql.is_none())
            );
        }
    }
    pub async fn verify_after_removal(
        &mut self,
        fixture: &crate::test_support::OwnedSourceFixture,
        database: &str,
        actor: &super::super::probe::Actor,
    ) {
        super::super::retained::verify_actor_disappeared(fixture, database, actor).await;
        self.owner.verify_retained(&self.capture).await.unwrap();
        self.explain();
    }
}
