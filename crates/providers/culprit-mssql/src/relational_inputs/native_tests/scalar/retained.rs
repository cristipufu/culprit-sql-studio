//! The original native source is captured before removal; replay authenticates the same sealed bytes afterwards.
use super::*;
use crate::test_support::owned_captures::{Captured, OwnedCaptures};
use crate::test_support::owned_case;
use culprit_collector::capture::CollectionCancellation;
use culprit_core::telemetry::*;
mod blocking;
mod diagnostics;
mod forcing;
pub(crate) mod grant_pressure;
mod source_parameters;
mod statistics;

pub(super) struct Original {
    owner: OwnedCaptures,
    capture: Captured,
    position: usize,
    statement: CachedStatementIdentity,
    original_parameter_context: Box<culprit_core::plan_facts::PlanParameterContext>,
    original_lookup_insights: Vec<culprit_core::insights::PlanInsight>,
    cancellation: CollectionCancellation,
}
impl Original {
    pub async fn capture(probe: &Probe, run: &RecordedRun) -> Self {
        let mut owner = OwnedCaptures::from_pool(probe.pool.clone(), probe.port).unwrap();
        let capture = owner
            .capture_with_cancellation(
                CollectionSelection::Snapshot { dataset: SnapshotDataset::RetainedPlans },
                probe.cancellation(),
            )
            .await
            .unwrap();
        Self::from_capture(owner, capture, run, probe.cancellation().clone())
    }
    async fn capture_owned(mut owner: OwnedCaptures, run: &RecordedRun) -> Self {
        let capture =
            owner.capture(CollectionSelection::Snapshot { dataset: SnapshotDataset::RetainedPlans }).await.unwrap();
        Self::from_capture(owner, capture, run, CollectionCancellation::default())
    }
    fn from_capture(
        owner: OwnedCaptures,
        capture: Captured,
        run: &RecordedRun,
        cancellation: CollectionCancellation,
    ) -> Self {
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
        let original_lookup_insights = lookup_insights(&plan);
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
            original_lookup_insights,
            cancellation,
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
        verify_actor_count(fixture, database, &actor, 0, &self.cancellation).await;
        self.owner.verify_retained(&self.capture).await.unwrap();
        let plan = self.explain();
        let context = plan.facts().parameter_context.as_ref().unwrap();
        assert_eq!(
            context,
            self.original_parameter_context.as_ref(),
            "the exact source-owned retained facts survive actor removal"
        );
        assert_eq!(
            serde_json::to_value(lookup_insights(&plan)).unwrap(),
            serde_json::to_value(&self.original_lookup_insights).unwrap(),
            "resolved history keeps its own original lookup evidence, without borrowing the executed plan's parameter facts"
        );
        assert!(
            culprit_core::insights::insights(&plan)
                .iter()
                .filter(|insight| insight.rule == "parameter-context")
                .all(|insight| insight.severity == Severity::Low && insight.impact == 0.0 && insight.fix_sql.is_none())
        );
    }
}

fn lookup_insights(plan: &Plan) -> Vec<culprit_core::insights::PlanInsight> {
    culprit_core::insights::insights(plan).into_iter().filter(|insight| insight.rule == "key-lookups").collect()
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
pub(super) async fn verify_actor_present(fixture: &OwnedSourceFixture, database: &str, actor: &probe::Actor) {
    verify_actor_count(fixture, database, actor, 1, &CollectionCancellation::default()).await;
}
pub(super) async fn verify_actor_disappeared(fixture: &OwnedSourceFixture, database: &str, actor: &probe::Actor) {
    verify_actor_count(fixture, database, actor, 0, &CollectionCancellation::default()).await;
}
async fn verify_actor_count(
    fixture: &OwnedSourceFixture,
    database: &str,
    actor: &probe::Actor,
    expected: i64,
    cancellation: &CollectionCancellation,
) {
    owned_case::require_active(cancellation).unwrap();
    let mut control = crate::test_support::connect(
        &config(fixture.port, database, fixture.password(), "scalar-retained-proof"),
        SessionKind::User,
    )
    .await
    .unwrap();
    let observed = async {
        owned_case::admitted(cancellation, verify_owned_source(&mut control)).await?;
        // Match the SAME full native datetime2 representation as Probe's original login point.
        // A count0 without the corresponding live positive would not validate this generation predicate.
        let mut query = tiberius::Query::new(
            "SELECT /* culprit fixture */ COUNT_BIG(*) AS original_actor FROM sys.dm_exec_sessions WHERE session_id=@P1 AND CONVERT(varchar(40),CAST(login_time AS datetime2),126)=@P2",
        );
        query.bind(actor.id);
        query.bind(actor.generation.as_str());
        owned_case::require_active(cancellation)?;
        let results = query
            .query(&mut control)
            .await
            .map_err(crate::conn::map_error)?
            .into_results()
            .await
            .map_err(crate::conn::map_error)?;
        let [rows] = results.as_slice() else { return Err(crate::read::invalid_data()) };
        let [row] = rows.as_slice() else { return Err(crate::read::invalid_data()) };
        crate::read::integer(&crate::value::Rec::from_row(row), "original_actor")
    }
    .await;
    // Always retire this original verifier, preserving the first error before any close failure.
    let closed = owned_case::retire(control).await;
    let count = observed.unwrap();
    closed.unwrap();
    assert_eq!(count, expected, "the exact original native actor generation must match its expected lifetime");
}

/// Activity/grant rows expose the original request SQL token, not a native plan incarnation. The execution
/// owner's before/final cache checks separately retain that incarnation; this one token join is shared.
fn matches_native_statement(request: &ActivityObservation, statement: &CachedStatementContext) -> bool {
    let Some(hash) = statement.query_hash.as_deref() else { return false };
    let expected = NativeIdentity::new(statement.identity.sql_handle.trim_start_matches("0x"))
        .qualified("native_object", "sys.dm_exec_requests.sql_handle")
        .qualified("database_id", statement.database_id.to_string())
        .qualified("statement_start_offset", statement.identity.statement_start_bytes.to_string())
        .qualified("statement_end_offset", statement.identity.statement_end_bytes.to_string());
    request.is_valid()
        && statement.is_valid()
        && request.statement_identity.as_ref() == Some(&expected)
        && request.query_hash.as_deref() == Some(hash.trim_start_matches("0x"))
}
