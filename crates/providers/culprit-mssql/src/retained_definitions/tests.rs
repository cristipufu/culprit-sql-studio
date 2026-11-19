use super::*;
use crate::{
    read::ReadError,
    resources::test_support::{FakeRead, replace},
    value::Val,
};
mod parameters;
pub(crate) fn feature(visible: i64, present: i64) -> Rec {
    Rec::of(&[
        ("metadata_visibility", Val::Int(visible)),
        ("last_actual", Val::Int(present)),
        ("live_xml", Val::Int(1)),
        ("profiles", Val::Int(1)),
    ])
}

#[test]
fn oversized_optional_native_binding_rosters_keep_retained_sql_plan_and_statement_counters() {
    let source = crate::plan_xml::test_source(SnapshotDataset::RetainedPlans);
    let layout = Layout {
        maximum_bytes: 128 * 1024,
        maximum_statement_bytes: retained::MAX_STATEMENT_BYTES,
        database_setting: Some(true),
    };
    for count in [MAX_MSSQL_RELATIONAL_INPUT_BINDINGS, MAX_MSSQL_RELATIONAL_INPUT_BINDINGS + 1] {
        let row = crate::cached_definitions::tests::joined_parameter_row(count);
        let mut definition = decode(&row, &layout, Some(&source)).unwrap();
        assert!(definition.statement.payload.is_available());
        assert!(definition.batch_plan.payload.is_available());
        assert!(definition.module_parameters.is_some());
        assert!(definition.statement_executions.is_some());
        if count == MAX_MSSQL_RELATIONAL_INPUT_BINDINGS {
            let proof = definition.relational_input_proof.as_ref().expect("the largest supported native roster");
            assert_eq!(proof.bindings.len(), count);
            assert_eq!(serde_json::to_vec(proof).unwrap().len(), 6_962);
            assert!(proof.matches_retained(&source, &definition));
        } else {
            assert!(definition.relational_input_proof.is_none(), "unavailable proof must not discard native bodies");
        }
        definition.relational_input_proof = None;
        assert_eq!(definition, decode(&row, &layout, None).unwrap());
        assert!(decode(&replace(&row, &[("plan_bytes", Val::Int(2))]), &layout, Some(&source)).is_err());
    }
}

#[tokio::test]
async fn unsupported_hidden_and_failed_feature_reads_cannot_masquerade_as_an_empty_plan_cache() {
    let mut absent = FakeRead::new(vec![Ok(vec![feature(1, 0)])]);
    assert!(layout(&mut absent, &CollectionBudget::default()).await.unwrap().is_none());
    assert_eq!(absent.calls.len(), 1);
    let mut hidden = FakeRead::new(vec![Ok(vec![feature(0, 0)])]);
    assert!(layout(&mut hidden, &CollectionBudget::default()).await.is_err());
    assert_eq!(hidden.calls.len(), 1);
    let mut failed = FakeRead::new(vec![Ok(vec![feature(1, 1)]), Err(ReadError::Stopped(CollectionStop::TimedOut))]);
    assert!(layout(&mut failed, &CollectionBudget::default()).await.is_err());
    assert_eq!(failed.calls.len(), 2);
}
#[test]
fn retained_exports_share_native_decoding_and_do_not_claim_a_batch_execution_count_or_utc_clock() {
    let layout = Layout {
        maximum_bytes: 4096,
        maximum_statement_bytes: retained::MAX_STATEMENT_BYTES,
        database_setting: Some(false),
    };
    let row =
        replace(&crate::cached_definitions::tests::row(), &[("execution_count", Val::Int(9_007_199_254_740_993))]);
    let native = decode(&row, &layout, None).unwrap();
    assert!(native.batch_plan.payload.is_available());
    assert_eq!(native.database_last_query_plan_stats, Some(false));
    assert_eq!(native.statement_executions.as_deref(), Some("9007199254740993"));
    assert_eq!(native.last_start_without_offset.as_deref(), Some("2020-01-01T08:00:00.0030000"));
    assert!(!serde_json::to_string(&native).unwrap().contains("private-value"));
    let unsupported = decode(&replace(&row, &[("plan_handle", Val::Bytes(vec![0; 64]))]), &layout, None).unwrap();
    assert!(unsupported.statement_executions.is_none());
    assert!(select_sql(&layout).contains(cached_statements::DATABASE_PREDICATE));
}

pub(crate) fn tvp_row() -> Rec {
    let statement = "SELECT id FROM @Rows WHERE id > 10";
    let header = format!("CREATE PROC dbo.consume @Rows dbo.RowsType READONLY AS {statement}");
    let start = header.find("SELECT").unwrap();
    let xml = format!(
        r#"<ShowPlanXML><BatchSequence><Batch><Statements><StmtSimple StatementText="{statement}" QueryHash="0x0123456789ABCDEF" StatementSubTreeCost="1"><QueryPlan>
        <RelOp NodeId="0" PhysicalOp="Table Scan" LogicalOp="Table Scan" Parallel="0" EstimateRows="100" EstimatedRowsRead="100" EstimatedTotalSubtreeCost="1">
        <RunTimeInformation><RunTimeCountersPerThread Thread="0" ActualRows="1000" ActualRowsRead="10000" ActualExecutions="1" ActualEndOfScans="1" /></RunTimeInformation>
        <TableScan><Object Table="[@Rows]" /><Predicate><ScalarOperator ScalarString="[@Rows].[id]&gt;(10)" /></Predicate></TableScan>
        </RelOp></QueryPlan></StmtSimple></Statements></Batch></BatchSequence></ShowPlanXML>"#
    );
    replace(
        &crate::cached_definitions::tests::row(),
        &[
            ("statement_start_offset", Val::Int((header[..start].encode_utf16().count() * 2) as i64)),
            ("statement_end_offset", Val::Int(-1)),
            ("source_text_bytes", Val::Int((header.encode_utf16().count() * 2) as i64)),
            ("text_prefix", Val::Text(header)),
            ("text_complete", Val::Int(1)),
            ("module_database_id", Val::Int(5)),
            ("module_object_id", Val::Int(1001)),
            ("module_number", Val::Int(1)),
            ("plan_bytes", Val::Int((xml.encode_utf16().count() * 2) as i64)),
            ("query_plan", Val::Text(xml)),
        ],
    )
}

#[test]
fn same_read_original_module_and_selected_statement_reach_actual_tvp_analysis_without_borrowing_other_definitions() {
    let layout = Layout {
        maximum_bytes: 4096,
        maximum_statement_bytes: retained::MAX_STATEMENT_BYTES,
        database_setting: Some(true),
    };
    let definition = decode(&tvp_row(), &layout, None).unwrap();
    assert!(definition.statement.payload.is_available());
    assert!(definition.module_parameters.is_some());
    let plan = crate::plan_xml::parse_retained_definition(
        &definition,
        &crate::plan_xml::test_source(culprit_core::telemetry::SnapshotDataset::RetainedPlans),
    )
    .unwrap();
    assert_eq!(
        plan.root.facts().relational_input.unwrap().qualified_kind(),
        culprit_core::plan_facts::RelationalInputKind::TableValuedParameter
    );
    assert_eq!(plan.root.act_rows, Some(1000.0));
    assert_eq!(plan.root.facts().access_rows.unwrap().actual_total, Some(10000.0));
    assert!(plan.root.facts().scan_completion.as_ref().unwrap().is_single_completed_serial_scan());
    let insight =
        culprit_core::insights::insights(&plan).into_iter().find(|insight| insight.rule == "misestimate").unwrap();
    assert!(insight.detail.contains(&culprit_core::t!(
        "insight.relational_input.completed_input_counts",
        estimated = "100",
        actual = "10K"
    )));
    assert!(insight.fix_sql.is_none());

    for field in ["module", "statement", "scope"] {
        let mut missing = definition.clone();
        match field {
            "module" => missing.module_parameters = None,
            "statement" => {
                missing.statement.payload = DefinitionPayload::Unavailable { reason: PayloadUnavailable::NotCollected }
            }
            "scope" => missing.module_parameters.as_mut().unwrap().database_id += 1,
            _ => unreachable!(),
        }
        if field == "scope" {
            assert!(
                crate::plan_xml::parse_retained_definition(
                    &missing,
                    &crate::plan_xml::test_source(culprit_core::telemetry::SnapshotDataset::RetainedPlans)
                )
                .is_err()
            );
        } else {
            let unknown = crate::plan_xml::parse_retained_definition(
                &missing,
                &crate::plan_xml::test_source(culprit_core::telemetry::SnapshotDataset::RetainedPlans),
            )
            .unwrap();
            assert_eq!(
                unknown.root.facts().relational_input.unwrap().qualified_kind(),
                culprit_core::plan_facts::RelationalInputKind::Unknown
            );
            assert_eq!(unknown.root.act_rows, Some(1000.0));
        }
    }
}

#[tokio::test]
async fn statement_retention_intersects_the_existing_budget_and_never_adds_a_native_read() {
    let budget = CollectionBudget { max_sql_bytes: 16, ..Default::default() };
    let mut io = FakeRead::new(vec![Ok(vec![feature(1, 1)]), Ok(vec![Rec::of(&[("value", Val::Int(1))])])]);
    let layout = layout(&mut io, &budget).await.unwrap().unwrap();
    assert_eq!(io.calls.len(), 2);
    assert_eq!(layout.maximum_statement_bytes, 16);
    assert!(
        (layout.maximum_bytes + layout.maximum_statement_bytes) * JSON_BYTES_PER_UTF16_BYTE + RECORD_METADATA_BYTES
            <= budget.page_bytes as usize
    );
    let record = decode(&tvp_row(), &layout, None).unwrap();
    assert!(matches!(
        record.statement.payload,
        DefinitionPayload::Unavailable { reason: PayloadUnavailable::Oversized }
    ));
    assert!(record.batch_plan.payload.is_available());
    let plan = crate::plan_xml::parse_retained_definition(
        &record,
        &crate::plan_xml::test_source(culprit_core::telemetry::SnapshotDataset::RetainedPlans),
    )
    .unwrap();
    assert_eq!(
        plan.root.facts().relational_input.unwrap().qualified_kind(),
        culprit_core::plan_facts::RelationalInputKind::Unknown
    );
    let sql = select_sql(&layout);
    assert_eq!(sql.matches("sys.dm_exec_query_plan_stats(").count(), 1);
    assert!(sql.contains("sys.parameters") && sql.contains("sys.table_types"));
}
