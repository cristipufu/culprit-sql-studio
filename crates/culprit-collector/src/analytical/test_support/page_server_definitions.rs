//! Controlled original compiler/runtime/definition captures share native facts, never forged receipt objects.
//! These fixtures exercise codecs and binding semantics; they are not native cost or platform qualification.
use super::*;
use culprit_core::query_store_statement_report::QueryStoreStatementBinding;

pub struct QueryStoreStatementCase {
    pub name: &'static str,
    pub compilation: (CollectionDescription, Vec<TelemetryRecord>),
    pub runtime: (CollectionDescription, Vec<TelemetryRecord>),
    pub definitions: Option<(CollectionDescription, Vec<TelemetryRecord>)>,
    pub binding: Option<QueryStoreStatementBinding>,
}

pub fn query_store_statement_evidence_cases() -> Vec<QueryStoreStatementCase> {
    super::compilation_definition_evidence_cases()
        .into_iter()
        .map(|case| {
            let OperationalFact::MssqlQueryCompilation(native) = &operational(&case.current.1[0]).fact else {
                panic!()
            };
            let mut original = super::page_server_reads::page_server_read_evidence_cases()
                .into_iter()
                .find(|case| case.name == "single_fragment_with_definitions")
                .unwrap();
            original.source.target = case.current.0.target.clone();
            original.source.source = "original-page-server-runtime".into();
            let mut plan_identity = None;
            for record in &mut original.records {
                match record {
                    TelemetryRecord::QueryDefinition(definition) => {
                        definition.identity = native.identity.clone();
                        definition.id = definition.identity.definition_id(&original.source).unwrap();
                        definition.origin = StatementOrigin::Unknown;
                        definition.monitoring_template_id = None;
                        definition.sql = DefinitionPayload::Unavailable { reason: PayloadUnavailable::Restricted };
                        definition.source_text_bytes = None;
                        if case.name == "matched" {
                            let TelemetryRecord::QueryDefinition(captured) = &case.definitions.as_ref().unwrap().1[0]
                            else {
                                panic!()
                            };
                            definition.sql = captured.sql.clone();
                            definition.origin = captured.origin;
                        }
                    }
                    TelemetryRecord::PlanDefinition(definition) => {
                        definition.identity.query = native.identity.clone();
                        definition.identity.qualifiers.insert(
                            "initial_compile_at".into(),
                            native.identity.qualifiers["initial_compile_at"].clone(),
                        );
                        definition.id = definition.identity.definition_id(&original.source).unwrap();
                        definition.query_definition_id = Some(native.identity.definition_id(&original.source).unwrap());
                        definition.plan = DefinitionPayload::Unavailable { reason: PayloadUnavailable::Restricted };
                        plan_identity = Some(definition.identity.clone());
                    }
                    _ => (),
                }
            }
            for record in &mut original.records {
                if let TelemetryRecord::QueryRuntime(runtime) = record {
                    let plan = plan_identity.as_ref().unwrap();
                    runtime.key.references = StatementReferences {
                        query: Some(NativeDefinitionReference {
                            native: native.identity.native(),
                            definition_id: Some(native.identity.definition_id(&original.source).unwrap()),
                        }),
                        plan: Some(NativeDefinitionReference {
                            native: plan.native(),
                            definition_id: Some(plan.definition_id(&original.source).unwrap()),
                        }),
                    };
                    runtime.executions = "17".into();
                    runtime.page_server_reads = Some(Box::new(PageServerReadStatistics {
                        availability: PageServerReadAvailability::Available,
                        average_pages: Some(12.5),
                        last_pages: Some("9007199254740993".into()),
                        minimum_pages: Some("0".into()),
                        maximum_pages: None,
                        standard_deviation_pages: None,
                    }));
                }
            }
            QueryStoreStatementCase {
                name: case.name,
                compilation: case.current,
                runtime: (original.source, original.records),
                definitions: case.definitions,
                binding: case.binding,
            }
        })
        .collect()
}

fn operational(record: &TelemetryRecord) -> &OperationalObservation {
    let TelemetryRecord::Operational(row) = record else { panic!() };
    row
}
