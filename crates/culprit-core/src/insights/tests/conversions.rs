use super::*;

const CONVERSION: &str = "CONVERT_IMPLICIT(nvarchar(64),[a].[name],0)=[@p]";
const OTHER_CONVERSION: &str = "CONVERT_IMPLICIT(int,[b].[id],0)=[@q]";

fn warning(issue: crate::PlanConversionIssue, expression: &str) -> crate::PlanConversion {
    crate::PlanConversion { issue, expression: expression.into() }
}

fn converted_plan() -> Plan {
    let mut scan = measured(1, "Index Scan", NodeKind::Scan, 20_000.0, 20_000.0, 1.0);
    scan.object = Some("dbo.Items".into());
    scan.facts_mut().table_alias = Some("a".into());
    scan.predicate = Some(CONVERSION.into());
    scan.self_cost = 5.0;
    let mut seek = measured(2, "Index Seek", NodeKind::Seek, 1.0, 1.0, 1.0);
    seek.object = Some("dbo.Other".into());
    seek.facts_mut().table_alias = Some("b".into());
    seek.seek_predicate = Some(OTHER_CONVERSION.into());
    seek.self_cost = 100.0;
    let mut join = measured(0, "Hash Match", NodeKind::Join, 20_000.0, 20_000.0, 1.0);
    join.children = vec![scan, seek];
    actual(Engine::Mssql, join, 400.0)
}

fn concern(plan: &Plan) -> PlanInsight {
    insights(plan).into_iter().find(|insight| insight.rule == "implicit-conversion").unwrap()
}

#[test]
fn node_only_native_conversion_warnings_keep_their_original_operator_and_verdict() {
    for issue in [crate::PlanConversionIssue::SeekPlan, crate::PlanConversionIssue::CardinalityEstimate] {
        let mut plan = converted_plan();
        plan.root.children[0].predicate = None;
        plan.root.children[1].seek_predicate = None;
        plan.root.children[0].facts_mut().conversions = vec![warning(issue.clone(), CONVERSION)];
        let evidence = conversion_evidence(&plan).unwrap();
        assert_eq!(evidence.node().unwrap().id, 1);
        assert_eq!(evidence.expression(), CONVERSION);
        assert_eq!(evidence.column(), "a.name");
        assert_eq!(evidence.target(), "nvarchar(64)");
        assert!(evidence.has_native_warning());
        assert_eq!(evidence.blocks_seek(), issue == crate::PlanConversionIssue::SeekPlan);
        assert_eq!(concern(&plan).nodes, vec![1]);
    }
}

#[test]
fn an_unrelated_costlier_converted_seek_cannot_downgrade_another_native_seek_warning() {
    let mut plan = converted_plan();
    plan.facts_mut().conversions = vec![warning(crate::PlanConversionIssue::SeekPlan, CONVERSION)];
    plan.root.children[1].facts_mut().conversions =
        vec![warning(crate::PlanConversionIssue::CardinalityEstimate, OTHER_CONVERSION)];
    let evidence = conversion_evidence(&plan).unwrap();
    assert!(evidence.blocks_seek());
    assert_eq!(evidence.node().unwrap().id, 1);
    assert_eq!(evidence.expression(), CONVERSION);
    let insight = concern(&plan);
    assert_eq!(insight.nodes, vec![1]);
    assert_eq!(insight.title, t!("insight.implicit_conversion.title.blocks_seek", named = "true", column = "a.name"));
}

#[test]
fn a_seek_that_owns_the_same_conversion_retains_seek_plan_without_an_estimate_or_harm_claim() {
    let mut plan = converted_plan();
    plan.facts_mut().conversions = vec![warning(crate::PlanConversionIssue::SeekPlan, CONVERSION)];
    plan.root.children[0].kind = NodeKind::Seek;
    plan.root.children[0].op = "Index Seek".into();
    let evidence = conversion_evidence(&plan).unwrap();
    assert!(!evidence.blocks_seek());
    assert_eq!(evidence.node().unwrap().id, 1);
    assert_eq!(evidence.effect(), ConversionEffect::SeekObserved);
    assert_eq!(concern(&plan).title, t!("insight.implicit_conversion.title.seek_observed", column = "a.name"));
    assert!(concern(&plan).detail.contains(&t!("insight.implicit_conversion.consequence.seek_observed")));
}

#[test]
fn a_statement_conversion_with_ambiguous_native_owners_keeps_the_warning_without_a_link() {
    let mut plan = converted_plan();
    plan.facts_mut().conversions = vec![warning(crate::PlanConversionIssue::SeekPlan, CONVERSION)];
    let mut duplicate = plan.root.children[0].clone();
    duplicate.id = 2;
    duplicate.kind = NodeKind::Seek;
    plan.root.children[1] = duplicate;
    let evidence = conversion_evidence(&plan).unwrap();
    assert!(evidence.blocks_seek());
    assert!(evidence.node().is_none());
    assert!(concern(&plan).nodes.is_empty());
}

#[test]
fn duplicate_native_node_ids_cannot_supply_an_operator_even_for_a_node_scoped_warning() {
    let mut plan = converted_plan();
    plan.root.children[0].facts_mut().conversions = vec![warning(crate::PlanConversionIssue::SeekPlan, CONVERSION)];
    plan.root.children[1].id = 1;
    let evidence = conversion_evidence(&plan).unwrap();
    assert!(evidence.blocks_seek());
    assert!(evidence.node().is_none());
    assert!(concern(&plan).nodes.is_empty());
}

#[test]
fn native_as_equivalence_binds_only_the_complete_canonical_reference() {
    for (suffix, qualified) in
        [("[a].[name]", true), ("[a].[name] + 1", false), ("[a].[name] COLLATE Latin1_General_100_CI_AS", false)]
    {
        let mut plan = converted_plan();
        plan.facts_mut().conversions = vec![warning(crate::PlanConversionIssue::SeekPlan, CONVERSION)];
        plan.root.children[0].predicate =
            Some(format!("CONVERT_IMPLICIT(nvarchar(64),[dbo].[Items].[name] AS {suffix},0)=[@p]"));
        let evidence = conversion_evidence(&plan).unwrap();
        assert_eq!(evidence.node().map(|node| node.id), qualified.then_some(1), "{suffix}");
        assert!(evidence.blocks_seek());
    }
}

#[test]
fn scalar_only_conversions_cannot_supply_a_column_warning_or_operator() {
    let mut plan = converted_plan();
    let scalar = "CONVERT_IMPLICIT(nvarchar(64),[@p],0)";
    plan.facts_mut().conversions = vec![warning(crate::PlanConversionIssue::SeekPlan, scalar)];
    plan.root.children[0].facts_mut().conversions =
        vec![warning(crate::PlanConversionIssue::CardinalityEstimate, scalar)];
    plan.root.children[0].predicate = Some(scalar.into());
    plan.root.children[1].seek_predicate = None;
    assert!(conversion_evidence(&plan).is_none());
    assert!(!insights(&plan).iter().any(|insight| insight.rule == "implicit-conversion"));
}

#[test]
fn a_plain_condition_retains_its_existing_per_row_words_without_an_engine_seek_verdict() {
    let plan = converted_plan();
    let evidence = conversion_evidence(&plan).unwrap();
    assert_eq!(evidence.node().unwrap().id, 2);
    assert!(!evidence.has_native_warning());
    assert!(!evidence.blocks_seek());
    assert_eq!(concern(&plan).title, t!("insight.implicit_conversion.title.per_row", named = "true", column = "b.id"));
}

#[test]
fn an_unrecognized_native_issue_does_not_become_a_cardinality_estimate_verdict() {
    let mut plan = converted_plan();
    plan.facts_mut().conversions = vec![warning(crate::PlanConversionIssue::Other("Future Issue".into()), CONVERSION)];
    let evidence = conversion_evidence(&plan).unwrap();
    assert_eq!(evidence.effect(), ConversionEffect::OtherWarning);
    let insight = concern(&plan);
    assert_eq!(insight.title, t!("insight.implicit_conversion.title.other_warning", column = "a.name"));
    assert!(insight.detail.contains(&t!("insight.implicit_conversion.consequence.other_warning")));
}

#[test]
fn a_complete_but_foreign_native_as_reference_cannot_bind_an_unrelated_seek() {
    let mut plan = converted_plan();
    plan.facts_mut().conversions = vec![warning(crate::PlanConversionIssue::SeekPlan, CONVERSION)];
    plan.root.children[0].predicate = None;
    plan.root.children[1].seek_predicate =
        Some("CONVERT_IMPLICIT(nvarchar(64),[dbo].[Other].[name] AS [a].[name],0)=[@p]".into());
    let evidence = conversion_evidence(&plan).unwrap();
    assert!(evidence.blocks_seek());
    assert!(evidence.node().is_none());
    assert!(concern(&plan).nodes.is_empty());
}

#[test]
fn a_legacy_condition_with_duplicate_ids_keeps_its_conversion_without_an_operator_link() {
    let mut plan = converted_plan();
    plan.root.children[1].id = 1;
    let evidence = conversion_evidence(&plan).unwrap();
    assert_eq!(evidence.effect(), ConversionEffect::PerRow);
    assert!(evidence.node().is_none());
    assert!(concern(&plan).nodes.is_empty());
}

#[test]
fn a_scalar_native_warning_does_not_lend_its_issue_to_an_unrelated_per_row_conversion() {
    let mut plan = converted_plan();
    let scalar = "CONVERT_IMPLICIT(nvarchar(64),[@p],0)";
    plan.facts_mut().conversions = vec![warning(crate::PlanConversionIssue::SeekPlan, scalar)];
    plan.warnings.push(PlanWarning::ImplicitConversion);
    plan.root.children[0].predicate = Some(scalar.into());
    let evidence = conversion_evidence(&plan).unwrap();
    assert_eq!(evidence.effect(), ConversionEffect::PerRow);
    assert_eq!(evidence.node().unwrap().id, 2);
    assert_eq!(evidence.expression(), OTHER_CONVERSION);
    assert!(!evidence.has_native_warning());
}
