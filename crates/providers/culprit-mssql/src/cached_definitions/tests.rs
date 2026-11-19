use super::*;
use crate::{resources::test_support::replace, value::Val};
pub(crate) const XML: &str = "<ShowPlanXML><BatchSequence><Batch><Statements><StmtSimple StatementText=\"SELECT 'private-value'\" /></Statements></Batch></BatchSequence></ShowPlanXML>";
pub(crate) fn row() -> Rec {
    let sql = "SELECT 'private-value' AS canary";
    let mut fields =
        crate::grants::tests::row().columns().map(|(name, value)| (name.to_owned(), value.clone())).collect::<Vec<_>>();
    fields.extend([
        ("statement_bytes".into(), Val::Int(sql.encode_utf16().count() as i64 * 2)),
        ("statement_text".into(), Val::Text(sql.into())),
        ("plan_bytes".into(), Val::Int(XML.encode_utf16().count() as i64 * 2)),
        ("query_plan".into(), Val::Text(XML.into())),
        ("encrypted".into(), Val::Bool(false)),
        ("module_database_id".into(), Val::Null),
        ("module_object_id".into(), Val::Null),
        ("module_number".into(), Val::Null),
        ("current_type_observed_at".into(), Val::Null),
        ("current_type_visibility".into(), Val::Null),
        ("current_type_bytes".into(), Val::Null),
        ("current_type_schema".into(), Val::Null),
        ("query_plan_hash".into(), Val::Text("0x0000000000000009".into())),
        ("last_start_without_offset".into(), Val::Text("2020-01-01T08:00:00.003".into())),
    ]);
    Rec::of(&fields.iter().map(|(name, value)| (name.as_str(), value.clone())).collect::<Vec<_>>())
}

pub(crate) fn module_row(sql: &str, xml: &str) -> Rec {
    let base = row();
    let context = crate::cached_statements::context(&base).unwrap();
    let prefix = "CREATE PROCEDURE dbo.consume @Rows dbo.RowsType READONLY AS ";
    let header = format!("{prefix}{sql}");
    replace(
        &base,
        &[
            ("statement_start_offset", Val::Int((prefix.encode_utf16().count() * 2) as i64)),
            ("statement_end_offset", Val::Int(-1)),
            ("source_text_bytes", Val::Int((header.encode_utf16().count() * 2) as i64)),
            ("text_prefix", Val::Text(header)),
            ("text_complete", Val::Int(1)),
            ("module_database_id", Val::Int(context.database_id.into())),
            ("module_object_id", Val::Int(1001)),
            ("module_number", Val::Int(1)),
            ("statement_bytes", Val::Int((sql.encode_utf16().count() * 2) as i64)),
            ("statement_text", Val::Text(sql.into())),
            ("plan_bytes", Val::Int((xml.encode_utf16().count() * 2) as i64)),
            ("query_plan", Val::Text(xml.into())),
        ],
    )
}

pub(crate) fn joined_parameter_row(count: usize) -> Rec {
    let mut sql = "SELECT p0.id FROM @Rows p0".to_owned();
    for at in 1..count {
        sql.push_str(&format!(" CROSS JOIN @Rows p{at}"));
    }
    let mut operators = (0..count)
        .map(|at| {
            let id = u32::MAX - u32::try_from(at).unwrap();
            format!(
                r#"<RelOp NodeId="{id}" PhysicalOp="Table Scan" LogicalOp="Table Scan" EstimateRows="1" EstimatedTotalSubtreeCost="1"><TableScan><Object Table="[@Rows]" Alias="[p{at}]" /></TableScan></RelOp>"#
            )
        })
        .collect::<Vec<_>>();
    let mut join_id = 0;
    while operators.len() > 1 {
        operators = operators
            .chunks(2)
            .map(|children| {
                if children.len() == 1 {
                    return children[0].clone();
                }
                let id = join_id;
                join_id += 1;
                format!(
                    r#"<RelOp NodeId="{id}" PhysicalOp="Nested Loops" LogicalOp="Inner Join" EstimateRows="1" EstimatedTotalSubtreeCost="1"><NestedLoops>{}{}</NestedLoops></RelOp>"#,
                    children[0], children[1]
                )
            })
            .collect();
    }
    let xml = format!(
        r#"<ShowPlanXML><BatchSequence><Batch><Statements><StmtSimple StatementText="{sql}" QueryHash="0x0123456789ABCDEF" StatementSubTreeCost="1"><QueryPlan>{}</QueryPlan></StmtSimple></Statements></Batch></BatchSequence></ShowPlanXML>"#,
        operators.pop().unwrap()
    );
    module_row(&sql, &xml)
}

#[test]
fn the_native_binding_limit_retains_payloads_when_optional_classification_exceeds_its_envelope() {
    let source = crate::plan_xml::test_source(SnapshotDataset::CachedDefinitions);
    let limits = Limits::from_budget(&CollectionBudget::default()).unwrap();
    for count in [MAX_MSSQL_RELATIONAL_INPUT_BINDINGS, MAX_MSSQL_RELATIONAL_INPUT_BINDINGS + 1] {
        let row = joined_parameter_row(count);
        let mut definition = decode(&row, &limits, Some(&source)).unwrap();
        assert!(definition.statement.payload.is_available());
        assert!(definition.estimated_plan.payload.is_available());
        assert!(definition.module_parameters.is_some());
        if count == MAX_MSSQL_RELATIONAL_INPUT_BINDINGS {
            let proof = definition.relational_input_proof.as_ref().expect("the largest supported native roster");
            assert_eq!(proof.bindings.len(), count);
            assert_eq!(serde_json::to_vec(proof).unwrap().len(), 6_962);
            assert!(proof.matches_cached(&source, &definition));
        } else {
            assert!(definition.relational_input_proof.is_none(), "unavailable proof must not discard native bodies");
        }
        definition.relational_input_proof = None;
        assert_eq!(definition, decode(&row, &limits, None).unwrap());
        assert!(decode(&replace(&row, &[("plan_bytes", Val::Int(2))]), &limits, Some(&source)).is_err());
    }
}

#[test]
fn cache_definitions_retain_native_identity_and_literal_changes_without_exporting_raw_literals() {
    let limits = Limits::from_budget(&CollectionBudget::default()).unwrap();
    let definition = decode(&row(), &limits, None).unwrap();
    let encoded = serde_json::to_string(&definition).unwrap();
    assert!(!encoded.contains("private-value"));
    assert!(definition.statement.native_sha256.is_some());
    assert!(definition.estimated_plan.payload.is_available());
    let first = ObjectDefinitionRecord::new(
        crate::sources::PLAN_CACHE.name.into(),
        ObjectDefinitionFact::MssqlCachedDefinition(Box::new(definition)),
    )
    .unwrap();
    let sql = "SELECT 'another-value' AS canary";
    let changed = replace(
        &row(),
        &[("statement_bytes", Val::Int(sql.len() as i64 * 2)), ("statement_text", Val::Text(sql.into()))],
    );
    let second = ObjectDefinitionRecord::new(
        first.source.clone(),
        ObjectDefinitionFact::MssqlCachedDefinition(Box::new(decode(&changed, &limits, None).unwrap())),
    )
    .unwrap();
    assert_eq!(first.key, second.key);
    assert_ne!(first.version, second.version);
}
#[test]
fn a_single_record_fits_the_page_policy_and_unavailable_bodies_are_not_truncated() {
    let budget = CollectionBudget::default();
    let limits = Limits::from_budget(&budget).unwrap();
    assert!(
        (limits.sql + limits.plan) * JSON_BYTES_PER_UTF16_BYTE + RECORD_METADATA_BYTES <= budget.page_bytes as usize
    );
    let oversized = decode(
        &replace(&row(), &[("statement_bytes", Val::Int((limits.sql + 1) as i64)), ("statement_text", Val::Null)]),
        &limits,
        None,
    )
    .unwrap();
    assert!(matches!(
        oversized.statement.payload,
        DefinitionPayload::Unavailable { reason: PayloadUnavailable::Oversized }
    ));
    assert!(decode(&replace(&row(), &[("statement_bytes", Val::Int(2))]), &limits, None).is_err());
    assert!(Limits::from_budget(&CollectionBudget { page_bytes: 1024, ..budget }).is_err());
    assert!(select_sql(&limits).contains(cached_statements::DATABASE_PREDICATE));
}
