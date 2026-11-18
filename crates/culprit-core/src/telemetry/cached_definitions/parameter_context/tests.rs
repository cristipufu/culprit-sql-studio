use super::*;
use crate::plan_facts::{
    PlanParameter, PlanParameterComparison, PlanParameterUse, PlanParameterUseKind, PlanParameterValueState,
};

fn source() -> CollectionDescription {
    CollectionDescription {
        target: crate::DatabaseIdentity::server(crate::Engine::Mssql, "app", "private-native-endpoint", 1433, None)
            .into(),
        partition: SourcePartition::InstanceLocal {
            member: Some(SharedResourceIdentity {
                engine: crate::Engine::Mssql,
                kind: SharedResourceKind::Instance,
                namespace: NativeIdentity::new("private-native-machine"),
                native: NativeIdentity::new("private-native-member"),
            }),
            epoch: Some("private-native-epoch".into()),
        },
        source: "native-retained".into(),
        continuity: SourceContinuity::Unverified,
        observation: SourceObservation::Snapshot(StateSnapshotDescription {
            dataset: SnapshotDataset::RetainedPlans,
            observed_from: "2026-11-14T12:00:00Z".into(),
            observed_until: "2026-11-14T12:00:01Z".into(),
            records: 1,
            sources: vec![StateSourceCoverage::observed("native-cache".into(), 1, true)],
        }),
    }
}

fn context() -> PlanParameterContext {
    PlanParameterContext {
        retrieved_from_cache: None,
        parameters: vec![PlanParameter {
            integer_values_equal: None,
            name: Some("@customer".into()),
            data_type: Some("[private-native-database].[dbo].[CustomerType]".into()),
            compiled: PlanParameterValueState::Recorded,
            runtime: PlanParameterValueState::Recorded,
            comparison: PlanParameterComparison::DifferentRepresentation,
        }],
        uses: vec![PlanParameterUse { parameter_index: 0, node_id: 7, kind: PlanParameterUseKind::EqualitySeek }],
        issues: vec![],
    }
}

#[test]
fn original_safe_comparisons_bind_the_exact_native_statement_payload_and_source_before_redaction() {
    let source = source();
    let mut definition = super::super::retained_tests::retained();
    definition.redact(crate::Engine::Mssql);
    let proof = MssqlParameterContextProof::for_retained(&source, &definition, context()).unwrap();
    assert!(proof.matches_retained(&source, &definition));
    assert_eq!(proof.context.parameters[0].data_type.as_deref(), Some("[dbo].[CustomerType]"));
    let encoded = serde_json::to_string(&proof).unwrap();
    assert!(!encoded.contains("private-native"));
    definition.parameter_context_proof = Some(proof.clone());
    assert!(definition.is_valid());
    for changed in ["generation", "sql-handle", "native-body", "redacted-body", "facts", "source"] {
        let mut definition = definition.clone();
        let mut source = source.clone();
        match changed {
            "generation" => definition.context.identity.plan_generation = "2".into(),
            "sql-handle" => definition.context.identity.sql_handle = "0x0400".into(),
            "native-body" => definition.batch_plan.native_sha256 = Some(digest(b"other original XML")),
            "redacted-body" => {
                definition.batch_plan.payload = DefinitionPayload::plan("<ShowPlanXML/>", crate::Engine::Mssql)
            }
            "facts" => definition.parameter_context_proof.as_mut().unwrap().context.uses[0].node_id += 1,
            "source" => source.source = "another-source".into(),
            _ => unreachable!(),
        }
        assert!(
            !definition.parameter_context_proof.as_ref().unwrap().matches_retained(&source, &definition),
            "{changed}"
        );
    }
    let mut later_acquisition = source;
    let SourceObservation::Snapshot(snapshot) = &mut later_acquisition.observation else { panic!() };
    snapshot.observed_until = "2026-11-14T12:00:02Z".into();
    assert!(
        proof.matches_retained(&later_acquisition, &definition),
        "content binding attests no execution clock; the authenticated capture retains its own acquisition bounds"
    );
}

#[test]
fn excessive_metadata_retains_an_explicit_gap_without_a_selected_parameter_subset() {
    let source = source();
    let definition = super::super::retained_tests::retained();
    let mut context = context();
    context.parameters = (0..crate::plan_facts::MAX_PLAN_PARAMETERS).map(|index| PlanParameter { integer_values_equal: None,
        name: Some(format!("@parameter_{index}")), data_type: Some("[dbo].[AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA]".into()),
        ..context.parameters[0].clone()
    }).collect();
    let proof = MssqlParameterContextProof::for_retained(&source, &definition, context).unwrap();
    assert!(proof.is_valid());
    assert!(proof.context.parameters.is_empty() && proof.context.uses.is_empty());
    assert_eq!(proof.context.issues, vec![PlanParameterIssue::MetadataLimit]);
    assert!(proof.matches_retained(&source, &definition));
}

#[test]
fn unsafe_original_metadata_cannot_gain_export_authority_by_being_masked_later() {
    let source = source();
    let mut definition = super::super::retained_tests::retained();
    definition.redact(crate::Engine::Mssql);
    let mut proof = MssqlParameterContextProof::for_retained(&source, &definition, context()).unwrap();
    proof.context.parameters[0].data_type = Some("[private-native-database].[dbo].[CustomerType]".into());
    proof.input_sha256 = input(&proof.source_scope, &definition, &proof.context).unwrap();
    assert!(!proof.is_valid(), "content equality cannot authorize unredacted metadata");
    definition.parameter_context_proof = Some(proof);
    assert!(!definition.is_valid());
    assert!(
        ObjectDefinitionRecord::new(
            "native-cache".into(),
            ObjectDefinitionFact::MssqlRetainedPlan(Box::new(definition))
        )
        .is_err()
    );
}
