use super::*;

fn description(records: u32, complete: bool) -> ActivitySnapshotDescription {
    ActivitySnapshotDescription {
        epoch: Some("native-process-epoch".into()),
        observed_from: "2020-01-01T06:00:00Z".into(),
        at: "2020-01-01T06:00:00.100000Z".into(),
        observed_until: "2020-01-01T06:00:00.200000Z".into(),
        scope: crate::MeasurementScope::Database,
        population: ActivityPopulation::ClientBackends,
        native_rows: records,
        observations: records,
        coverage: StateSourceCoverage::observed("activity".into(), records, complete),
    }
}

#[test]
fn quiet_and_short_wait_baselines_do_not_require_an_incident_policy_or_condition() {
    let mut ordinary = tests::request("1");
    ordinary.elapsed_ms = Some("10".into());
    assert!(ordinary.conditions(&ActivityPolicy::default()).is_empty());
    assert!(ActivityEvidence::validate(&[ordinary]).is_ok());
    assert!(description(1, true).is_valid());
    assert!(description(0, true).is_valid());
    let value = serde_json::to_value(description(1, true)).unwrap();
    for removed in ["policy", "qualifying", "uncertain", "blocking_edges", "details", "complete_scope"] {
        assert!(value.get(removed).is_none(), "{removed} is not a native baseline fact");
    }
}

#[test]
fn a_fully_read_prefix_keeps_positive_partial_coverage_without_absence_authority() {
    let mut prefix = description(2, false);
    prefix.native_rows = 3;
    prefix.coverage.stopped = Some(CollectionStop::RowLimit);
    assert!(prefix.is_valid());
    assert_eq!(prefix.coverage.state, crate::SourceState::Partial);
    assert!(!prefix.coverage.complete_scope);
    let encoded = serde_json::to_string(&prefix).unwrap();
    assert_eq!(serde_json::from_str::<ActivitySnapshotDescription>(&encoded).unwrap(), prefix);
    let mut hidden = description(0, false);
    hidden.native_rows = 1;
    hidden.coverage.error_code = Some(crate::ErrorCode::PermissionDenied);
    assert!(hidden.is_valid(), "a hidden actor is not a measured empty population");
    let mut unqualified = description(1, false);
    unqualified.epoch = None;
    assert!(unqualified.is_valid(), "positive native actors can remain unowned");
    unqualified.coverage = StateSourceCoverage::collected("activity".into(), 1);
    assert!(!unqualified.is_valid(), "an unknown epoch cannot authorize absence");
}

#[test]
fn baseline_clocks_counts_and_source_state_are_validated_as_one_contract() {
    let original = description(1, true);
    for field in ["observed_from", "observed_until", "at", "observations", "native_rows", "coverage"] {
        let mut value = serde_json::to_value(&original).unwrap();
        value[field] = match field {
            "observed_from" => serde_json::json!("2020-01-01T07:00:00Z"),
            "observed_until" => serde_json::json!("2020-01-01T05:00:00Z"),
            "at" => serde_json::json!("not-a-clock"),
            "observations" => serde_json::json!(0),
            "native_rows" => serde_json::json!(0),
            "coverage" => {
                serde_json::json!({"source":"activity","state":"partial","records":1,"complete_scope":true,"error_code":null,"stopped":null})
            }
            _ => unreachable!(),
        };
        assert!(!serde_json::from_value::<ActivitySnapshotDescription>(value).unwrap().is_valid(), "{field}");
    }
    let mut old_shape = serde_json::to_value(original).unwrap();
    old_shape["policy"] = serde_json::to_value(ActivityPolicy::default()).unwrap();
    assert!(serde_json::from_value::<ActivitySnapshotDescription>(old_shape).is_err());
}

#[test]
fn unknown_transactions_and_statement_context_do_not_become_measured_zero_or_idle_ownership() {
    let mut row = tests::request("1");
    row.open_transactions = None;
    row.statement_identity = Some(NativeIdentity::new("native-statement").qualified("source", "native-metadata"));
    assert!(row.is_valid());
    let encoded = serde_json::to_value(&row).unwrap();
    assert!(encoded["open_transactions"].is_null());
    row.idle = true;
    row.key.request = None;
    row.request_counters = None;
    assert!(!row.is_valid(), "a current-statement reference cannot name an idle previous batch");
    row.statement_identity = None;
    assert!(row.is_valid());
}
