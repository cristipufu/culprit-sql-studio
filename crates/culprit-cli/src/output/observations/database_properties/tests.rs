use super::*;
use culprit_collector::analytical::test_support::{self, DatabasePropertiesCase};
use culprit_core::telemetry::*;
#[test]
fn text_retains_native_code_unknown_zero_clock_and_original_source_without_configuration_advice() {
    let (source, rows) = test_support::database_properties_evidence(DatabasePropertiesCase::UnknownCodes);
    let rows = rows
        .into_iter()
        .map(|row| {
            let TelemetryRecord::Operational(row) = row else { panic!() };
            row
        })
        .collect();
    let read = DatabasePropertiesRead::from_source(&source, rows, true).unwrap();
    let report = culprit_checks::database_properties::explain(&read).unwrap();
    let rendered = database_properties(&report);
    assert!(rendered.contains("255"));
    assert!(rendered.contains("ONLINE"));
    assert!(rendered.contains(&t!("view.database_properties.unknown")));
    assert!(rendered.contains("2020-01-01T12:00:00Z"));
    assert!(rendered.contains(&t!("view.database_properties.field.last_good_checkdb")));
    assert!(rendered.contains("2019-12-28T23:10:00.003"));
    assert!(rendered.contains(&t!("core.engines.mssql.integrity_history")));
    assert!(!rendered.contains("ALTER DATABASE"));
    assert!(!rendered.contains("fixture.invalid"));
}

#[test]
fn a_native_no_record_sentinel_and_unknown_metadata_keep_distinct_plain_text_values() {
    for (case, expected) in [
        (DatabasePropertiesCase::NoRecordedCheckDb, t!("view.database_properties.checkdb_not_recorded")),
        (DatabasePropertiesCase::UnknownValues, t!("view.database_properties.unknown")),
    ] {
        let (source, rows) = test_support::database_properties_evidence(case);
        let rows = rows
            .into_iter()
            .map(|row| {
                let TelemetryRecord::Operational(row) = row else { panic!() };
                row
            })
            .collect();
        let read = DatabasePropertiesRead::from_source(&source, rows, true).unwrap();
        let report = culprit_checks::database_properties::explain(&read).unwrap();
        let rendered = database_properties(&report);
        assert!(rendered.contains(&t!(
            "view.counter_ratios.cli.metric",
            label = t!("view.database_properties.field.last_good_checkdb"),
            value = expected
        )));
        assert!(!rendered.contains("1900-01-01"));
        assert!(!rendered.contains("DBCC CHECKDB"));
        assert!(
            read.observation
                .as_ref()
                .unwrap()
                .last_good_checkdb_without_offset
                .as_deref()
                .is_none_or(|value| value == "1900-01-01T00:00:00.000")
        );
    }
}

#[test]
fn canonical_auto_shrink_configuration_and_unknown_are_rendered_without_a_shrink_command() {
    for (case, expected) in [
        (DatabasePropertiesCase::AutoShrinkEnabled, t!("view.database_properties.enabled")),
        (DatabasePropertiesCase::Exact, t!("view.database_properties.disabled")),
        (DatabasePropertiesCase::UnknownValues, t!("view.database_properties.unknown")),
    ] {
        let (source, rows) = test_support::database_properties_evidence(case);
        let rows = rows
            .into_iter()
            .map(|row| match row {
                TelemetryRecord::Operational(row) => row,
                _ => panic!(),
            })
            .collect();
        let read = DatabasePropertiesRead::from_source(&source, rows, true).unwrap();
        let report = culprit_checks::database_properties::explain(&read).unwrap();
        let text = database_properties(&report);
        assert!(text.contains(&t!(
            "view.counter_ratios.cli.metric",
            label = t!("view.database_properties.field.auto_shrink"),
            value = expected
        )));
        assert!(!text.contains("ALTER DATABASE"));
        assert!(!text.contains("DBCC SHRINK"));
    }
}

#[test]
fn canonical_standby_and_delayed_durability_states_are_present_without_restore_or_durability_advice() {
    for (case, key, label, expected) in [
        (
            DatabasePropertiesCase::Standby,
            "in_standby",
            t!("view.database_properties.field.in_standby"),
            t!("view.database_properties.yes"),
        ),
        (
            DatabasePropertiesCase::DelayedDurabilityAllowed,
            "delayed_durability",
            t!("view.database_properties.field.delayed_durability"),
            t!("view.database_properties.state.allowed"),
        ),
        (
            DatabasePropertiesCase::DelayedDurabilityForced,
            "delayed_durability",
            t!("view.database_properties.field.delayed_durability"),
            t!("view.database_properties.state.forced"),
        ),
    ] {
        let (source, rows) = test_support::database_properties_evidence(case);
        let rows = rows
            .into_iter()
            .map(|row| match row {
                TelemetryRecord::Operational(row) => row,
                _ => panic!(),
            })
            .collect();
        let read = DatabasePropertiesRead::from_source(&source, rows, true).unwrap();
        let report = culprit_checks::database_properties::explain(&read).unwrap();
        assert_eq!(
            report.fields.iter().find(|field| field.key == key).unwrap().value.as_deref(),
            Some(expected.as_str())
        );
        let text = database_properties(&report);
        assert!(text.contains(&t!("view.counter_ratios.cli.metric", label = label, value = expected)));
        assert!(!text.contains("ALTER DATABASE"));
        assert!(!text.contains("RESTORE LOG"));
    }
}

#[test]
fn canonical_service_broker_configuration_and_unknown_render_without_enablement_ddl_or_delivery_advice() {
    for (case, expected) in [
        (DatabasePropertiesCase::BrokerEnabled, t!("view.database_properties.enabled")),
        (DatabasePropertiesCase::Exact, t!("view.database_properties.disabled")),
        (DatabasePropertiesCase::UnknownValues, t!("view.database_properties.unknown")),
    ] {
        let (source, rows) = test_support::database_properties_evidence(case);
        let rows = rows
            .into_iter()
            .map(|row| match row {
                TelemetryRecord::Operational(row) => row,
                _ => panic!(),
            })
            .collect();
        let read = DatabasePropertiesRead::from_source(&source, rows, true).unwrap();
        let report = culprit_checks::database_properties::explain(&read).unwrap();
        let rendered = database_properties(&report);
        assert!(rendered.contains(&t!(
            "view.counter_ratios.cli.metric",
            label = t!("view.database_properties.field.broker_enabled"),
            value = expected
        )));
        assert!(!rendered.contains("ALTER DATABASE"));
        assert!(!rendered.contains("ENABLE_BROKER"));
        assert!(!rendered.contains("NEW_BROKER"));
    }
}

#[test]
fn canonical_catalog_features_keep_their_exact_field_values_and_limits_in_plain_text() {
    for case in DatabasePropertiesCase::ALL {
        let (source, rows) = test_support::database_properties_evidence(case);
        if rows.is_empty() {
            continue;
        }
        let rows = rows
            .into_iter()
            .map(|row| match row {
                TelemetryRecord::Operational(row) => row,
                _ => panic!(),
            })
            .collect();
        let read = DatabasePropertiesRead::from_source(&source, rows, true).unwrap();
        let report = culprit_checks::database_properties::explain(&read).unwrap();
        let rendered = database_properties(&report);
        for key in [
            "native_database_id",
            "catalog_database_id",
            "catalog_created_without_offset",
            "local_replica_role",
            "local_replica_role_source",
            "local_replica_role_observed_at",
            "local_replica_role_error_number",
            "containment",
            "encrypted",
            "ledger_enabled",
            "change_feed_enabled",
            "published",
            "subscribed",
            "merge_published",
            "distributor",
            "accelerated_recovery_enabled",
        ] {
            let field = report.fields.iter().find(|field| field.key == key).unwrap();
            assert!(rendered.contains(&t!(
                "view.counter_ratios.cli.metric",
                label = field.label.clone(),
                value = field.value.clone().unwrap_or_else(|| t!("view.database_properties.unknown"))
            )));
        }
        assert!(rendered.contains(&t!("view.database_properties.limit.features")));
        assert!(rendered.contains(&t!("view.database_properties.limit.catalog_origin")));
        assert!(rendered.contains(&t!("view.database_properties.limit.local_replica_role")));
        for command in ["ALTER DATABASE", "sp_replicationdboption", "DBCC", "fixture.invalid"] {
            assert!(!rendered.contains(command));
        }
    }
}
