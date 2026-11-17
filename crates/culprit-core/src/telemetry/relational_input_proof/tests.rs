use super::*;

fn source(dataset: SnapshotDataset) -> CollectionDescription {
    CollectionDescription {
        target: crate::DatabaseIdentity::server(crate::Engine::Mssql, "private-db", "private-endpoint", 1433, None)
            .into(),
        partition: SourcePartition::InstanceLocal {
            member: Some(SharedResourceIdentity {
                engine: crate::Engine::Mssql,
                kind: SharedResourceKind::Instance,
                namespace: NativeIdentity::new("controlled-source"),
                native: NativeIdentity::new("member-a"),
            }),
            epoch: Some("native-generation-a".into()),
        },
        source: "native-definitions".into(),
        continuity: SourceContinuity::Unverified,
        observation: SourceObservation::Snapshot(StateSnapshotDescription {
            dataset,
            observed_from: "2026-11-14T12:00:00Z".into(),
            observed_until: "2026-11-14T12:00:01Z".into(),
            records: 1,
            sources: vec![StateSourceCoverage::observed("native-cache".into(), 1, true)],
        }),
    }
}
fn payload(raw: &str, redact: fn(&str, crate::Engine) -> DefinitionPayload) -> MssqlCachedPayload {
    MssqlCachedPayload {
        source_utf16_bytes: Some((raw.encode_utf16().count() * 2).to_string()),
        native_sha256: Some(digest(raw.as_bytes())),
        payload: redact(raw, crate::Engine::Mssql),
    }
}
fn cached() -> MssqlCachedDefinition {
    let sql = "SELECT value FROM @Rows WHERE value=9264";
    MssqlCachedDefinition {
        context: CachedStatementContext {
            identity: CachedStatementIdentity {
                plan_handle: "0x0102".into(),
                sql_handle: "0x0304".into(),
                statement_start_bytes: 0,
                statement_end_bytes: -1,
                created_without_offset: "2026-11-14T12:00:00.003".into(),
                plan_generation: "2".into(),
            },
            database_id: 7,
            query_hash: None,
            origin: StatementOrigin::Application,
            monitoring_template_id: None,
            source_text_bytes: Some((sql.len() * 2).to_string()),
            source_text_complete: true,
        },
        query_plan_hash: None,
        statement: payload(sql, DefinitionPayload::sql),
        estimated_plan: payload(
            r#"<ShowPlanXML><BatchSequence><Batch><Statements><StmtSimple StatementText="SELECT value FROM @Rows WHERE value=9264"><QueryPlan><RelOp NodeId="1" PhysicalOp="Table Scan"><TableScan><Object Table="[@Rows]" /></TableScan></RelOp></QueryPlan></StmtSimple></Statements></Batch></BatchSequence></ShowPlanXML>"#,
            DefinitionPayload::plan,
        ),
        module_parameters: Some(MssqlModuleParameters {
            database_id: 7,
            object_id: 1001,
            module_number: 1,
            sql_handle: "0x0304".into(),
            table_parameters: [("@Rows".into(), vec!["dbo".into(), "RowsType".into()])].into(),
        }),
        relational_input_proof: None,
    }
}
fn binding(id: u32) -> MssqlRelationalInputBinding {
    MssqlRelationalInputBinding {
        node_id: id,
        input: RelationalInput {
            kind: RelationalInputKind::TableValuedParameter,
            provenance: RelationalInputProvenance::NativeParameterMetadata,
        },
    }
}

#[test]
fn proof_scope_survives_paging_counts_and_final_read_bounds_but_not_source_or_owner_changes() {
    let original = source(SnapshotDataset::CachedDefinitions);
    let definition = cached();
    let proof = MssqlRelationalInputProof::for_cached(&original, &definition, vec![binding(1)]).unwrap();
    let mut paged = original.clone();
    paged.observation = SourceObservation::PagedSnapshot(PagedSnapshotDescription {
        dataset: SnapshotDataset::CachedDefinitions,
        observed_from: "2026-11-14T11:59:00Z".into(),
        sources: vec!["native-cache".into()],
    });
    assert!(proof.matches_cached(&paged, &definition));
    let mut completed = original.clone();
    let SourceObservation::Snapshot(snapshot) = &mut completed.observation else { panic!() };
    snapshot.observed_until = "2026-11-14T12:01:00Z".into();
    snapshot.records = 2;
    snapshot.sources = vec![StateSourceCoverage::observed("native-cache".into(), 2, true)];
    assert!(proof.matches_cached(&completed, &definition));
    for change in ["source", "target", "member", "epoch", "dataset", "unknown-member", "unknown-epoch", "target-scope"]
    {
        let mut changed = original.clone();
        match change {
            "source" => changed.source = "another-source".into(),
            "target" => {
                let CollectionTarget::Database(database) = &mut changed.target else { panic!() };
                database.database = "another-db".into();
            }
            "dataset" => {
                let SourceObservation::Snapshot(snapshot) = &mut changed.observation else { panic!() };
                snapshot.dataset = SnapshotDataset::RetainedPlans;
            }
            "target-scope" => changed.partition = SourcePartition::Target,
            _ => {
                let SourcePartition::InstanceLocal { member, epoch } = &mut changed.partition else { panic!() };
                match change {
                    "member" => member.as_mut().unwrap().native = NativeIdentity::new("member-b"),
                    "epoch" => *epoch = Some("native-generation-b".into()),
                    "unknown-member" => *member = None,
                    _ => *epoch = None,
                }
            }
        }
        assert!(!proof.matches_cached(&changed, &definition), "{change}");
    }
}

#[test]
fn every_native_and_redacted_payload_header_and_original_module_is_bound_without_copying_private_text() {
    let source = source(SnapshotDataset::CachedDefinitions);
    let mut original = cached();
    original.relational_input_proof =
        Some(MssqlRelationalInputProof::for_cached(&source, &original, vec![binding(1)]).unwrap());
    assert!(original.is_valid());
    for change in [
        "plan-handle",
        "sql-handle",
        "generation",
        "created",
        "database",
        "module",
        "native-sql",
        "native-plan",
        "redacted-sql",
        "redacted-plan",
        "bytes",
        "binding",
    ] {
        let mut changed = original.clone();
        match change {
            "plan-handle" => changed.context.identity.plan_handle = "0x0103".into(),
            "sql-handle" => {
                changed.context.identity.sql_handle = "0x0305".into();
                changed.module_parameters.as_mut().unwrap().sql_handle = "0x0305".into();
            }
            "generation" => changed.context.identity.plan_generation = "3".into(),
            "created" => changed.context.identity.created_without_offset = "2026-11-14T12:00:00.007".into(),
            "database" => {
                changed.context.database_id = 8;
                changed.module_parameters.as_mut().unwrap().database_id = 8;
            }
            "module" => changed.module_parameters.as_mut().unwrap().object_id = 1002,
            "native-sql" => changed.statement.native_sha256 = Some(digest(b"another original native statement")),
            "native-plan" => changed.estimated_plan.native_sha256 = Some(digest(b"another original native plan")),
            "redacted-sql" => {
                changed.statement.payload = DefinitionPayload::sql("SELECT value FROM @Other", crate::Engine::Mssql)
            }
            "redacted-plan" => {
                changed.estimated_plan.payload = DefinitionPayload::plan("<ShowPlanXML />", crate::Engine::Mssql)
            }
            "bytes" => changed.estimated_plan.source_utf16_bytes = Some("10000".into()),
            _ => changed.relational_input_proof.as_mut().unwrap().bindings[0].node_id = 2,
        }
        assert!(!changed.is_valid(), "{change}");
    }
    let encoded = serde_json::to_string(original.relational_input_proof.as_ref().unwrap()).unwrap();
    for private in ["private-db", "private-endpoint", "SELECT", "RowsType", "9264", "member-a"] {
        assert!(!encoded.contains(private), "{private}");
    }
    let before = original.clone();
    original.redact(crate::Engine::Mssql);
    assert_eq!(original, before);
    assert!(original.is_valid());
}

#[test]
fn bindings_are_positive_unique_bounded_and_not_reinterpreted_as_complete_input_measurements() {
    let source = source(SnapshotDataset::CachedDefinitions);
    let definition = cached();
    assert!(MssqlRelationalInputProof::for_cached(&source, &definition, vec![]).is_err());
    assert!(MssqlRelationalInputProof::for_cached(&source, &definition, vec![binding(1), binding(1)]).is_err());
    let mut unsupported = binding(1);
    unsupported.input =
        RelationalInput { kind: RelationalInputKind::Unknown, provenance: RelationalInputProvenance::Unresolved };
    assert!(MssqlRelationalInputProof::for_cached(&source, &definition, vec![unsupported]).is_err());
    let maximum = (0..MAX_MSSQL_RELATIONAL_INPUT_BINDINGS as u32).map(|at| binding(u32::MAX - at)).collect();
    let proof = MssqlRelationalInputProof::for_cached(&source, &definition, maximum).unwrap();
    assert!(proof.is_valid());
    let bytes = serde_json::to_vec(&proof).unwrap().len();
    assert_eq!(bytes, 6_962, "largest supported kinds and ten-digit native NodeIds fit the proof envelope");
    assert!(bytes <= MAX_MSSQL_RELATIONAL_INPUT_PROOF_BYTES);
    let mut encoded = serde_json::to_value(&proof).unwrap();
    encoded["bindings"].as_array_mut().unwrap().push(serde_json::to_value(binding(1000)).unwrap());
    assert!(serde_json::from_value::<MssqlRelationalInputProof>(encoded).is_err());
}

#[test]
fn retained_and_cached_proofs_are_distinct_and_missing_payloads_never_reuse_positive_tvp_authority() {
    let cached = cached();
    let mut retained = MssqlRetainedPlanDefinition {
        context: cached.context.clone(),
        statement: cached.statement.clone(),
        module_parameters: cached.module_parameters.clone(),
        relational_input_proof: None,
        parameter_context_proof: None,
        statement_executions: Some("9007199254740993".into()),
        last_start_without_offset: None,
        database_last_query_plan_stats: None,
        batch_plan: cached.estimated_plan.clone(),
    };
    let source = source(SnapshotDataset::RetainedPlans);
    let proof = MssqlRelationalInputProof::for_retained(&source, &retained, vec![binding(1)]).unwrap();
    assert!(!proof.matches_cached_contents(&cached));
    retained.relational_input_proof = Some(proof.clone());
    assert!(retained.is_valid());
    retained.statement_executions = Some("9007199254740994".into());
    assert!(proof.matches_retained(&source, &retained), "classification does not authenticate an execution population");
    retained.statement.payload = DefinitionPayload::Unavailable { reason: PayloadUnavailable::Restricted };
    assert!(!retained.is_valid());
    retained.relational_input_proof = None;
    assert!(retained.is_valid());
}
