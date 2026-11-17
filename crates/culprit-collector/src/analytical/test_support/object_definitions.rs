//! One source-owned definition population shared by warehouse and SDK tests, without native work.
use super::*;
use culprit_core::{Engine, MeasurementScope, SourceState};

mod bound_joins;
pub use bound_joins::object_definition_bound_join_evidence;
mod memory_feedback;
pub use memory_feedback::{MemoryFeedbackCase, memory_feedback_evidence, memory_feedback_evidence_cases};
mod parameters;
pub use parameters::{ParameterEvidenceCase, parameter_evidence, parameter_evidence_cases};
mod page_server_io;
pub use page_server_io::{PageServerIoCase, page_server_io_evidence};
mod tvp_loops;
pub use tvp_loops::{TvpLoopCase, tvp_loop_evidence, tvp_loop_evidence_cases};

const OBSERVED_FROM: &str = "2020-01-01T12:00:00Z";
const OBSERVED_UNTIL: &str = "2020-01-01T12:00:01Z";
const SQL: &str = "SELECT value FROM @Rows WHERE value = 9264";
const RETAINED_PLAN: &str = r#"<ShowPlanXML><BatchSequence><Batch><Statements><StmtSimple StatementText="SELECT value FROM @Rows WHERE value = 9264" QueryHash="0x0123456789ABCDEF" StatementType="SELECT"><QueryPlan><RelOp NodeId="1" PhysicalOp="Table Scan" LogicalOp="Table Scan" EstimateRows="100"><RunTimeInformation><RunTimeCountersPerThread Thread="0" ActualRows="10000" ActualExecutions="1" /></RunTimeInformation><TableScan><Object Table="[@Rows]" /></TableScan></RelOp></QueryPlan></StmtSimple></Statements></Batch></BatchSequence></ShowPlanXML>"#;

const ESTIMATED_PLAN: &str = r#"<ShowPlanXML><BatchSequence><Batch><Statements><StmtSimple StatementText="SELECT value FROM @Rows WHERE value = 9264" QueryHash="0x0123456789ABCDEF" StatementType="SELECT"><QueryPlan><RelOp NodeId="1" PhysicalOp="Table Scan" LogicalOp="Table Scan" EstimateRows="100"><TableScan><Object Table="[@Rows]" /></TableScan></RelOp></QueryPlan></StmtSimple></Statements></Batch></BatchSequence></ShowPlanXML>"#;

/// The first case has native module metadata; the second is retained XML without any borrowed module header.
/// Later cases cover generic formats, changed definition versions and independent native visibility states.
pub fn object_definition_evidence_cases() -> Vec<(CollectionDescription, Vec<TelemetryRecord>)> {
    let cached = cached_definition();
    let mut unavailable = cached.clone();
    unavailable.context.identity.plan_generation = "2".into();
    unavailable.module_parameters = None;
    unavailable.statement = missing_payload(PayloadUnavailable::Missing);
    unavailable.estimated_plan = missing_payload(PayloadUnavailable::Restricted);
    let mut oversized = cached.clone();
    oversized.context.identity.plan_generation = "3".into();
    oversized.module_parameters = None;
    oversized.statement = missing_payload(PayloadUnavailable::Oversized);
    oversized.statement.source_utf16_bytes = Some("1000000".into());
    let mut cases = vec![case(
        Engine::Mssql,
        SnapshotDataset::CachedDefinitions,
        vec![
            ObjectDefinitionFact::MssqlCachedDefinition(Box::new(cached.clone())),
            ObjectDefinitionFact::MssqlCachedDefinition(Box::new(unavailable)),
            ObjectDefinitionFact::MssqlCachedDefinition(Box::new(oversized)),
        ],
        SourceState::Collected,
    )];
    let retained = MssqlRetainedPlanDefinition {
        relational_input_proof: None,
        parameter_context_proof: None,
        context: cached.context.clone(),
        statement: missing_payload(PayloadUnavailable::NotCollected),
        module_parameters: None,
        statement_executions: Some("9007199254740993".into()),
        last_start_without_offset: Some("2020-01-01T11:59:00.003".into()),
        database_last_query_plan_stats: Some(true),
        batch_plan: cached_payload(RETAINED_PLAN, DefinitionPayload::plan),
    };
    cases.push(case(
        Engine::Mssql,
        SnapshotDataset::RetainedPlans,
        vec![ObjectDefinitionFact::MssqlRetainedPlan(Box::new(retained))],
        SourceState::Collected,
    ));
    for engine in [Engine::Postgres, Engine::Mysql] {
        cases.push(generic_case(engine));
    }
    let mut changed = cached.clone();
    changed.module_parameters = None;
    cases.push(case(
        Engine::Mssql,
        SnapshotDataset::CachedDefinitions,
        vec![ObjectDefinitionFact::MssqlCachedDefinition(Box::new(changed))],
        SourceState::Collected,
    ));
    cases.push(case(
        Engine::Mssql,
        SnapshotDataset::CachedDefinitions,
        vec![ObjectDefinitionFact::MssqlCachedDefinition(Box::new(cached))],
        SourceState::Partial,
    ));
    for state in [SourceState::Empty, SourceState::Unavailable] {
        cases.push(case(Engine::Mssql, SnapshotDataset::CachedDefinitions, vec![], state));
    }
    cases
}

/// A valid full native body whose warehouse preview cannot reconstruct an exact record. Projection-only reads keep
/// an explicit gap; reading the retained payload requires the caller's separate recovery opt-in.
pub fn object_definition_recovery_only_evidence() -> (CollectionDescription, Vec<TelemetryRecord>) {
    let sql =
        format!("SELECT {} FROM @Rows", (0..500).map(|index| format!("column_{index}")).collect::<Vec<_>>().join(","));
    let mut definition = cached_definition();
    definition.statement = cached_payload(&sql, DefinitionPayload::sql);
    definition.context.source_text_bytes = definition.statement.source_utf16_bytes.clone();
    definition.estimated_plan = missing_payload(PayloadUnavailable::NotCollected);
    case(
        Engine::Mssql,
        SnapshotDataset::CachedDefinitions,
        vec![ObjectDefinitionFact::MssqlCachedDefinition(Box::new(definition))],
        SourceState::Collected,
    )
}

/// Original native-style input shared by positive and unavailable-provenance cases, before privacy runs.
fn hydration_native_inputs() -> [(String, String); 2] {
    let direct_xml = format!(
        r#"<ShowPlanXML><BatchSequence><Batch><Statements>
        <StmtSimple StatementText="{SQL}" QueryHash="0x0123456789ABCDEF" StatementType="SELECT" StatementSubTreeCost="0.01">
        <QueryPlan CachedPlanSize="16" CompileTime="1" CompileCPU="1" CompileMemory="128">
        <RelOp NodeId="0" PhysicalOp="Table Scan" LogicalOp="Table Scan" EstimateRows="100" EstimateIO="0" EstimateCPU="0.001" AvgRowSize="11" EstimatedTotalSubtreeCost="0.01" Parallel="false" EstimatedExecutionMode="Row">
        <OutputList><ColumnReference Table="[@Rows]" Column="value" /></OutputList>
        <TableScan Ordered="false" Storage="RowStore"><DefinedValues>
        <DefinedValue><ColumnReference Table="[@Rows]" Column="value" /></DefinedValue></DefinedValues>
        <Object Table="[@Rows]" /><Predicate><ScalarOperator ScalarString="[@Rows].[value]=(9264)">
        <Compare CompareOp="EQ"><ScalarOperator><Identifier><ColumnReference Table="[@Rows]" Column="value" /></Identifier></ScalarOperator>
        <ScalarOperator><Const ConstValue="(9264)" /></ScalarOperator></Compare>
        </ScalarOperator></Predicate></TableScan></RelOp></QueryPlan></StmtSimple>
        </Statements></Batch></BatchSequence></ShowPlanXML>"#
    );
    let sql = "SELECT r.value, o.id FROM @Rows AS r CROSS JOIN dbo.orders AS o WHERE o.customer_id = 9264";
    let join_xml = format!(
        r#"<ShowPlanXML><BatchSequence><Batch><Statements>
        <StmtSimple StatementText="{sql}" QueryHash="0x1023456789ABCDEF" StatementType="SELECT" StatementSubTreeCost="100">
        <QueryPlan><RelOp NodeId="0" PhysicalOp="Nested Loops" LogicalOp="Inner Join" EstimateRows="10000" EstimatedTotalSubtreeCost="100">
        <OutputList><ColumnReference Table="[@Rows]" Alias="[r]" Column="value" />
        <ColumnReference Database="[db]" Schema="[dbo]" Table="[orders]" Alias="[o]" Column="id" /></OutputList>
        <Warnings NoJoinPredicate="true" /><NestedLoops Optimized="false">
        <RelOp NodeId="1" PhysicalOp="Table Scan" LogicalOp="Table Scan" EstimateRows="100" EstimatedTotalSubtreeCost="1">
        <OutputList><ColumnReference Table="[@Rows]" Alias="[r]" Column="value" /></OutputList>
        <TableScan Ordered="false"><Object Table="[@Rows]" Alias="[r]" /></TableScan></RelOp>
        <RelOp NodeId="2" PhysicalOp="Index Seek" LogicalOp="Index Seek" EstimateRows="100" EstimatedTotalSubtreeCost="1">
        <OutputList><ColumnReference Database="[db]" Schema="[dbo]" Table="[orders]" Alias="[o]" Column="id" /></OutputList>
        <IndexScan Ordered="true"><Object Database="[db]" Schema="[dbo]" Table="[orders]" Index="[ix_orders_customer_id]" Alias="[o]" IndexKind="NonClustered" />
        <SeekPredicates><SeekPredicateNew><SeekKeys><Prefix ScanType="EQ">
        <RangeColumns><ColumnReference Database="[db]" Schema="[dbo]" Table="[orders]" Alias="[o]" Column="customer_id" /></RangeColumns>
        <RangeExpressions><ScalarOperator ScalarString="(9264)"><Const ConstValue="(9264)" /></ScalarOperator></RangeExpressions>
        </Prefix></SeekKeys></SeekPredicateNew></SeekPredicates></IndexScan></RelOp>
        </NestedLoops></RelOp></QueryPlan></StmtSimple>
        </Statements></Batch></BatchSequence></ShowPlanXML>"#
    );
    [(SQL.into(), direct_xml), (sql.into(), join_xml)]
}

/// A direct TVP and a join without retained namespace proof share one batch exceeding warehouse previews.
pub fn object_definition_hydration_evidence() -> (CollectionDescription, Vec<TelemetryRecord>) {
    let [(_, direct_xml), (sql, join_xml)] = hydration_native_inputs();
    let mut first = cached_definition();
    first.estimated_plan = cached_payload(&direct_xml, DefinitionPayload::plan);
    let mut second = cached_definition();
    second.context.identity.plan_handle = "0x11121314".into();
    second.context.identity.sql_handle = "0x15161718".into();
    second.context.query_hash = Some("0x1023456789ABCDEF".into());
    second.query_plan_hash = Some("0xEFDCBA9876543210".into());
    second.module_parameters.as_mut().unwrap().sql_handle = second.context.identity.sql_handle.clone();
    second.statement = cached_payload(&sql, DefinitionPayload::sql);
    second.context.source_text_bytes = second.statement.source_utf16_bytes.clone();
    second.estimated_plan = cached_payload(&join_xml, DefinitionPayload::plan);
    for definition in [&first, &second] {
        let DefinitionPayload::Available { text, .. } = &definition.estimated_plan.payload else {
            panic!("valid native XML")
        };
        assert!(text.len() > 1024 && text.len() < 32 * 1024);
    }
    case(
        Engine::Mssql,
        SnapshotDataset::CachedDefinitions,
        vec![
            ObjectDefinitionFact::MssqlCachedDefinition(Box::new(first)),
            ObjectDefinitionFact::MssqlCachedDefinition(Box::new(second)),
        ],
        SourceState::Collected,
    )
}

/// Synthetic same-read retained evidence, independent of the estimated-definition population above. The native
/// TableScan counters distinguish consumed input from output; the large plan requires authenticated hydration.
pub fn object_definition_retained_input_evidence() -> (CollectionDescription, Vec<TelemetryRecord>) {
    let context = CachedStatementContext {
        identity: CachedStatementIdentity {
            plan_handle: "0x11223344".into(),
            sql_handle: "0x55667788".into(),
            statement_start_bytes: 0,
            statement_end_bytes: -1,
            created_without_offset: "2020-01-01T11:30:00.003".into(),
            plan_generation: "2".into(),
        },
        database_id: 7,
        query_hash: Some("0x0123456789ABCDEF".into()),
        origin: StatementOrigin::Application,
        monitoring_template_id: None,
        source_text_bytes: Some((SQL.encode_utf16().count() * 2).to_string()),
        source_text_complete: true,
    };
    let module_parameters = MssqlModuleParameters {
        database_id: context.database_id,
        object_id: 2001,
        module_number: 1,
        sql_handle: context.identity.sql_handle.clone(),
        table_parameters: [("@Rows".into(), vec!["dbo".into(), "RowsType".into()])].into(),
    };
    let xml = format!(
        r#"<ShowPlanXML><BatchSequence><Batch><Statements>
        <StmtSimple StatementText="{SQL}" QueryHash="0x0123456789ABCDEF" StatementType="SELECT" StatementSubTreeCost="10">
        <QueryPlan><RelOp NodeId="0" PhysicalOp="Table Scan" LogicalOp="Table Scan" EstimateRows="100" EstimatedRowsRead="100" Parallel="false">
        <OutputList><ColumnReference Table="[@Rows]" Column="value" /></OutputList>
        <RunTimeInformation><RunTimeCountersPerThread Thread="0" ActualRows="1000" ActualRowsRead="10000" ActualExecutions="1" ActualEndOfScans="1" ActualRebinds="0" ActualRewinds="0" /></RunTimeInformation>
        <TableScan Ordered="false" Storage="RowStore"><DefinedValues>
        <DefinedValue><ColumnReference Table="[@Rows]" Column="value" /></DefinedValue></DefinedValues>
        <Object Table="[@Rows]" /><Predicate><ScalarOperator ScalarString="[@Rows].[value]=(9264)">
        <Compare CompareOp="EQ"><ScalarOperator><Identifier><ColumnReference Table="[@Rows]" Column="value" /></Identifier></ScalarOperator>
        <ScalarOperator><Const ConstValue="(9264)" /></ScalarOperator></Compare>
        </ScalarOperator></Predicate></TableScan></RelOp></QueryPlan></StmtSimple>
        </Statements></Batch></BatchSequence></ShowPlanXML>"#
    );
    let retained = MssqlRetainedPlanDefinition {
        relational_input_proof: None,
        parameter_context_proof: None,
        context,
        statement: cached_payload(SQL, DefinitionPayload::sql),
        module_parameters: Some(module_parameters),
        statement_executions: Some("9007199254740993".into()),
        last_start_without_offset: Some("2020-01-01T11:59:00.003".into()),
        database_last_query_plan_stats: Some(true),
        batch_plan: cached_payload(&xml, DefinitionPayload::plan),
    };
    case(
        Engine::Mssql,
        SnapshotDataset::RetainedPlans,
        vec![ObjectDefinitionFact::MssqlRetainedPlan(Box::new(retained))],
        SourceState::Collected,
    )
}

fn cached_definition() -> MssqlCachedDefinition {
    let context = CachedStatementContext {
        identity: CachedStatementIdentity {
            plan_handle: "0x01020304".into(),
            sql_handle: "0x05060708".into(),
            statement_start_bytes: 0,
            statement_end_bytes: -1,
            created_without_offset: "2020-01-01T11:00:00.003".into(),
            plan_generation: "1".into(),
        },
        database_id: 7,
        query_hash: Some("0x0123456789ABCDEF".into()),
        origin: StatementOrigin::Application,
        monitoring_template_id: None,
        source_text_bytes: Some((SQL.encode_utf16().count() * 2).to_string()),
        source_text_complete: true,
    };
    let module_parameters = MssqlModuleParameters {
        database_id: context.database_id,
        object_id: 1999,
        module_number: 1,
        sql_handle: context.identity.sql_handle.clone(),
        table_parameters: [("@Rows".into(), vec!["dbo".into(), "RowsType".into()])].into(),
    };
    MssqlCachedDefinition {
        relational_input_proof: None,
        context,
        query_plan_hash: Some("0xFEDCBA9876543210".into()),
        statement: cached_payload(SQL, DefinitionPayload::sql),
        estimated_plan: cached_payload(ESTIMATED_PLAN, DefinitionPayload::plan),
        module_parameters: Some(module_parameters),
    }
}

fn cached_payload(raw: &str, redact: fn(&str, Engine) -> DefinitionPayload) -> MssqlCachedPayload {
    MssqlCachedPayload {
        source_utf16_bytes: Some((raw.encode_utf16().count() * 2).to_string()),
        native_sha256: Some(digest(raw.as_bytes())),
        payload: redact(raw, Engine::Mssql),
    }
}

fn missing_payload(reason: PayloadUnavailable) -> MssqlCachedPayload {
    MssqlCachedPayload {
        source_utf16_bytes: None,
        native_sha256: None,
        payload: DefinitionPayload::Unavailable { reason },
    }
}

fn source(engine: Engine, dataset: SnapshotDataset, count: usize, state: SourceState) -> CollectionDescription {
    let mut target = fixture().0.source.target;
    let CollectionTarget::Database(database) = &mut target else { unreachable!() };
    database.engine = engine;
    let name = "native-object-definitions".to_owned();
    CollectionDescription {
        partition: culprit_core::telemetry::SourcePartition::Target,
        target,
        source: "object-definition-capture".into(),
        continuity: SourceContinuity::Unverified,
        observation: SourceObservation::Snapshot(StateSnapshotDescription {
            dataset,
            observed_from: OBSERVED_FROM.into(),
            observed_until: OBSERVED_UNTIL.into(),
            records: count as u32,
            sources: vec![if state == SourceState::Unavailable {
                StateSourceCoverage::unavailable(name, culprit_core::ErrorCode::PermissionDenied, None)
            } else {
                StateSourceCoverage::observed(name, count as u32, state != SourceState::Partial)
            }],
        }),
    }
}

fn qualified_source(dataset: SnapshotDataset, count: usize, state: SourceState) -> CollectionDescription {
    let mut source = source(Engine::Mssql, dataset, count, state);
    source.partition = SourcePartition::InstanceLocal {
        member: Some(SharedResourceIdentity {
            engine: Engine::Mssql,
            kind: SharedResourceKind::Instance,
            namespace: NativeIdentity::new("private-cache-machine-canary"),
            native: NativeIdentity::new("private-cache-instance-canary"),
        }),
        epoch: Some("native-cache-epoch-a".into()),
    };
    source
}

fn case(
    engine: Engine,
    dataset: SnapshotDataset,
    facts: Vec<ObjectDefinitionFact>,
    state: SourceState,
) -> (CollectionDescription, Vec<TelemetryRecord>) {
    let source = source(engine, dataset, facts.len(), state);
    let records = facts
        .into_iter()
        .map(|fact| {
            TelemetryRecord::ObjectDefinition(
                ObjectDefinitionRecord::new("native-object-definitions".into(), fact).unwrap(),
            )
        })
        .collect();
    (source, records)
}

fn generic_case(engine: Engine) -> (CollectionDescription, Vec<TelemetryRecord>) {
    let source = source(engine, SnapshotDataset::CachedDefinitions, 3, SourceState::Collected);
    let query = NativeQueryIdentity {
        query_id: "42".into(),
        qualifiers: [("generation".into(), "native-entry-1".into())].into(),
    };
    let query_id = query.definition_id(&source).unwrap();
    let statement = QueryDefinition {
        id: query_id.clone(),
        origin: StatementOrigin::Application,
        monitoring_template_id: None,
        identity: query.clone(),
        variant_family: None,
        context: [("database".into(), "db".into())].into(),
        context_available: true,
        source_text_bytes: None,
        sql: DefinitionPayload::sql("SELECT value FROM items WHERE id = 9264", engine),
    };
    let identity = NativePlanIdentity {
        plan_id: "native-plan-1".into(),
        query,
        qualifiers: [("generation".into(), "retained-plan-1".into())].into(),
    };
    let plan_id = identity.definition_id(&source).unwrap();
    let retained = NativeRetainedPlan {
        definition: PlanDefinition {
            id: plan_id.clone(),
            identity,
            query_definition_id: Some(query_id),
            engine_version: Some("fixture-native-version".into()),
            native_properties: Default::default(),
            source_text_bytes: None,
            plan: if engine == Engine::Postgres {
                DefinitionPayload::plan("[{\"Plan\":{\"Node Type\":\"Seq Scan\",\"Relation Name\":\"items\"}}]", engine)
            } else {
                DefinitionPayload::Unavailable { reason: PayloadUnavailable::NotCollected }
            },
        },
        counters: NativeCounterSet {
            identity: NativeIdentity::new(plan_id),
            scope: MeasurementScope::Database,
            counters: vec![NativeCounterSample {
                nature: CounterNature::Cumulative,
                ..NativeCounterSample::gauge(
                    "uses",
                    9_007_199_254_740_993,
                    CounterUnit::Count,
                    MeasurementScope::Database,
                    "native-plan-cache",
                    "uses",
                )
            }],
            properties: Default::default(),
        },
        first_used: Some("2020-01-01T12:00:00.100000000Z".into()),
        last_used: Some("2020-01-01T12:00:00.900000000Z".into()),
    };
    let catalog = NativeCatalogDefinition {
        identity: NativeIdentity::new("native-object-1").qualified("generation", "catalog-1"),
        object_type: "index".into(),
        properties: [("unique".into(), "1".into())].into(),
        definition: Some(CatalogExpression::sql(
            "CREATE INDEX items_value ON items(value)",
            engine,
            MAX_CATALOG_EXPRESSION_BYTES,
        )),
        predicate: Some(CatalogExpression::sql("value > 9264", engine, MAX_CATALOG_EXPRESSION_BYTES)),
    };
    case(
        engine,
        SnapshotDataset::CachedDefinitions,
        vec![
            ObjectDefinitionFact::Statement(Box::new(statement)),
            ObjectDefinitionFact::RetainedPlan(Box::new(retained)),
            ObjectDefinitionFact::NativeCatalog(Box::new(catalog)),
        ],
        SourceState::Collected,
    )
}

pub fn assert_object_definition_capture(
    capture: &crate::analytical::evidence::EvidenceCapture,
    source: &CollectionDescription,
    records: &[TelemetryRecord],
) {
    assert_operational_capture(capture, source, records);
    assert_eq!(capture.families(), &[crate::analytical::evidence::EvidenceFamily::ObjectDefinitions]);
    assert!(source.target.database().is_some());
    for verified in capture.records() {
        let TelemetryRecord::ObjectDefinition(row) = &verified.row().record else { panic!("exact native definition") };
        assert!(row.is_valid());
        assert_eq!(row.version, digest(&serde_json::to_vec(&row.fact).unwrap()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opaque_native_digests_do_not_count_as_sensitive_definition_text() {
        let original = "SELECT value FROM @Rows WHERE value = 9264 AND value <> 29";
        let mut native = cached_definition();
        native.statement = cached_payload(original, DefinitionPayload::sql);
        native.context.source_text_bytes = native.statement.source_utf16_bytes.clone();
        assert!(native.statement.native_sha256.as_ref().unwrap().contains("9264"));
        let record = TelemetryRecord::ObjectDefinition(
            ObjectDefinitionRecord::new(
                "native-object-definitions".into(),
                ObjectDefinitionFact::MssqlCachedDefinition(Box::new(native)),
            )
            .unwrap(),
        );
        assert!(record.is_valid());
        assert!(serde_json::to_string(&record).unwrap().contains("9264"));
        super::super::assert_redacted_definition_payloads(&record);
    }

    #[test]
    #[should_panic(expected = "unredacted fixture literal in /record/data/fact/data/statement/payload")]
    fn an_available_payload_with_valid_hashes_still_fails_when_its_sensitive_literal_survives() {
        let mut native = cached_definition();
        native.statement.payload = DefinitionPayload::Available {
            text: SQL.into(),
            sha256: digest(SQL.as_bytes()),
            redaction_version: REDACTION_VERSION,
        };
        let record = TelemetryRecord::ObjectDefinition(
            ObjectDefinitionRecord::new(
                "native-object-definitions".into(),
                ObjectDefinitionFact::MssqlCachedDefinition(Box::new(native)),
            )
            .unwrap(),
        );
        assert!(record.is_valid());
        super::super::assert_redacted_definition_payloads(&record);
    }
}
