use super::*;

#[test]
fn native_last_values_are_exact_and_averages_remain_independent_approximate_measurements() {
    let timings = MssqlCompilationTimings {
        last_duration_us: Some("9007199254740993".into()),
        binding: MssqlCompilationStage {
            average_duration_us: Some(125.5),
            last_duration_us: Some("0".into()),
            average_cpu_us: None,
            last_cpu_us: Some(i64::MAX.to_string()),
        },
        optimization: MssqlCompilationStage {
            average_duration_us: None,
            last_duration_us: None,
            average_cpu_us: Some(0.0),
            last_cpu_us: None,
        },
    };
    assert!(timings.is_valid());
    let encoded = serde_json::to_value(&timings).unwrap();
    assert_eq!(encoded["last_duration_us"], "9007199254740993");
    assert_eq!(encoded["binding"]["last_cpu_us"], i64::MAX.to_string());
    assert_eq!(encoded["binding"]["last_duration_us"], "0");
    assert!(encoded["binding"]["average_cpu_us"].is_null());
    assert_eq!(encoded["optimization"]["average_cpu_us"], 0.0);
    assert_eq!(serde_json::from_value::<MssqlCompilationTimings>(encoded.clone()).unwrap(), timings);
    for invented in ["total_cpu_us", "total_duration_us", "concurrent_compilations", "active_memory_kb"] {
        assert!(!encoded.to_string().contains(invented));
    }
}

#[test]
fn invalid_bigints_or_nonfinite_negative_averages_do_not_become_valid_zero_measurements() {
    for value in ["", "-1", "00", "1.0", "1e3", "9223372036854775808", "18446744073709551615"] {
        let mut timings = MssqlCompilationTimings { last_duration_us: Some(value.into()), ..Default::default() };
        assert!(!timings.is_valid(), "{value}");
        timings.last_duration_us = None;
        timings.binding.last_cpu_us = Some(value.into());
        assert!(!timings.is_valid(), "{value}");
    }
    for value in [-1.0, f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        let mut timings = MssqlCompilationTimings::default();
        timings.binding.average_duration_us = Some(value);
        assert!(!timings.is_valid());
        timings.binding.average_duration_us = None;
        timings.optimization.average_cpu_us = Some(value);
        assert!(!timings.is_valid());
    }
    assert!(
        MssqlCompilationTimings::default().is_valid(),
        "all unknown is a valid incomplete measurement, never a zero"
    );
}

#[test]
fn timing_values_never_change_the_query_incarnation_or_repair_a_foreign_namespace() {
    let mut row = super::super::tests::observation();
    let key = row.key().unwrap();
    row.timings.last_duration_us = Some("9007199254740993".into());
    row.timings.binding.average_cpu_us = Some(1.25);
    assert_eq!(row.key().unwrap(), key);
    let body = serde_json::to_vec(&row).unwrap();
    let mut other = row.clone();
    other.timings.binding.average_cpu_us = Some(2.5);
    assert_eq!(other.key().unwrap(), key);
    assert_ne!(
        serde_json::to_vec(&other).unwrap(),
        body,
        "immutable body authentication includes timings independently of identity"
    );
    other.identity.qualifiers.insert("context_settings_id".into(), "6".into());
    assert_ne!(other.key().unwrap(), key);
    other = row.clone();
    other.identity.qualifiers.insert("initial_compile_at".into(), "2020-01-01T06:00:01Z".into());
    assert_ne!(other.key().unwrap(), key);
    row.timings.optimization.last_duration_us = Some("-1".into());
    assert!(!row.is_valid() && row.key().is_err());
}

#[test]
fn logical_database_namespace_is_shared_without_inventing_acquisition_bounds() {
    let target = CollectionTarget::from(crate::DatabaseIdentity {
        engine: crate::Engine::Mssql,
        location: crate::DatabaseLocation::Server { host: "alias.invalid".into(), port: 1433, instance: None },
        database: "app".into(),
        incarnation: Some("native-logical-db".into()),
    });
    let expected = query_store_database_namespace(&target, &SourcePartition::Target).unwrap();
    let context = NativeReadContext {
        target: target.clone(),
        partition: SourcePartition::Target,
        observed_from: "2020-01-01T06:00:00Z".into(),
        observed_until: "2020-01-01T06:00:01Z".into(),
        database_writable: None,
    };
    assert_eq!(compilation_namespace(&context), Some(expected));
    let mut invalid = context.clone();
    invalid.observed_until = "2020-01-01T05:00:00Z".into();
    assert!(compilation_namespace(&invalid).is_none());
    let mut unknown = target.clone();
    let CollectionTarget::Database(database) = &mut unknown else { panic!("database") };
    database.incarnation = None;
    assert!(query_store_database_namespace(&unknown, &SourcePartition::Target).is_none());
    assert!(
        query_store_database_namespace(&target, &SourcePartition::InstanceLocal { member: None, epoch: None })
            .is_none()
    );
}
