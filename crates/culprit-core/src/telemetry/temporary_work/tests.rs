use super::*;

fn metric(key: RuntimeMetricKey, unit: RuntimeMetricUnit, value: &str) -> StatementCounter {
    StatementCounter { key, unit, total: MeasurementValue::Integer(value.into()) }
}

fn sample(totals: Vec<StatementCounter>) -> StatementCounterSample {
    StatementCounterSample {
        identity: NativeQueryIdentity { query_id: "native-query".into(), qualifiers: Default::default() },
        scope: crate::MeasurementScope::Database,
        observed_at: "2020-01-01T12:00:00.123456789Z".into(),
        reset: crate::ResetMarker::Unknown,
        totals,
        distributions: vec![],
    }
}

#[test]
fn mixed_native_families_preserve_exact_counts_without_using_an_engine_or_native_metric_name() {
    let original = sample(vec![
        metric(RuntimeMetricKey::TemporaryTables, RuntimeMetricUnit::Count, "18446744073709551615"),
        metric(RuntimeMetricKey::TemporaryDiskTables, RuntimeMetricUnit::Count, "0"),
        metric(RuntimeMetricKey::SortMergePasses, RuntimeMetricUnit::Count, "9007199254740993"),
        metric(RuntimeMetricKey::TemporaryBlocksRead, RuntimeMetricUnit::Pages { bytes_per_page: 8192 }, "0"),
        metric(
            RuntimeMetricKey::TemporaryBlocksWritten,
            RuntimeMetricUnit::Pages { bytes_per_page: 8192 },
            "9007199254740993",
        ),
    ]);
    let work = statement_temporary_work(&original, "embedding-native-source").unwrap();
    assert_eq!(
        work,
        vec![
            StatementTemporaryWork::BlockIo {
                source: "embedding-native-source".into(),
                bytes_per_block: Some(8192),
                read_blocks: Some("0".into()),
                written_blocks: Some("9007199254740993".into()),
                read_time_ms: None,
                write_time_ms: None
            },
            StatementTemporaryWork::TableCreation {
                source: "embedding-native-source".into(),
                created_tables: Some("18446744073709551615".into()),
                disk_tables: Some("0".into()),
                sort_merge_passes: Some("9007199254740993".into())
            },
        ]
    );
    assert_eq!(original.reset, crate::ResetMarker::Unknown);
    assert_eq!(original.observed_at, "2020-01-01T12:00:00.123456789Z");
}

#[test]
fn zero_is_retained_while_missing_metrics_and_opaque_native_lookalikes_remain_unknown() {
    let zero = sample(vec![metric(RuntimeMetricKey::TemporaryDiskTables, RuntimeMetricUnit::Count, "0")]);
    let work = statement_temporary_work(&zero, "native").unwrap();
    assert_eq!(
        work,
        vec![StatementTemporaryWork::TableCreation {
            source: "native".into(),
            created_tables: None,
            disk_tables: Some("0".into()),
            sort_merge_passes: None
        }]
    );
    assert!(!work[0].has_observed_work());
    let opaque = sample(vec![metric(
        RuntimeMetricKey::Native("mysql.temporary-disk-tables".into()),
        RuntimeMetricUnit::Count,
        "99",
    )]);
    assert!(statement_temporary_work(&opaque, "native").unwrap().is_empty());
}

#[test]
fn time_only_native_observations_keep_unknown_block_counts_and_convert_declared_units_once() {
    for (unit, native) in [
        (RuntimeMetricUnit::Picoseconds, 1_500_000_000.0),
        (RuntimeMetricUnit::Nanoseconds, 1_500_000.0),
        (RuntimeMetricUnit::Microseconds, 1500.0),
        (RuntimeMetricUnit::Milliseconds, 1.5),
        (RuntimeMetricUnit::Seconds, 0.0015),
    ] {
        let row = sample(vec![StatementCounter {
            key: RuntimeMetricKey::TemporaryReadTime,
            unit,
            total: MeasurementValue::Real(native),
        }]);
        assert_eq!(
            statement_temporary_work(&row, "native").unwrap(),
            vec![StatementTemporaryWork::BlockIo {
                source: "native".into(),
                bytes_per_block: None,
                read_blocks: None,
                written_blocks: None,
                read_time_ms: Some(1.5),
                write_time_ms: None,
            }]
        );
    }
}

#[test]
fn incompatible_page_sizes_units_duplicate_metrics_and_fractional_counts_are_rejected() {
    let cases = [
        vec![
            metric(RuntimeMetricKey::TemporaryBlocksRead, RuntimeMetricUnit::Pages { bytes_per_page: 8192 }, "1"),
            metric(RuntimeMetricKey::TemporaryBlocksWritten, RuntimeMetricUnit::Pages { bytes_per_page: 4096 }, "1"),
        ],
        vec![metric(RuntimeMetricKey::TemporaryBlocksRead, RuntimeMetricUnit::Count, "1")],
        vec![metric(RuntimeMetricKey::TemporaryDiskTables, RuntimeMetricUnit::Bytes, "1")],
        vec![
            metric(RuntimeMetricKey::TemporaryTables, RuntimeMetricUnit::Count, "1"),
            metric(RuntimeMetricKey::TemporaryTables, RuntimeMetricUnit::Count, "2"),
        ],
        vec![StatementCounter {
            key: RuntimeMetricKey::TemporaryTables,
            unit: RuntimeMetricUnit::Count,
            total: MeasurementValue::Real(1.0),
        }],
        vec![metric(RuntimeMetricKey::TemporaryBlocksWritten, RuntimeMetricUnit::Pages { bytes_per_page: 1234 }, "1")],
    ];
    for totals in cases {
        assert!(statement_temporary_work(&sample(totals), "native").is_err());
    }
}

#[test]
fn malformed_identity_clock_reset_values_and_unbounded_time_conversion_do_not_yield_work() {
    let good = sample(vec![metric(RuntimeMetricKey::TemporaryTables, RuntimeMetricUnit::Count, "1")]);
    for source in ["", " ", "native\nsource"] {
        assert!(statement_temporary_work(&good, source).is_err());
    }
    let mut bad = good.clone();
    bad.identity.query_id.clear();
    assert!(statement_temporary_work(&bad, "native").is_err());
    let mut bad = good.clone();
    bad.reset = crate::ResetMarker::At("not-a-time".into());
    assert!(statement_temporary_work(&bad, "native").is_err());
    let mut bad = good;
    bad.observed_at = "not-a-time".into();
    assert!(statement_temporary_work(&bad, "native").is_err());
    for value in [-1.0, f64::NAN, f64::INFINITY, f64::MAX] {
        let bad = sample(vec![StatementCounter {
            key: RuntimeMetricKey::TemporaryWriteTime,
            unit: RuntimeMetricUnit::Seconds,
            total: MeasurementValue::Real(value),
        }]);
        assert!(statement_temporary_work(&bad, "native").is_err());
    }
}
