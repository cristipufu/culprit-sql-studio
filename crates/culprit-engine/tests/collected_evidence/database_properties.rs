use super::fixture::Fixture;
use culprit_collector::analytical::{
    evidence::{EvidenceFamily, EvidenceGapReason},
    test_support::{self, DatabasePropertiesCase},
};
use culprit_core::{SourceState, telemetry::*};
use culprit_engine::collected_resources;

#[tokio::test]
async fn database_properties_replay_keeps_original_native_namespace_codes_unknowns_and_pure_meanings() {
    for case in DatabasePropertiesCase::ALL {
        let (source, rows) = test_support::database_properties_evidence(case);
        let fixture = Fixture::new(source.clone(), rows.clone()).await;
        let opened = fixture.recovery_object_reads();
        let capture = fixture.read_families(&[EvidenceFamily::OperationalSamples]).await;
        test_support::assert_operational_capture(&capture, &source, &rows);
        let analysis = collected_resources::database_properties(&capture).unwrap();
        assert_eq!(analysis.context.source, source);
        assert_eq!(analysis.report.native_source.as_ref(), Some(&source.observation.snapshot().unwrap().sources[0]));
        assert_eq!(analysis.report.read_complete, Some(true));
        let read = DatabasePropertiesRead::from_source(
            &source,
            rows.into_iter()
                .map(|row| {
                    let TelemetryRecord::Operational(row) = row else { panic!() };
                    row
                })
                .collect(),
            true,
        )
        .unwrap();
        let expected = culprit_checks::database_properties::explain(&read).unwrap();
        assert_eq!(serde_json::to_value(&analysis.report).unwrap(), serde_json::to_value(expected).unwrap());
        assert_eq!(fixture.recovery_object_reads(), opened);
        if case == DatabasePropertiesCase::Exact {
            assert_eq!(analysis.report.fields.len(), 37);
            assert_eq!(analysis.report.native_database_id.as_deref(), Some("5"));
            assert_eq!(
                analysis
                    .report
                    .fields
                    .iter()
                    .find(|field| field.key == "target_recovery_time_seconds")
                    .unwrap()
                    .value
                    .as_deref(),
                Some("0")
            );
        }
        if let Some(point) = &read.observation {
            for (key, expected) in [
                ("native_database_id", Some(point.native_database_id.clone())),
                ("catalog_database_id", Some(point.catalog_database_id.clone())),
                ("catalog_created_without_offset", point.catalog_created_without_offset.clone()),
            ] {
                assert_eq!(
                    analysis.report.fields.iter().find(|field| field.key == key).unwrap().value,
                    expected,
                    "{case:?}/{key}"
                );
            }
            assert!(
                analysis
                    .report
                    .limitations
                    .contains(&culprit_core::t!("view.database_properties.limit.catalog_origin"))
            );
            assert!(
                analysis
                    .report
                    .limitations
                    .contains(&culprit_core::t!("view.database_properties.limit.local_replica_role"))
            );
            for (name, native, enabled_case, enabled_wording) in [
                ("encrypted", point.encrypted, DatabasePropertiesCase::Encrypted, true),
                ("ledger_enabled", point.ledger_enabled, DatabasePropertiesCase::LedgerEnabled, false),
                ("change_feed_enabled", point.change_feed_enabled, DatabasePropertiesCase::ChangeFeedEnabled, true),
                ("published", point.published, DatabasePropertiesCase::Published, false),
                ("subscribed", point.subscribed, DatabasePropertiesCase::Subscribed, false),
                ("merge_published", point.merge_published, DatabasePropertiesCase::MergePublished, false),
                ("distributor", point.distributor, DatabasePropertiesCase::Distributor, false),
                (
                    "accelerated_recovery_enabled",
                    point.accelerated_recovery_enabled,
                    DatabasePropertiesCase::AcceleratedRecoveryEnabled,
                    true,
                ),
            ] {
                let expected = (case != DatabasePropertiesCase::UnknownValues).then_some(case == enabled_case);
                assert_eq!(native, expected, "{name}");
                let property = analysis.report.fields.iter().find(|field| field.key == name).unwrap();
                let displayed = expected.map(|value| match (enabled_wording, value) {
                    (true, true) => culprit_core::t!("view.database_properties.enabled"),
                    (true, false) => culprit_core::t!("view.database_properties.disabled"),
                    (false, true) => culprit_core::t!("view.database_properties.yes"),
                    (false, false) => culprit_core::t!("view.database_properties.no"),
                });
                assert_eq!(property.value, displayed, "{name}");
            }
            let containment = analysis.report.fields.iter().find(|field| field.key == "containment").unwrap();
            assert_eq!(
                point.containment.code,
                match case {
                    DatabasePropertiesCase::PartiallyContained => Some(1),
                    DatabasePropertiesCase::UnknownValues => None,
                    DatabasePropertiesCase::UnknownCodes => Some(255),
                    _ => Some(0),
                }
            );
            assert_eq!(containment.native_description, point.containment.description);
            assert_eq!(
                containment.value,
                match case {
                    DatabasePropertiesCase::PartiallyContained =>
                        Some(culprit_core::t!("view.database_properties.state.partially_contained")),
                    DatabasePropertiesCase::UnknownValues | DatabasePropertiesCase::UnknownCodes => None,
                    _ => Some(culprit_core::t!("view.database_properties.state.not_contained")),
                }
            );
            assert!(analysis.report.limitations.contains(&culprit_core::t!("view.database_properties.limit.features")));
            let expected_auto_shrink = match case {
                DatabasePropertiesCase::AutoShrinkEnabled => Some(true),
                DatabasePropertiesCase::UnknownValues => None,
                _ => Some(false),
            };
            assert_eq!(point.auto_shrink, expected_auto_shrink);
            let broker_enabled = match case {
                DatabasePropertiesCase::BrokerEnabled => Some(true),
                DatabasePropertiesCase::UnknownValues => None,
                _ => Some(false),
            };
            assert_eq!(point.broker_enabled, broker_enabled);
            let broker = analysis.report.fields.iter().find(|field| field.key == "broker_enabled").unwrap();
            assert_eq!(
                broker.value,
                broker_enabled.map(|value| if value {
                    culprit_core::t!("view.database_properties.enabled")
                } else {
                    culprit_core::t!("view.database_properties.disabled")
                })
            );
            assert_eq!(
                point.in_standby,
                match case {
                    DatabasePropertiesCase::Standby => Some(true),
                    DatabasePropertiesCase::UnknownValues => None,
                    _ => Some(false),
                }
            );
            assert_eq!(
                point.delayed_durability.code,
                match case {
                    DatabasePropertiesCase::DelayedDurabilityAllowed => Some(1),
                    DatabasePropertiesCase::DelayedDurabilityForced => Some(2),
                    DatabasePropertiesCase::UnknownValues => None,
                    DatabasePropertiesCase::UnknownCodes => Some(255),
                    _ => Some(0),
                }
            );
            let delayed = analysis.report.fields.iter().find(|field| field.key == "delayed_durability").unwrap();
            assert_eq!(delayed.native_description, point.delayed_durability.description);
            if matches!(case, DatabasePropertiesCase::UnknownValues | DatabasePropertiesCase::UnknownCodes) {
                assert!(delayed.value.is_none());
            }

            let auto_shrink = analysis.report.fields.iter().find(|field| field.key == "auto_shrink").unwrap();
            assert_eq!(
                auto_shrink.value,
                expected_auto_shrink.map(|enabled| if enabled {
                    culprit_core::t!("view.database_properties.enabled")
                } else {
                    culprit_core::t!("view.database_properties.disabled")
                })
            );
            let expected_native = match case {
                DatabasePropertiesCase::UnknownValues => None,
                DatabasePropertiesCase::NoRecordedCheckDb => Some("1900-01-01T00:00:00.000"),
                _ => Some("2019-12-28T23:10:00.003"),
            };
            assert_eq!(point.last_good_checkdb_without_offset.as_deref(), expected_native);
            let field =
                analysis.report.fields.iter().find(|field| field.key == "last_good_checkdb_without_offset").unwrap();
            let expected_display = match case {
                DatabasePropertiesCase::UnknownValues => None,
                DatabasePropertiesCase::NoRecordedCheckDb => {
                    Some(culprit_core::t!("view.database_properties.checkdb_not_recorded"))
                }
                _ => expected_native.map(str::to_owned),
            };
            assert_eq!(field.value, expected_display);
            assert!(analysis.report.limitations.contains(&culprit_core::t!("core.engines.mssql.integrity_history")));
            assert_eq!(analysis.report.observed_at.as_deref(), Some("2020-01-01T12:00:00.0000001Z"));
        }
        if case == DatabasePropertiesCase::UnknownCodes {
            let state = analysis.report.fields.iter().find(|field| field.key == "catalog_state").unwrap();
            assert!(state.value.is_none());
            assert_eq!(state.native_code.as_deref(), Some("255"));
            assert_eq!(state.native_description.as_deref(), Some("ONLINE"));
        }
    }
}

#[tokio::test]
async fn missing_property_body_preserves_original_source_count_and_partial_read_without_hydration() {
    let (source, rows) = test_support::database_properties_evidence(DatabasePropertiesCase::Exact);
    let fixture = Fixture::new(source.clone(), rows).await;
    fixture.remove_first_row(TelemetryStream::OperationalSamples);
    let opened = fixture.recovery_object_reads();
    let capture = fixture.read_families(&[EvidenceFamily::OperationalSamples]).await;
    assert!(!capture.is_read_complete());
    assert!(capture.gaps().iter().any(|gap| gap.reason == EvidenceGapReason::MissingRows));
    let analysis = collected_resources::database_properties(&capture).unwrap();
    assert_eq!(analysis.context.source, source);
    assert_eq!(analysis.report.native_source.as_ref().unwrap().records, 1);
    assert_eq!(analysis.report.source_state, Some(SourceState::Partial));
    assert_eq!(analysis.report.read_complete, Some(false));
    assert!(analysis.report.fields.is_empty());
    assert!(analysis.report.observed_at.is_none());
    assert_eq!(fixture.recovery_object_reads(), opened);
}
