use super::*;
use crate::plan_facts::{
    PlanParameter, PlanParameterComparison as Comparison, PlanParameterContext, PlanParameterValueState as State,
};

fn with_parameter(name: &str, data_type: &str) -> crate::Plan {
    let snapshot: Snapshot = serde_json::from_str(include_str!("../../../../../fixtures/mssql-demo.json")).unwrap();
    let mut plan = snapshot.queries().iter().find_map(|query| query.plan.clone()).unwrap();
    plan.facts_mut().parameter_context = Some(PlanParameterContext {
        parameters: vec![PlanParameter {
            name: Some(name.into()),
            data_type: Some(data_type.into()),
            compiled: State::Recorded,
            runtime: State::Recorded,
            comparison: Comparison::DifferentRepresentation,
        }],
        uses: vec![],
        issues: vec![],
    });
    plan
}

const TYPES: &[(&str, &str)] = &[
    ("private_database_canary.dbo.CustomerType", "[dbo].[CustomerType]"),
    ("[private_database_canary].dbo.[CustomerType]", "[dbo].[CustomerType]"),
    ("\"private_database_canary\".\"dbo\".\"CustomerType\"", "[dbo].[CustomerType]"),
    ("private_database_canary . [dbo] . \"CustomerType\"", "[dbo].[CustomerType]"),
    ("[private_database_canary].[d.bo].[Customer.Type]", "[d.bo].[Customer.Type]"),
    ("nvarchar(4000)", "nvarchar(4000)"),
    ("decimal(12,2)", "decimal(12,2)"),
    ("varbinary(max)", "varbinary(max)"),
];

#[test]
fn typed_parameter_types_remove_every_supported_database_qualifier_without_losing_native_sizes() {
    for (data_type, expected) in TYPES {
        let original = with_parameter("@customer", data_type);
        let shared = redact_plan(&original);
        let context = shared.facts().parameter_context.as_ref().unwrap();
        assert!(context.is_valid());
        assert_eq!(context.parameters[0].data_type.as_deref(), Some(*expected), "{data_type}");
        assert_eq!(context.parameters[0].comparison, Comparison::DifferentRepresentation);
        assert!(!serde_json::to_string(&shared).unwrap().contains("private_database_canary"));
        assert_eq!(serde_json::to_value(&shared).unwrap(), serde_json::to_value(redact_plan(&shared)).unwrap());
        assert_eq!(
            original.facts().parameter_context.as_ref().unwrap().parameters[0].data_type.as_deref(),
            Some(*data_type)
        );
    }
}

#[test]
fn native_xml_and_typed_parameter_context_share_one_qualified_type_privacy_policy() {
    for (data_type, expected) in TYPES {
        let xml = format!(
            r#"<ColumnReference Column="@customer" ParameterDataType='{data_type}' ParameterCompiledValue="(4)" ParameterRuntimeValue="(5)"/>"#
        );
        let shared = redact_native(&xml, Engine::Mssql);
        assert!(!shared.contains("private_database_canary"), "{shared}");
        let retained = crate::xml::read(&shared, |document| {
            document.unwrap().root_element().attribute("ParameterDataType").unwrap().to_owned()
        });
        assert_eq!(retained, *expected);
        assert_eq!(redact_native(&shared, Engine::Mssql), shared);
    }
}

#[test]
fn unresolved_private_type_syntax_is_explicitly_withheld_in_both_formats() {
    let mut types = [
        "private_database_canary..CustomerType",
        "[private_database_canary.dbo.CustomerType",
        "private_database_canary.dbo.CustomerType COLLATE other",
        "nvarchar(private_database_canary)",
    ]
    .map(str::to_owned)
    .to_vec();
    types.push(format!("private_database_canary.\"{}\".\"{}\"", "]".repeat(128), "]".repeat(128)));
    for data_type in types {
        let shared = redact_plan(&with_parameter("@customer", &data_type));
        let context = shared.facts().parameter_context.as_ref().unwrap();
        assert!(context.is_valid());
        assert_eq!(context.parameters[0].data_type, None);
        let xml = format!(r#"<ColumnReference ParameterDataType='{data_type}'/>"#);
        let shared_xml = redact_native(&xml, Engine::Mssql);
        assert!(!shared_xml.contains("private_database_canary"));
        let retained = crate::xml::read(&shared_xml, |document| {
            document.unwrap().root_element().attribute("ParameterDataType").unwrap().to_owned()
        });
        assert!(retained.is_empty());
        assert_eq!(redact_native(&shared_xml, Engine::Mssql), shared_xml);
    }
}

#[test]
fn filtering_an_unchanged_invalid_name_also_withdraws_its_comparison_authority() {
    for name in [Some("@invalid name".to_string()), None] {
        let mut original = with_parameter("@customer", "int");
        original.facts_mut().parameter_context.as_mut().unwrap().parameters[0].name = name;
        let shared = redact_plan(&original);
        let context = shared.facts().parameter_context.as_ref().unwrap();
        assert_eq!(context.parameters[0].name, None);
        assert_eq!(context.parameters[0].comparison, Comparison::Unavailable);
        assert!(context.is_valid());
        assert_eq!(serde_json::to_value(&shared).unwrap(), serde_json::to_value(redact_plan(&shared)).unwrap());
    }
}
