use super::*;
#[test]
fn withdrawn_history_retains_unknown_scope_without_fabricated_bounds_or_references() {
    let value = value(
        ("History", "Unavailable"),
        SourceState::Unavailable,
        false,
        None,
        &["Original withheld".into()],
        &[],
        &[],
    );
    assert!(value["original_read_bounds"].is_null());
    assert_eq!(value["read_complete"], false);
    assert_eq!(value["source_state"], "unavailable");
    assert_eq!(value["entries_available"], 0);
    assert_eq!(value["historical_context"], true);
}
#[test]
fn compact_fields_remain_nullable_bounded_and_independent_of_private_observation_keys() {
    let field = culprit_core::ChangeCaptureField { key: "native".into(), label: "Native value".into(), value: None };
    let entry = ChangeCaptureEntry {
        source: "history".into(),
        observation_key: "private-original-reference".into(),
        kind: culprit_core::ChangeCaptureEntryKind::Error,
        observed_at: "2020-01-01T00:00:00Z".into(),
        title: "Observed error".into(),
        explanation: "Original context".into(),
        fields: vec![field; 13],
        investigation: "Investigate".into(),
        verify: "Verify".into(),
    };
    let value = value(
        ("History", "Recorded"),
        SourceState::Partial,
        false,
        None,
        &[],
        &vec![entry; 7],
        &[culprit_core::telemetry::StateSourceCoverage::unavailable(
            "Native errors".into(),
            culprit_core::ErrorCode::PermissionDenied,
            None,
        )],
    );
    assert_eq!(value["sources"][0]["error_code"], json!(culprit_core::ErrorCode::PermissionDenied));
    assert_eq!(value["entries_available"], 7);
    assert_eq!(value["entries_presented"], 5);
    assert_eq!(value["entries_omitted"], 2);
    assert_eq!(value["entries"][0]["fields"].as_array().unwrap().len(), 8);
    assert!(value["entries"][0]["fields"][0]["value"].is_null());
    assert!(!value.to_string().contains("private-original-reference"));
}
