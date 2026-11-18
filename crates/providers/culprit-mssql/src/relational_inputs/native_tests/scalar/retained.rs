//! The original native source is captured before removal; replay authenticates the same sealed bytes afterwards.
use super::*;
use crate::test_support::owned_captures::{Captured, OwnedCaptures};
use culprit_core::telemetry::*;
mod diagnostics;
mod source_parameters;

pub(super) struct Original {
    owner: OwnedCaptures,
    capture: Captured,
    position: usize,
    statement: CachedStatementIdentity,
    original_parameter_context: Box<culprit_core::plan_facts::PlanParameterContext>,
}
impl Original {
    pub async fn capture(probe: &Probe, run: &RecordedRun) -> Self {
        let mut owner = OwnedCaptures::from_pool(probe.pool.clone(), probe.port).unwrap();
        let capture =
            owner.capture(CollectionSelection::Snapshot { dataset: SnapshotDataset::RetainedPlans }).await.unwrap();
        let positions = capture
            .records
            .iter()
            .enumerate()
            .filter_map(|(position, row)| {
                let TelemetryRecord::ObjectDefinition(row) = &row.record else { return None };
                let ObjectDefinitionFact::MssqlRetainedPlan(definition) = &row.fact else { return None };
                (definition.context.identity.key().unwrap() == run.context.identity.key().unwrap()).then_some(position)
            })
            .collect::<Vec<_>>();
        let [position] = positions.as_slice() else {
            panic!("the actual source must retain the exact original scalar incarnation")
        };
        let position = *position;
        let plan = explain(&capture, position, &run.context.identity);
        let retained = plan.facts().parameter_context.as_ref().unwrap().clone();
        // The executed STATISTICS XML and this retained native source are independent evidence. Presence and
        // unavailable states were checked against the retained payload itself; neither source repairs the other.
        eprintln!(
            "owned scalar native cache flags: executed={:?}, retained={:?}; retained runtime comparison available={}",
            run.plan.facts().parameter_context.as_ref().unwrap().retrieved_from_cache,
            retained.retrieved_from_cache,
            retained.parameters.iter().any(|parameter| parameter.integer_values_equal.is_some()),
        );
        Self {
            owner,
            capture,
            position,
            statement: run.context.identity.clone(),
            original_parameter_context: Box::new(retained),
        }
    }
    fn explain(&self) -> Plan {
        explain(&self.capture, self.position, &self.statement)
    }
    pub async fn verify_after_actor_disappears(
        &mut self,
        fixture: &OwnedSourceFixture,
        database: &str,
        actor: probe::Actor,
    ) {
        verify_actor_disappeared(fixture, database, &actor).await;
        self.owner.verify_retained(&self.capture).await.unwrap();
        let plan = self.explain();
        let context = plan.facts().parameter_context.as_ref().unwrap();
        assert_eq!(
            context,
            self.original_parameter_context.as_ref(),
            "the exact source-owned retained facts survive actor removal"
        );
        assert!(
            culprit_core::insights::insights(&plan)
                .iter()
                .filter(|insight| insight.rule == "parameter-context")
                .all(|insight| insight.severity == Severity::Low && insight.impact == 0.0 && insight.fix_sql.is_none())
        );
    }
}

/// Only authenticated original records enter the native retained parser. This is shared by the scalar and PSP
/// fixtures; it adds no reader, reconstructed body or substitute proof.
pub(super) fn explain(capture: &Captured, position: usize, statement: &CachedStatementIdentity) -> Plan {
    let source = capture.source().unwrap();
    let row = &capture.records[position];
    capture.reference(position).unwrap().validate_canonical_record(source, &row.record).unwrap();
    let TelemetryRecord::ObjectDefinition(record) = &row.record else { panic!() };
    assert!(record.fact.native_plan_proofs_match(source));
    let ObjectDefinitionFact::MssqlRetainedPlan(definition) = &record.fact else { panic!() };
    assert_eq!(definition.context.identity.key().unwrap(), statement.key().unwrap());
    if definition.parameter_context_proof.is_none() {
        diagnostics::report(source, definition);
    }
    assert!(definition.parameter_context_proof.is_some());
    let plan = crate::plan_xml::parse_retained_definition(definition, source).unwrap();
    source_parameters::assert_own_metadata(definition, &plan);
    let encoded = serde_json::to_string(&plan.facts().parameter_context).unwrap();
    assert!(!encoded.contains("ParameterCompiledValue") && !encoded.contains("ParameterRuntimeValue"));
    plan
}
pub(super) async fn verify_actor_disappeared(fixture: &OwnedSourceFixture, database: &str, actor: &probe::Actor) {
    let mut control = crate::test_support::connect(
        &config(fixture.port, database, fixture.password(), "scalar-retained-proof"),
        SessionKind::User,
    )
    .await
    .unwrap();
    verify_owned_source(&mut control).await.unwrap();
    // Bind the exact original native generation rather than infer identity from a reused session number.
    let mut query = tiberius::Query::new(
        "SELECT /* culprit fixture */ COUNT_BIG(*) AS original_actor FROM sys.dm_exec_sessions WHERE session_id=@P1 AND CONVERT(varchar(40),login_time,126)=@P2",
    );
    query.bind(actor.id);
    query.bind(actor.generation.as_str());
    let rows = query.query(&mut control).await.unwrap().into_first_result().await.unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].get::<i64, _>("original_actor"), Some(0));
    control.close().await.unwrap();
}
