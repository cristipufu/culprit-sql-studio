//! One completed cache population and an independent original definition; receipts are created by real capture helpers.
use super::*;
use culprit_core::{CompletedSpillState, CompletedSpillStatementBinding, Engine};

pub struct CompletedSpillCase {
    pub name: &'static str,
    pub runtime: (CollectionDescription, Vec<TelemetryRecord>),
    pub definitions: Option<(CollectionDescription, Vec<TelemetryRecord>)>,
    pub state: Option<CompletedSpillState>,
    pub binding: Option<CompletedSpillStatementBinding>,
}

pub fn completed_spill_evidence_cases() -> Vec<CompletedSpillCase> {
    let mut result = Vec::new();
    for (index, name) in ["unavailable_metric", "native_zero", "recorded_spills"].into_iter().enumerate() {
        let mut runtime = super::cached_runtime_evidence_cases().remove(index);
        qualify(&mut runtime.0);
        let definitions = definition(&runtime);
        result.push(CompletedSpillCase {
            name,
            runtime,
            definitions: Some(definitions),
            state: Some(
                [
                    CompletedSpillState::Unavailable,
                    CompletedSpillState::NoRecordedSpills,
                    CompletedSpillState::RecordedSpills,
                ][index],
            ),
            binding: Some(CompletedSpillStatementBinding::Matched),
        });
    }
    for name in [
        "not_requested",
        "retained_plan",
        "short_unicode_sql",
        "endpoint_alias",
        "unqualified_alias",
        "recompiled",
        "foreign_database",
        "foreign_member",
        "new_process",
        "unqualified",
        "restricted_sql",
        "monitoring",
        "partial",
        "native_unavailable",
        "native_empty",
    ] {
        let mut runtime = super::cached_runtime_evidence_cases().pop().unwrap();
        qualify(&mut runtime.0);
        let mut definitions = definition(&runtime);
        let mut state = Some(CompletedSpillState::RecordedSpills);
        let mut binding = Some(CompletedSpillStatementBinding::Matched);
        match name {
            "not_requested" => binding = Some(CompletedSpillStatementBinding::NotRequested),
            "retained_plan" => {
                let TelemetryRecord::ObjectDefinition(record) = &definitions.1[0] else { panic!() };
                let ObjectDefinitionFact::MssqlCachedDefinition(native) = &record.fact else { panic!() };
                let retained = MssqlRetainedPlanDefinition {
                    context: native.context.clone(),
                    statement: native.statement.clone(),
                    module_parameters: None,
                    relational_input_proof: None,
                    parameter_context_proof: None,
                    statement_executions: Some("17".into()),
                    last_start_without_offset: Some("2020-01-01T11:58:00.003".into()),
                    database_last_query_plan_stats: Some(true),
                    batch_plan: native.estimated_plan.clone(),
                };
                definitions.1[0] = TelemetryRecord::ObjectDefinition(
                    ObjectDefinitionRecord::new(
                        record.source.clone(),
                        ObjectDefinitionFact::MssqlRetainedPlan(Box::new(retained)),
                    )
                    .unwrap(),
                );
                let SourceObservation::Snapshot(snapshot) = &mut definitions.0.observation else { panic!() };
                snapshot.dataset = SnapshotDataset::RetainedPlans;
            }
            "short_unicode_sql" => {
                let raw = format!(
                    "SELECT [{}] AS a,[{}] AS b,[{}] AS c,[{}] AS d FROM dbo.orders",
                    "界".repeat(100),
                    "界".repeat(100),
                    "界".repeat(100),
                    "界".repeat(100)
                );
                assert!(raw.chars().count() < 512 && raw.len() > 1024);
                let TelemetryRecord::ObjectDefinition(record) = &definitions.1[0] else { panic!() };
                let mut fact = record.fact.clone();
                let ObjectDefinitionFact::MssqlCachedDefinition(native) = &mut fact else { panic!() };
                native.statement = MssqlCachedPayload {
                    source_utf16_bytes: Some((raw.encode_utf16().count() * 2).to_string()),
                    native_sha256: Some(digest(raw.as_bytes())),
                    payload: DefinitionPayload::sql(&raw, Engine::Mssql),
                };
                definitions.1[0] = TelemetryRecord::ObjectDefinition(
                    ObjectDefinitionRecord::new(record.source.clone(), fact).unwrap(),
                );
            }
            "endpoint_alias" | "unqualified_alias" => {
                let CollectionTarget::Database(database) = &mut runtime.0.target else { panic!() };
                if name == "endpoint_alias" {
                    database.incarnation = Some("original-native-database-incarnation".into());
                }
                let CollectionTarget::Database(other) = &mut definitions.0.target else { panic!() };
                other.incarnation = database.incarnation.clone();
                other.location = culprit_core::DatabaseLocation::Server {
                    host: "different-route.invalid".into(),
                    port: 1433,
                    instance: None,
                };
                if name == "unqualified_alias" {
                    binding = Some(CompletedSpillStatementBinding::SourceChanged);
                }
            }
            "recompiled" | "foreign_database" | "restricted_sql" => {
                let TelemetryRecord::ObjectDefinition(record) = &definitions.1[0] else { panic!("definition") };
                let mut fact = record.fact.clone();
                let ObjectDefinitionFact::MssqlCachedDefinition(native) = &mut fact else {
                    panic!("native definition")
                };
                match name {
                    "recompiled" => {
                        native.context.identity.plan_generation = "3".into();
                        binding = Some(CompletedSpillStatementBinding::DefinitionNotObserved);
                    }
                    "foreign_database" => {
                        native.context.database_id = 8;
                        binding = Some(CompletedSpillStatementBinding::DefinitionNotObserved);
                    }
                    _ => {
                        native.statement.payload =
                            DefinitionPayload::Unavailable { reason: PayloadUnavailable::Restricted }
                    }
                }
                definitions.1[0] = TelemetryRecord::ObjectDefinition(
                    ObjectDefinitionRecord::new(record.source.clone(), fact).unwrap(),
                );
            }
            "foreign_member" => {
                let SourcePartition::InstanceLocal { member: Some(member), .. } = &mut definitions.0.partition else {
                    panic!()
                };
                member.native = NativeIdentity::new("other-native-instance");
                binding = Some(CompletedSpillStatementBinding::SourceChanged);
            }
            "new_process" => {
                let SourcePartition::InstanceLocal { epoch, .. } = &mut definitions.0.partition else { panic!() };
                *epoch = Some("other-native-process".into());
                binding = Some(CompletedSpillStatementBinding::SourceChanged);
            }
            "unqualified" => {
                runtime.0.partition = SourcePartition::Target;
                definitions.0.partition = SourcePartition::Target;
                binding = Some(CompletedSpillStatementBinding::SourceUnverified);
            }
            "monitoring" => {
                let TelemetryRecord::Operational(OperationalObservation {
                    fact: OperationalFact::MssqlCachedRuntime(native),
                    ..
                }) = &mut runtime.1[0]
                else {
                    panic!()
                };
                native.context.origin = StatementOrigin::CulpritMonitoring;
                native.context.monitoring_template_id = Some(digest(b"native-monitor-template"));
                definitions = definition(&runtime);
            }
            "partial" => {
                let SourceObservation::Snapshot(snapshot) = &mut runtime.0.observation else { panic!() };
                snapshot.sources[0] = StateSourceCoverage::observed(snapshot.sources[0].source.clone(), 1, false);
            }
            "native_unavailable" | "native_empty" => {
                runtime.1.clear();
                state = None;
                binding = None;
                let SourceObservation::Snapshot(snapshot) = &mut runtime.0.observation else { panic!() };
                snapshot.records = 0;
                snapshot.sources[0] = if name == "native_unavailable" {
                    StateSourceCoverage::unavailable(
                        snapshot.sources[0].source.clone(),
                        culprit_core::ErrorCode::PermissionDenied,
                        Some(CollectionStop::SourceUnavailable),
                    )
                } else {
                    StateSourceCoverage::collected(snapshot.sources[0].source.clone(), 0)
                };
            }
            _ => unreachable!(),
        }
        result.push(CompletedSpillCase {
            name,
            runtime,
            definitions: (name != "not_requested").then_some(definitions),
            state,
            binding,
        });
    }
    result
}

fn qualify(source: &mut CollectionDescription) {
    source.partition = SourcePartition::InstanceLocal {
        member: Some(SharedResourceIdentity {
            engine: Engine::Mssql,
            kind: SharedResourceKind::Instance,
            namespace: NativeIdentity::new("native-test-namespace"),
            native: NativeIdentity::new("native-test-instance"),
        }),
        epoch: Some("native-test-process".into()),
    };
}

fn definition(
    runtime: &(CollectionDescription, Vec<TelemetryRecord>),
) -> (CollectionDescription, Vec<TelemetryRecord>) {
    let TelemetryRecord::Operational(OperationalObservation {
        fact: OperationalFact::MssqlCachedRuntime(sample), ..
    }) = &runtime.1[0]
    else {
        panic!("runtime")
    };
    let raw = "SELECT SUM(amount) FROM dbo.original_spill_input WHERE tenant_id=9264";
    let native = MssqlCachedDefinition {
        context: sample.context.clone(),
        query_plan_hash: sample.query_plan_hash.clone(),
        statement: MssqlCachedPayload {
            source_utf16_bytes: Some((raw.encode_utf16().count() * 2).to_string()),
            native_sha256: Some(digest(raw.as_bytes())),
            payload: DefinitionPayload::sql(raw, Engine::Mssql),
        },
        estimated_plan: MssqlCachedPayload {
            source_utf16_bytes: None,
            native_sha256: None,
            payload: DefinitionPayload::Unavailable { reason: PayloadUnavailable::NotCollected },
        },
        module_parameters: None,
        relational_input_proof: None,
    };
    let source_id = "native-cache-definitions";
    let record = TelemetryRecord::ObjectDefinition(
        ObjectDefinitionRecord::new(source_id.into(), ObjectDefinitionFact::MssqlCachedDefinition(Box::new(native)))
            .unwrap(),
    );
    let source = CollectionDescription {
        source: "cached-definitions-state".into(),
        observation: SourceObservation::Snapshot(StateSnapshotDescription {
            dataset: SnapshotDataset::CachedDefinitions,
            observed_from: "2020-01-01T12:00:02Z".into(),
            observed_until: "2020-01-01T12:00:03Z".into(),
            records: 1,
            sources: vec![StateSourceCoverage::collected(source_id.into(), 1)],
        }),
        ..runtime.0.clone()
    };
    (source, vec![record])
}
