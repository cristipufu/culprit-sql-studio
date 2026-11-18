use super::*;

#[test]
fn session_census_is_a_closed_existing_tool_choice_with_no_sql_path_or_source_configuration() {
    let parsed: LocalCaptureArgs = serde_json::from_value(json!({
        "connection":"local-evidence","observation_id":"a".repeat(32),"analysis":"session_census"
    }))
    .unwrap();
    assert_eq!(parsed.analysis, LocalCaptureAnalysisKind::SessionCensus);
    for extra in ["sql", "path", "endpoint", "native_profile"] {
        let mut value =
            json!({"connection":"local-evidence","observation_id":"a".repeat(32),"analysis":"session_census"});
        value[extra] = json!("must not be accepted");
        assert!(serde_json::from_value::<LocalCaptureArgs>(value).is_err());
    }
}
