//! Controlled original native facts for replay conformance. This declares evidence; it implements no classifier.
use super::*;
use culprit_core::plan_facts::{RelationalInput, RelationalInputKind, RelationalInputProvenance};

pub fn object_definition_bound_join_evidence() -> Vec<(CollectionDescription, Vec<TelemetryRecord>)> {
    let [_, (sql, raw_xml)] = hydration_native_inputs();
    let derived = sql.replacen(
        "SELECT r.value, o.id FROM @Rows AS r",
        "SELECT d.value, o.id FROM (SELECT * FROM @Rows AS r) AS d",
        1,
    );
    assert_ne!(derived, sql, "the closed wrapper must exercise a different original statement");
    let derived_xml = raw_xml.replace(&sql, &derived);
    assert_ne!(derived_xml, raw_xml, "the native statement and original payload must describe the same wrapper");
    let chain = sql.replacen(
        "SELECT r.value, o.id FROM @Rows AS r",
        "WITH seed AS (SELECT r.value FROM @Rows AS r), input AS (SELECT p.value FROM seed AS p) SELECT d.value, o.id FROM input AS d",
        1,
    );
    assert_ne!(chain, sql, "the closed CTE chain must be a distinct original statement");
    let chain_xml = raw_xml.replace(&sql, &chain);
    assert_ne!(chain_xml, raw_xml, "the original native statement must describe that exact chain");
    [(sql, raw_xml), (derived, derived_xml), (chain, chain_xml)]
        .into_iter()
        .flat_map(|(sql, xml)| [false, true].into_iter().map(move |retained| bound_join(&sql, &xml, retained)))
        .collect()
}

fn bound_join(sql: &str, raw_xml: &str, retained: bool) -> (CollectionDescription, Vec<TelemetryRecord>) {
    let dataset = if retained { SnapshotDataset::RetainedPlans } else { SnapshotDataset::CachedDefinitions };
    let source = qualified_source(dataset, 1, SourceState::Collected);
    let mut definition = cached_definition();
    definition.context.query_hash = Some("0x1023456789ABCDEF".into());
    definition.statement = cached_payload(sql, DefinitionPayload::sql);
    definition.context.source_text_bytes = definition.statement.source_utf16_bytes.clone();
    let xml = if retained { retained_xml(raw_xml) } else { raw_xml.into() };
    definition.estimated_plan = cached_payload(&xml, DefinitionPayload::plan);
    let current_schema = current_type_schema(&definition);
    let bindings = vec![MssqlRelationalInputBinding {
        node_id: 1,
        input: RelationalInput {
            kind: RelationalInputKind::TableValuedParameter,
            provenance: RelationalInputProvenance::NativeParameterMetadata,
        },
    }];
    let fact = if retained {
        let mut retained = MssqlRetainedPlanDefinition {
            context: definition.context,
            statement: definition.statement,
            module_parameters: definition.module_parameters,
            current_type_schema: None,
            relational_input_proof: None,
            parameter_context_proof: None,
            statement_executions: Some("9007199254740993".into()),
            last_start_without_offset: Some("2020-01-01T11:59:00.003".into()),
            database_last_query_plan_stats: Some(true),
            batch_plan: definition.estimated_plan,
        };
        retained.relational_input_proof =
            Some(MssqlRelationalInputProof::for_retained(&source, &retained, bindings).unwrap());
        retained.current_type_schema = Some(current_schema);
        ObjectDefinitionFact::MssqlRetainedPlan(Box::new(retained))
    } else {
        definition.relational_input_proof =
            Some(MssqlRelationalInputProof::for_cached(&source, &definition, bindings).unwrap());
        definition.current_type_schema = Some(current_schema);
        ObjectDefinitionFact::MssqlCachedDefinition(Box::new(definition))
    };
    let record = TelemetryRecord::ObjectDefinition(
        ObjectDefinitionRecord::new("native-object-definitions".into(), fact).unwrap(),
    );
    (source, vec![record])
}

fn current_type_schema(definition: &MssqlCachedDefinition) -> MssqlCurrentTypeSchema {
    let module = definition.module_parameters.as_ref().unwrap();
    let (parameter, declared_type) = module.table_parameters.iter().next().unwrap();
    let [schema, name] = declared_type.as_slice() else { panic!("qualified native table type") };
    MssqlCurrentTypeSchema {
        observed_at: "2020-01-01T12:00:00.7500000Z".into(),
        status: MssqlCurrentTypeSchemaStatus::Available,
        module: Some(MssqlCurrentTypeModule {
            identity: MssqlObjectIdentity {
                database_id: module.database_id,
                object_id: module.object_id.try_into().unwrap(),
                created_without_offset: "2020-01-01T10:00:00.003".into(),
            },
            schema: "dbo".into(),
            name: "read_rows".into(),
            modified_without_offset: "2020-01-01T11:30:00.007".into(),
        }),
        parameters: vec![MssqlCurrentTypeParameter { parameter_id: 1, name: parameter.clone(), user_type_id: 257 }],
        types: vec![MssqlCurrentTableType {
            schema_id: 1,
            user_type_id: 257,
            schema: schema.clone(),
            name: name.clone(),
            backing_object: MssqlObjectIdentity {
                database_id: module.database_id,
                object_id: 2001,
                created_without_offset: "2020-01-01T09:00:00.003".into(),
            },
            modified_without_offset: "2020-01-01T09:00:00.003".into(),
            memory_optimized: false,
            columns: vec![
                MssqlCurrentTypeColumn {
                    column_id: 1,
                    name: "value".into(),
                    user_type_id: 231,
                    system_type_id: 231,
                    type_schema: "sys".into(),
                    type_name: "nvarchar".into(),
                    max_length: 128,
                    precision: 0,
                    scale: 0,
                    nullable: false,
                    identity: false,
                    computed: false,
                    collation: Some("Latin1_General_100_BIN2".into()),
                },
                MssqlCurrentTypeColumn {
                    column_id: 2,
                    name: "payload".into(),
                    user_type_id: 165,
                    system_type_id: 165,
                    type_schema: "sys".into(),
                    type_name: "varbinary".into(),
                    max_length: -1,
                    precision: 0,
                    scale: 0,
                    nullable: true,
                    identity: false,
                    computed: false,
                    collation: None,
                },
                MssqlCurrentTypeColumn {
                    column_id: 3,
                    name: "amount".into(),
                    user_type_id: 106,
                    system_type_id: 106,
                    type_schema: "sys".into(),
                    type_name: "decimal".into(),
                    max_length: 9,
                    precision: 18,
                    scale: 3,
                    nullable: true,
                    identity: false,
                    computed: false,
                    collation: None,
                },
            ],
            indexes: vec![MssqlCurrentTypeIndex {
                index_id: 2,
                name: "ix_rows_value".into(),
                type_code: 2,
                native_type: "NONCLUSTERED".into(),
                unique: true,
                primary_key: false,
                unique_constraint: false,
                disabled: false,
                hypothetical: false,
                columns: vec![
                    MssqlCatalogIndexColumn {
                        index_column_id: 1,
                        column_id: 1,
                        name: "value".into(),
                        key_ordinal: 1,
                        descending: true,
                        included: false,
                    },
                    MssqlCatalogIndexColumn {
                        index_column_id: 2,
                        column_id: 2,
                        name: "payload".into(),
                        key_ordinal: 0,
                        descending: false,
                        included: true,
                    },
                ],
            }],
        }],
    }
}

fn retained_xml(raw: &str) -> String {
    let access = r#"<RelOp NodeId="1" PhysicalOp="Table Scan" LogicalOp="Table Scan" EstimateRows="100" EstimatedTotalSubtreeCost="1">"#;
    // The retained native fragment records this operator only. Other operators have no fabricated runtime rows.
    let measured = r#"<RelOp NodeId="1" PhysicalOp="Table Scan" LogicalOp="Table Scan" EstimateRows="100" EstimatedRowsRead="100" Parallel="false" EstimatedTotalSubtreeCost="1"><RunTimeInformation><RunTimeCountersPerThread Thread="0" ActualRows="10000" ActualRowsRead="20000" ActualExecutions="1" ActualEndOfScans="1" /></RunTimeInformation>"#;
    assert!(raw.contains(access));
    raw.replace(access, measured)
}
