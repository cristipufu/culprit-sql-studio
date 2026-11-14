use super::*;
use crate::PlanNodeFacts;

#[test]
fn native_kind_requires_the_evidence_that_can_establish_it() {
    use RelationalInputKind::*;
    use RelationalInputProvenance::*;
    for (kind, provenance) in [
        (TableValuedParameter, NativeParameterMetadata),
        (LocalTableVariable, NativeDeclaration),
        (TemporaryTable, NativePlan),
        (TableValuedFunction, NativePlan),
        (Unknown, Unresolved),
    ] {
        let input = RelationalInput { kind, provenance };
        assert!(input.is_valid());
        let facts = PlanNodeFacts { relational_input: Some(input), ..Default::default() };
        let reread: PlanNodeFacts = serde_json::from_value(serde_json::to_value(&facts).unwrap()).unwrap();
        assert_eq!(reread.relational_input, Some(input));
        assert_eq!(input.qualified_kind(), kind);
    }
}

#[test]
fn a_name_or_generic_native_plan_cannot_authorize_tvp_or_local_advice() {
    use RelationalInputKind::*;
    use RelationalInputProvenance::*;
    for input in [
        RelationalInput { kind: TableValuedParameter, provenance: Unresolved },
        RelationalInput { kind: TableValuedParameter, provenance: NativePlan },
        RelationalInput { kind: LocalTableVariable, provenance: NativePlan },
        RelationalInput { kind: LocalTableVariable, provenance: NativeParameterMetadata },
        RelationalInput { kind: TemporaryTable, provenance: NativeDeclaration },
    ] {
        assert!(!input.is_valid());
        assert_eq!(input.qualified_kind(), Unknown);
        let facts = PlanNodeFacts { relational_input: Some(input), ..Default::default() };
        let reread: PlanNodeFacts = serde_json::from_value(serde_json::to_value(&facts).unwrap()).unwrap();
        assert_eq!(reread.relational_input, None, "unqualified imported input must be unknown");
    }
}
