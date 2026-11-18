use super::*;
use crate::telemetry::DefinitionPayload;

fn attribute(xml: &str, element: &str, name: &str) -> String {
    crate::xml::read(xml, |document| {
        document
            .unwrap()
            .descendants()
            .find(|node| node.is_element() && node.tag_name().name() == element)
            .unwrap()
            .attribute(name)
            .unwrap()
            .to_owned()
    })
}
fn available(payload: &DefinitionPayload) -> &str {
    let DefinitionPayload::Available { text, .. } = payload else {
        panic!("the canonical redaction must remain valid XML")
    };
    text
}
#[test]
fn showplan_attribute_whitespace_references_preserve_meaning_privacy_and_one_pass_canonical_bytes() {
    let raw = r#"<?xml version="1.0"?><ShowPlanXML xmlns="http://schemas.microsoft.com/sqlserver/2004/07/showplan"><BatchSequence><Batch><Statements><StmtSimple StatementText="SELECT&#xD;&#xA;&#x9;[keep&#x9;column], &quot;keep&#xA;&amp;column&quot; FROM dbo.rows WHERE secret=N&apos;PRIVATE_STATEMENT&apos; AND id=987654321 option (PLAN PER VALUE(ObjectID=12345, QueryVariantID=2))" QueryHash="0x0123456789ABCDEF" QueryPlanHash="0x0000000000000001"><QueryPlan QueryVariantID="2"><RelOp NodeId="7" EstimateRows="3.5"><RunTimeInformation><RunTimeCountersPerThread Thread="0" ActualRows="9007199254740993" ActualExecutions="1"/></RunTimeInformation><ScalarOperator ScalarString="[PRIVATE_DATABASE].[dbo].[rows].[id]&#x9;=&#xA;&#xD;(987654321)"/><Object Database="[PRIVATE_DATABASE]" Schema="[dbo]" Table="[rows]"/></RelOp><ParameterList><ColumnReference Column="@p" ParameterDataType="int" ParameterCompiledValue="(987654321)" ParameterRuntimeValue="(987654322)"/></ParameterList></QueryPlan></StmtSimple></Statements></Batch></BatchSequence></ShowPlanXML>"#;
    let once = DefinitionPayload::plan(raw, Engine::Mssql);
    assert!(once.is_valid());
    let xml = available(&once);
    assert_eq!(
        attribute(xml, "StmtSimple", "StatementText"),
        "SELECT\r\n\t[keep\tcolumn], \"keep\n&column\" FROM dbo.rows WHERE secret=N'?' AND id=? option (PLAN PER VALUE(ObjectID=?, QueryVariantID=?))"
    );
    assert_eq!(attribute(xml, "ScalarOperator", "ScalarString"), "[dbo].[rows].[id]\t=\n\r(?)");
    assert_eq!(attribute(xml, "QueryPlan", "QueryVariantID"), "2");
    assert_eq!(attribute(xml, "RelOp", "NodeId"), "7");
    assert_eq!(attribute(xml, "RelOp", "EstimateRows"), "3.5");
    assert_eq!(attribute(xml, "RunTimeCountersPerThread", "ActualRows"), "9007199254740993");
    assert_eq!(attribute(xml, "StmtSimple", "QueryHash"), "0x0123456789ABCDEF");
    for private in ["PRIVATE_STATEMENT", "PRIVATE_DATABASE", "987654321", "987654322"] {
        assert!(!xml.contains(private));
    }
    assert!(xml.contains("&#xD;&#xA;&#x9;"));
    let twice = DefinitionPayload::plan(xml, Engine::Mssql);
    assert_eq!(once, twice, "including payload SHA and redaction version");
    assert_eq!(twice, DefinitionPayload::plan(available(&twice), Engine::Mssql));
}
#[test]
fn showplan_character_combinations_and_existing_entities_keep_their_decoded_values() {
    for (encoded, decoded) in [
        ("&#x9;", "\t"),
        ("&#xA;", "\n"),
        ("&#xD;", "\r"),
        ("&#xD;&#xA;", "\r\n"),
        ("&#9;&#10;&#13;", "\t\n\r"),
        ("&#xD;&#x9;&#xA;&#xD;", "\r\t\n\r"),
        ("\t", " "),
        ("\n", " "),
        ("\r", " "),
        ("\r\n", " "),
    ] {
        let raw = format!(
            r#"<ShowPlanXML><ScalarOperator ScalarString="keep{encoded}[kept&amp;name] &lt; 987654321 AND value=N&apos;PRIVATE_ENTITY&apos;"/></ShowPlanXML>"#
        );
        let once = DefinitionPayload::plan(&raw, Engine::Mssql);
        let xml = available(&once);
        assert_eq!(
            attribute(xml, "ScalarOperator", "ScalarString"),
            format!("keep{decoded}[kept&name] < ? AND value=N'?'")
        );
        assert!(!xml.contains("PRIVATE_ENTITY") && !xml.contains("987654321"));
        assert_eq!(once, DefinitionPayload::plan(xml, Engine::Mssql));
    }
    let raw = r#"<ShowPlanXML><Detail>SELECT&#xD;&#xA;&#x9;[kept&amp;name] WHERE secret='PRIVATE_TEXT'</Detail></ShowPlanXML>"#;
    let once = DefinitionPayload::plan(raw, Engine::Mssql);
    let xml = available(&once);
    assert_eq!(
        crate::xml::read(xml, |document| document
            .unwrap()
            .descendants()
            .find(|node| node.is_element() && node.tag_name().name() == "Detail")
            .unwrap()
            .text()
            .unwrap()
            .to_owned()),
        "SELECT\r\n\t[kept&name] WHERE secret='?'"
    );
    assert!(!xml.contains("PRIVATE_TEXT"));
    assert_eq!(once, DefinitionPayload::plan(xml, Engine::Mssql));
}
#[test]
fn psp_boundary_values_are_withheld_for_every_native_spelling_while_ids_and_measurements_survive() {
    for value in [
        "987654321",
        "-987654321.25",
        "N&apos;PRIVATE_BOUNDARY&apos;",
        "PRIVATE_UNQUOTED_BOUNDARY",
        "&#80;RIVATE_ENTITY_BOUNDARY",
        "&quot;PRIVATE_QUOTED_BOUNDARY&quot;",
    ] {
        let raw = format!(
            r#"<ShowPlanXML><StmtSimple StatementId="1"><Dispatcher><ParameterSensitivePredicate LowBoundary="{value}" HighBoundary="{value}"><StatisticsInfo ModificationCount="7" SamplingPercent="100"/></ParameterSensitivePredicate></Dispatcher><QueryPlan QueryVariantID="2"><RelOp NodeId="8" EstimateRows="4.25"><RunTimeInformation><RunTimeCountersPerThread Thread="0" ActualRows="9007199254740993" ActualExecutions="1"/></RunTimeInformation></RelOp></QueryPlan></StmtSimple></ShowPlanXML>"#
        );
        let once = DefinitionPayload::plan(&raw, Engine::Mssql);
        let xml = available(&once);
        assert_eq!(attribute(xml, "ParameterSensitivePredicate", "LowBoundary"), "?");
        assert_eq!(attribute(xml, "ParameterSensitivePredicate", "HighBoundary"), "?");
        assert!(!xml.contains("PRIVATE_") && !xml.contains("987654321"));
        assert_eq!(attribute(xml, "QueryPlan", "QueryVariantID"), "2");
        assert_eq!(attribute(xml, "RelOp", "NodeId"), "8");
        assert_eq!(attribute(xml, "RelOp", "EstimateRows"), "4.25");
        assert_eq!(attribute(xml, "StatisticsInfo", "ModificationCount"), "7");
        assert_eq!(attribute(xml, "StatisticsInfo", "SamplingPercent"), "100");
        assert_eq!(attribute(xml, "RunTimeCountersPerThread", "ActualRows"), "9007199254740993");
        assert_eq!(once, DefinitionPayload::plan(xml, Engine::Mssql));
    }
}
