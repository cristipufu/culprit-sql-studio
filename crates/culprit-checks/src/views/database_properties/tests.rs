use super::*;
fn read() -> DatabasePropertiesRead {
    let point: DatabasePropertiesObservation = serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../fixtures/database-properties.native.json"
    )))
    .unwrap();
    DatabasePropertiesRead {
        target: culprit_core::DatabaseIdentity::server(
            culprit_core::Engine::Mssql,
            "sample",
            "fixture.invalid",
            1433,
            None,
        )
        .into(),
        context: ObservationReadContext::Collector {
            observed_from: "2020-01-01T12:00:00Z".into(),
            observed_until: "2020-01-01T12:00:01Z".into(),
        },
        source: StateSourceCoverage::collected(DATABASE_PROPERTIES_SOURCE.into(), 1),
        observation: Some(Box::new(point)),
        read_complete: true,
    }
}
fn field<'a>(report: &'a DatabasePropertiesReport, key: &str) -> &'a DatabasePropertyField {
    report.fields.iter().find(|field| field.key == key).unwrap()
}
#[test]
fn one_interpretation_preserves_native_codes_zero_unknown_and_independent_connected_updateability() {
    let mut read = read();
    let point = read.observation.as_mut().unwrap();
    point.updateability = Some("READ_ONLY".into());
    point.catalog_state.code = Some(255);
    point.auto_update_statistics_async = None;
    let report = explain(&read).unwrap();
    assert_eq!(report.fields.len(), 30);
    assert_eq!(field(&report, "catalog_read_only").value.as_deref(), Some(t!("view.database_properties.no").as_str()));
    assert_eq!(
        field(&report, "updateability").value.as_deref(),
        Some(t!("view.database_properties.read_only").as_str())
    );
    assert_eq!(field(&report, "target_recovery_time_seconds").value.as_deref(), Some("0"));
    assert!(field(&report, "catalog_state").value.is_none());
    assert_eq!(field(&report, "catalog_state").native_code.as_deref(), Some("255"));
    assert_eq!(field(&report, "catalog_state").native_description.as_deref(), Some("ONLINE"));
    assert!(field(&report, "auto_update_statistics_async").value.is_none());
    assert!(serde_json::to_value(&report).unwrap().get("fix_sql").is_none());
}
#[test]
fn persisted_raw_properties_reanalyze_to_the_same_report_and_sharing_keeps_an_explicit_gap() {
    let mut snapshot: Snapshot =
        serde_json::from_str(include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../fixtures/mssql-demo.json")))
            .unwrap();
    let mut read = read();
    read.observation.as_mut().unwrap().collation = Some("private-collation-canary".into());
    snapshot.operations.database_properties = Some(Box::new(read));
    let before = explain_snapshot(&snapshot).unwrap().unwrap();
    let restored: Snapshot = serde_json::from_slice(&serde_json::to_vec(&snapshot).unwrap()).unwrap();
    let after = explain_snapshot(&restored).unwrap().unwrap();
    assert_eq!(serde_json::to_value(before).unwrap(), serde_json::to_value(after).unwrap());
    let shared = culprit_core::redact::redact_snapshot(&snapshot);
    assert!(shared.operations.database_properties.is_none());
    assert!(!serde_json::to_string(&shared).unwrap().contains("private-collation-canary"));
    let shared_report = explain_snapshot(&shared).unwrap().unwrap();
    assert_eq!(shared_report.source_state, Some(SourceState::Unavailable));
    assert!(shared_report.fields.is_empty());
}
#[test]
fn body_read_gaps_and_unavailable_or_inapplicable_sources_never_become_measured_empty_properties() {
    let mut read = read();
    read.observation = None;
    read.read_complete = false;
    let report = explain(&read).unwrap();
    assert_eq!(report.source_state, Some(SourceState::Partial));
    assert_eq!(report.native_source.as_ref().unwrap().records, 1);
    assert!(report.fields.is_empty());
    for source in [
        StateSourceCoverage::unavailable(
            DATABASE_PROPERTIES_SOURCE.into(),
            culprit_core::ErrorCode::PermissionDenied,
            None,
        ),
        StateSourceCoverage::not_applicable(DATABASE_PROPERTIES_SOURCE.into()),
    ] {
        read.source = source.clone();
        read.read_complete = true;
        assert_eq!(explain(&read).unwrap().source_state, Some(source.state));
    }
    read.source = StateSourceCoverage::collected(DATABASE_PROPERTIES_SOURCE.into(), 1);
    assert!(explain(&read).is_err(), "read-complete must not hide a missing original body");
}

#[test]
fn last_recorded_checkdb_is_informational_primary_reported_metadata_with_unknown_offset() {
    for (native, expected) in [
        (Some("2019-12-28T23:10:00.003"), Some("2019-12-28T23:10:00.003".to_owned())),
        (Some("1900-01-01T00:00:00.000"), Some(t!("view.database_properties.checkdb_not_recorded"))),
        (None, None),
    ] {
        let mut read = read();
        let point = read.observation.as_mut().unwrap();
        point.last_good_checkdb_without_offset = native.map(str::to_owned);
        point.updateability = Some("READ_ONLY".into());
        let report = explain(&read).unwrap();
        let check = field(&report, "last_good_checkdb_without_offset");
        assert_eq!(check.value, expected);
        assert_eq!(check.label, t!("view.database_properties.field.last_good_checkdb"));
        assert!(report.limitations.contains(&t!("core.engines.mssql.integrity_history")));
        assert_eq!(report.observed_at.as_deref(), Some("2020-01-01T12:00:00.0000001Z"));
        assert_eq!(read.observation.as_ref().unwrap().last_good_checkdb_without_offset.as_deref(), native);
        assert!(!serde_json::to_string(&report).unwrap().contains("age_seconds"));
    }
}

#[test]
fn auto_shrink_remains_recorded_configuration_with_an_explicit_unknown_and_no_new_diagnosis() {
    for (native, expected) in [
        (Some(true), Some(t!("view.database_properties.enabled"))),
        (Some(false), Some(t!("view.database_properties.disabled"))),
        (None, None),
    ] {
        let mut original = read();
        original.observation.as_mut().unwrap().auto_shrink = native;
        let report = explain(&original).unwrap();
        let property = field(&report, "auto_shrink");
        assert_eq!(property.label, t!("view.database_properties.field.auto_shrink"));
        assert_eq!(property.value, expected);
        assert_eq!(report.summary, t!("view.database_properties.summary.recorded"));
        assert_eq!(property.native_code, None);
        assert_eq!(property.native_description, None);
        let value = serde_json::to_value(report).unwrap();
        for verdict in ["severity", "cause", "fix_sql", "current_shrink"] {
            assert!(value.get(verdict).is_none());
        }
    }
}

#[test]
fn standby_and_delayed_durability_are_independent_catalog_facts_not_role_transaction_or_health_verdicts() {
    for (standby, code, expected) in [
        (Some(true), Some(0), Some(t!("view.database_properties.disabled"))),
        (Some(false), Some(1), Some(t!("view.database_properties.state.allowed"))),
        (None, Some(2), Some(t!("view.database_properties.state.forced"))),
        (None, Some(255), None),
        (None, None, None),
    ] {
        let mut read = read();
        let point = read.observation.as_mut().unwrap();
        point.in_standby = standby;
        point.delayed_durability = DatabasePropertyCode { code, description: Some("DISABLED".into()) };
        let report = explain(&read).unwrap();
        assert_eq!(
            field(&report, "in_standby").value,
            standby.map(|value| if value {
                t!("view.database_properties.yes")
            } else {
                t!("view.database_properties.no")
            })
        );
        assert_eq!(field(&report, "delayed_durability").value, expected);
        assert_eq!(field(&report, "delayed_durability").native_description.as_deref(), Some("DISABLED"));
        assert_eq!(field(&report, "delayed_durability").native_code, code.map(|value| value.to_string()));
        assert_eq!(field(&report, "updateability").value, Some(t!("view.database_properties.read_write")));
        assert_eq!(report.summary, t!("view.database_properties.summary.recorded"));
    }
}

#[test]
fn service_broker_enablement_remains_informational_in_partial_sources_without_message_or_queue_claims() {
    for (native, expected) in [
        (Some(true), Some(t!("view.database_properties.enabled"))),
        (Some(false), Some(t!("view.database_properties.disabled"))),
        (None, None),
    ] {
        let mut original = read();
        original.observation.as_mut().unwrap().broker_enabled = native;
        original.source = StateSourceCoverage::observed(DATABASE_PROPERTIES_SOURCE.into(), 1, false);
        let report = explain(&original).unwrap();
        let broker = field(&report, "broker_enabled");
        assert_eq!(broker.label, t!("view.database_properties.field.broker_enabled"));
        assert_eq!(broker.value, expected);
        assert_eq!(broker.native_code, None);
        assert_eq!(broker.native_description, None);
        assert_eq!(report.source_state, Some(SourceState::Partial));
        assert_eq!(report.summary, t!("view.database_properties.summary.recorded"));
        let value = serde_json::to_value(report).unwrap();
        for verdict in ["severity", "cause", "message_delivery", "queue_progress", "fix_sql"] {
            assert!(value.get(verdict).is_none());
        }
    }
}

#[test]
fn independent_feature_and_replication_bits_are_recorded_configuration_even_with_partial_coverage() {
    let names = [
        "encrypted",
        "ledger_enabled",
        "change_feed_enabled",
        "published",
        "subscribed",
        "merge_published",
        "distributor",
        "accelerated_recovery_enabled",
    ];
    for selected in names {
        for native in [Some(true), Some(false), None] {
            let mut original = read();
            let mut value = serde_json::to_value(original.observation.as_ref().unwrap()).unwrap();
            value[selected] = serde_json::to_value(native).unwrap();
            original.observation = Some(Box::new(serde_json::from_value(value).unwrap()));
            original.source = StateSourceCoverage::observed(DATABASE_PROPERTIES_SOURCE.into(), 1, false);
            let report = explain(&original).unwrap();
            let property = field(&report, selected);
            let enabled_wording =
                matches!(selected, "encrypted" | "change_feed_enabled" | "accelerated_recovery_enabled");
            assert_eq!(property.value, if enabled_wording { enabled(native) } else { yes_no(native) });
            assert!(property.native_code.is_none());
            assert!(property.native_description.is_none());
            assert_eq!(report.source_state, Some(SourceState::Partial));
            assert_eq!(report.summary, t!("view.database_properties.summary.recorded"));
            assert!(report.limitations.contains(&t!("view.database_properties.limit.features")));
            for other in names.into_iter().filter(|name| *name != selected) {
                let expected = if matches!(other, "encrypted" | "change_feed_enabled" | "accelerated_recovery_enabled")
                {
                    enabled(Some(false))
                } else {
                    yes_no(Some(false))
                };
                assert_eq!(field(&report, other).value, expected, "{selected} must not overwrite {other}");
            }
            let json = serde_json::to_value(report).unwrap();
            for claim in
                ["severity", "cause", "fix_sql", "replication_progress", "encryption_complete", "version_store_bytes"]
            {
                assert!(json.get(claim).is_none());
            }
        }
    }
}

#[test]
fn containment_interpretation_uses_only_the_original_native_code_and_retains_unknown_descriptions() {
    for (code, expected) in [
        (Some(0), Some(t!("view.database_properties.state.not_contained"))),
        (Some(1), Some(t!("view.database_properties.state.partially_contained"))),
        (Some(255), None),
        (None, None),
    ] {
        let mut original = read();
        original.observation.as_mut().unwrap().containment =
            DatabasePropertyCode { code, description: Some("PARTIAL".into()) };
        let report = explain(&original).unwrap();
        let property = field(&report, "containment");
        assert_eq!(property.value, expected);
        assert_eq!(property.native_code, code.map(|value| value.to_string()));
        assert_eq!(property.native_description.as_deref(), Some("PARTIAL"));
        assert_eq!(report.observed_at, Some(original.observation.as_ref().unwrap().observed_at.clone()));
    }
}
