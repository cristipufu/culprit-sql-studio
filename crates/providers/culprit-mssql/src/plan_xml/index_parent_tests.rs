use super::*;
use culprit_core::telemetry::MssqlIndexParentKind;
fn native(objects: &str) -> Plan {
    parse(&format!(r#"<ShowPlanXML><BatchSequence><Batch><Statements><StmtSimple StatementText="SELECT id FROM reporting.Revenue" StatementSubTreeCost="10"><QueryPlan><RelOp NodeId="0" PhysicalOp="Index Scan" LogicalOp="Index Scan" EstimateRows="10" EstimatedTotalSubtreeCost="10"><IndexScan>{objects}</IndexScan></RelOp></QueryPlan></StmtSimple></Statements></Batch></BatchSequence></ShowPlanXML>"#)).unwrap()
}
#[test]
fn only_one_direct_original_view_index_kind_establishes_an_indexed_view() {
    for kind in ["ViewClustered", "ViewNonClustered"] {
        let plan = native(&format!(
            r#"<Object Schema="[reporting]" Table="[Revenue]" Index="[ix_revenue]" IndexKind="{kind}"/>"#
        ));
        assert_eq!(plan.root.facts().mssql_index_parent_kind, Some(MssqlIndexParentKind::IndexedView));
        let restored: Plan = serde_json::from_slice(&serde_json::to_vec(&plan).unwrap()).unwrap();
        assert_eq!(restored.root.facts().mssql_index_parent_kind, plan.root.facts().mssql_index_parent_kind);
    }
    for objects in [
        r#"<Object Schema="[reporting]" Table="[Revenue]" Index="[ix_revenue]"/>"#,
        r#"<Object Schema="[reporting]" Table="[Revenue]" Index="[ix_revenue]" IndexKind="Clustered"/>"#,
        r#"<Object Schema="[reporting]" Table="[Revenue]" Index="[ix_revenue]" IndexKind="Future"/>"#,
        r#"<Object Table="[Revenue]" IndexKind="ViewClustered"/>"#,
        r#"<Object Index="[ix_revenue]" IndexKind="ViewClustered"/>"#,
        r#"<Object Table="[Revenue]" Index="[fixture_clustered]" IndexKind="ViewClustered"/><Object Table="[Revenue]" Index="[fixture_clustered]" IndexKind="ViewClustered"/>"#,
        r#"<Predicate><Object Table="[Revenue]" Index="[fixture_clustered]" IndexKind="ViewClustered"/></Predicate>"#,
    ] {
        assert_eq!(native(objects).root.facts().mssql_index_parent_kind, None, "{objects}");
    }
}
