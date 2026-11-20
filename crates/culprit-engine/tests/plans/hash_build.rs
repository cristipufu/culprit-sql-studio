use super::*;
use culprit_core::{
    insights::insights,
    plan_facts::{RelationalInput, RelationalInputKind, RelationalInputProvenance},
    t,
};

fn scan(id: u32, tvp: bool) -> String {
    let object = if tvp {
        r#"Table="[@Rows]" Alias="[r]""#
    } else {
        r#"Database="[app]" Schema="[dbo]" Table="[Items]" Alias="[i]""#
    };
    format!(
        r#"<RelOp NodeId="{id}" PhysicalOp="Table Scan" LogicalOp="Table Scan" Parallel="0" EstimateRows="100" EstimatedRowsRead="100" EstimatedTotalSubtreeCost="1"><RunTimeInformation><RunTimeCountersPerThread Thread="0" ActualRows="10000" ActualRowsRead="20000" ActualExecutions="1" ActualEndOfScans="1" /></RunTimeInformation><TableScan><Object {object} /></TableScan></RelOp>"#
    )
}

fn supplied_plan(tvp_build: bool) -> Plan {
    let build = scan(if tvp_build { 8 } else { 3 }, tvp_build);
    let probe = scan(2, !tvp_build);
    let xml = format!(
        r#"<ShowPlanXML><BatchSequence><Batch><Statements><StmtSimple StatementText="SELECT COUNT_BIG(*) FROM @Rows r JOIN dbo.Items i ON r.id=i.id" StatementSubTreeCost="3"><QueryPlan><QueryTimeStats ElapsedTime="400" CpuTime="100" />
<RelOp NodeId="0" PhysicalOp="Stream Aggregate" LogicalOp="Aggregate" Parallel="0" EstimateRows="1" EstimatedTotalSubtreeCost="3"><RunTimeInformation><RunTimeCountersPerThread Thread="0" ActualRows="1" ActualExecutions="1" /></RunTimeInformation><StreamAggregate>
<RelOp NodeId="1" PhysicalOp="Hash Match" LogicalOp="Inner Join" Parallel="0" EstimateRows="100" EstimatedTotalSubtreeCost="2"><RunTimeInformation><RunTimeCountersPerThread Thread="0" ActualRows="100" ActualExecutions="1" /></RunTimeInformation><Hash><HashKeysBuild><ColumnReference Column="id" /></HashKeysBuild><HashKeysProbe><ColumnReference Column="id" /></HashKeysProbe>{build}{probe}</Hash></RelOp>
</StreamAggregate></RelOp></QueryPlan></StmtSimple></Statements></Batch></BatchSequence></ShowPlanXML>"#
    );
    let mut plan = parse_plan(&xml).unwrap();
    // This consumer fixture supplies already-qualified exported input metadata. Native module/namespace binding
    // is independently exercised by the provider replay tests; JSON import must transport, never infer it.
    plan.root.children[0].children[usize::from(!tvp_build)].facts_mut().relational_input = Some(RelationalInput {
        kind: RelationalInputKind::TableValuedParameter,
        provenance: RelationalInputProvenance::NativeParameterMetadata,
    });
    plan
}

fn complete_input(plan: &Plan) -> bool {
    let counts = t!("insight.relational_input.completed_input_counts", estimated = "100", actual = "20K");
    insights(plan).iter().any(|insight| insight.detail.contains(&counts))
}

fn import(plan: &Plan) -> Plan {
    let json = serde_json::to_string(plan).unwrap();
    assert_eq!(detect(&json), Some(PlanFormat::Culprit));
    parse_plan(&json).unwrap()
}

#[test]
fn public_json_import_remaps_the_original_build_child_and_preserves_complete_input_evidence() {
    let supplied = supplied_plan(true);
    assert_eq!(supplied.root.children[0].facts().hash_build_input, Some(8));
    assert!(complete_input(&supplied));
    let imported = import(&supplied);
    let hash = &imported.root.children[0];
    assert_eq!(hash.facts().hash_build_input, Some(2));
    assert_eq!(hash.children[0].id, 2);
    assert_eq!(hash.children[0].object.as_deref(), Some("@Rows"));
    assert!(complete_input(&imported));
    assert_eq!(
        supplied.root.children[0].facts().hash_build_input,
        Some(8),
        "the original supplied tree stays unchanged"
    );
}

#[test]
fn public_json_import_cannot_transfer_a_native_build_role_to_the_original_tvp_probe() {
    let supplied = supplied_plan(false);
    let hash = &supplied.root.children[0];
    assert_eq!(hash.facts().hash_build_input, Some(3));
    assert_eq!(hash.children[1].id, 2);
    assert!(!complete_input(&supplied));
    let imported = import(&supplied);
    let hash = &imported.root.children[0];
    assert_eq!(hash.children[0].id, 2);
    assert_eq!(hash.children[1].id, 3);
    assert_eq!(hash.facts().hash_build_input, Some(2));
    assert_eq!(hash.children[1].object.as_deref(), Some("@Rows"));
    assert!(!complete_input(&imported));
}

#[test]
fn public_json_import_withdraws_ambiguous_missing_or_detached_ownership_instead_of_reconstructing_it() {
    for case in ["duplicate-child", "duplicate-ancestor", "missing-role", "detached-role", "parent-role"] {
        let mut supplied = supplied_plan(true);
        match case {
            "duplicate-child" => supplied.root.children[0].children[1].id = 8,
            "duplicate-ancestor" => supplied.root.id = 8,
            "missing-role" => supplied.root.children[0].facts_mut().hash_build_input = None,
            "detached-role" => supplied.root.children[0].facts_mut().hash_build_input = Some(99),
            "parent-role" => supplied.root.children[0].facts_mut().hash_build_input = Some(0),
            _ => unreachable!(),
        }
        let imported = import(&supplied);
        assert_eq!(imported.root.children[0].facts().hash_build_input, None, "{case}");
        assert!(!complete_input(&imported), "{case}");
        let ids = imported.nodes().into_iter().map(|node| node.id).collect::<Vec<_>>();
        assert_eq!(ids, vec![0, 1, 2, 3], "UI numbering still has one unique bounded tree");
    }
}
