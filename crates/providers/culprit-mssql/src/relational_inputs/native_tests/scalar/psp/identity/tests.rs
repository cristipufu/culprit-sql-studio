use super::*;
use crate::{cached_definitions, resources::test_support::replace, value::Val};

fn xml(text: &str, plan_hash: &str, variant: &str) -> String {
    format!(
        r#"<ShowPlanXML><BatchSequence><Batch><Statements><StmtSimple StatementText="{text}" QueryHash="0x0123456789ABCDEF" QueryPlanHash="{plan_hash}"><QueryPlan {variant}><RelOp NodeId="0" PhysicalOp="Index Seek"><RunTimeInformation><RunTimeCountersPerThread Thread="0" ActualRows="1" ActualExecutions="1"/></RunTimeInformation></RelOp></QueryPlan></StmtSimple></Statements></Batch></BatchSequence></ShowPlanXML>"#
    )
}
fn point(text: &str, hash: &str) -> NamedDefinition {
    let row = replace(
        &cached_definitions::tests::module_row(text, &xml(text, hash, "QueryVariantID=\"1\"")),
        &[("query_plan_hash", Val::Text(hash.into()))],
    );
    let limits = cached_definitions::Limits::from_budget(&CollectionBudget::default()).unwrap();
    NamedDefinition { fact: cached_definitions::decode(&row, &limits, None).unwrap(), original_statement: text.into() }
}
#[test]
fn variants_with_the_same_query_hash_require_the_exact_native_statement_and_plan() {
    let first = "SELECT id AS native_payload_length FROM dbo.t WHERE bucket=@bucket option (PLAN PER VALUE(ObjectID=12,QueryVariantID=1))";
    let second = "SELECT id AS native_payload_length FROM dbo.t WHERE bucket=@bucket option (PLAN PER VALUE(ObjectID=12,QueryVariantID=2))";
    let points = [point(first, "0x0000000000000001"), point(second, "0x0000000000000002")];
    let xml = [xml(second, "0x0000000000000002", "QueryVariantID=\"2\"")];
    let selected = bind(&xml, &points, None, RoutingExpectation::Variant);
    assert_eq!(selected.definition, 1);
    assert_eq!(selected.variant, Some(2));
    assert!(std::panic::catch_unwind(|| bind(&xml, &points[..1], None, RoutingExpectation::Variant)).is_err());
}
#[test]
fn a_native_recycled_generation_cannot_relabel_the_original_actual_plan() {
    let text = "SELECT id AS native_payload_length FROM dbo.t WHERE bucket=@bucket";
    let point = point(text, "0x0000000000000001");
    let mut original = point.fact.context.clone();
    original.identity.plan_generation = (original.identity.plan_generation.parse::<u64>().unwrap() + 1).to_string();
    let xml = [xml(text, "0x0000000000000001", "QueryVariantID=\"1\"")];
    assert!(std::panic::catch_unwind(|| bind(&xml, &[point], Some(&original), RoutingExpectation::Variant)).is_err());
}
#[test]
fn missing_invalid_and_ambiguous_native_variant_evidence_is_not_a_positive() {
    let text = "SELECT id AS native_payload_length FROM dbo.t WHERE bucket=@bucket";
    let point = point(text, "0x0000000000000001");
    for variant in ["", "QueryVariantID=\"0\"", "QueryVariantID=\"-1\"", "QueryVariantID=\"bad\""] {
        let xml = [xml(text, "0x0000000000000001", variant)];
        assert!(
            std::panic::catch_unwind(|| bind(&xml, std::slice::from_ref(&point), None, RoutingExpectation::Variant))
                .is_err()
        );
    }
    let actual = xml(text, "0x0000000000000001", "QueryVariantID=\"1\"");
    assert!(
        std::panic::catch_unwind(|| bind(
            &[actual.clone(), actual],
            std::slice::from_ref(&point),
            None,
            RoutingExpectation::Variant
        ))
        .is_err()
    );
    let estimated = xml(text, "0x0000000000000001", "QueryVariantID=\"1\"")
        .replace("RunTimeCountersPerThread", "AbsentNativeRuntime");
    assert!(
        std::panic::catch_unwind(|| bind(
            &[estimated],
            std::slice::from_ref(&point),
            None,
            RoutingExpectation::Variant
        ))
        .is_err()
    );
}

#[test]
fn another_database_sql_handle_or_plan_cannot_supply_the_original_statement_binding() {
    let text = "SELECT id AS native_payload_length FROM dbo.t WHERE bucket=@bucket";
    let point = point(text, "0x0000000000000001");
    let mut foreign_database = point.fact.context.clone();
    foreign_database.database_id += 1;
    let mut recycled_text = point.fact.context.clone();
    recycled_text.identity.sql_handle = "0x12345678".into();
    let actual = [xml(text, "0x0000000000000001", "QueryVariantID=\"1\"")];
    for original in [foreign_database, recycled_text] {
        assert!(
            std::panic::catch_unwind(|| bind(
                &actual,
                std::slice::from_ref(&point),
                Some(&original),
                RoutingExpectation::Variant
            ))
            .is_err()
        );
    }
    let changed_plan = [xml(text, "0x0000000000000009", "QueryVariantID=\"1\"")];
    assert!(
        std::panic::catch_unwind(|| bind(
            &changed_plan,
            std::slice::from_ref(&point),
            None,
            RoutingExpectation::Variant
        ))
        .is_err()
    );
    let changed_statement = [xml(
        "SELECT id AS native_payload_length FROM dbo.another_t WHERE bucket=@bucket",
        "0x0000000000000001",
        "QueryVariantID=\"1\"",
    )];
    assert!(
        std::panic::catch_unwind(|| bind(
            &changed_statement,
            std::slice::from_ref(&point),
            None,
            RoutingExpectation::Variant
        ))
        .is_err()
    );
}
