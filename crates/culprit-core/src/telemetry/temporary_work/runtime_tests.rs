use super::*;

fn row(executions: &str, average: Option<f64>) -> RuntimeMeasurement {
    RuntimeMeasurement {
        key: NativeMeasurementKey { native: NativeIdentity::new("native-row"), references: Default::default() },
        executions: executions.into(),
        metrics: average
            .map(|average| {
                RuntimeMetric::average(
                    RuntimeMetricKey::TemporaryStorage,
                    RuntimeMetricUnit::Pages { bytes_per_page: 16_384 },
                    average,
                )
            })
            .into_iter()
            .collect(),
        first_execution_at: None,
        last_execution_at: None,
    }
}

#[test]
fn native_space_averages_are_weighted_by_their_own_execution_counts() {
    let rows = [row("2", Some(1.25)), row("10", Some(0.5)), row("0", None)];
    let before = rows.clone();
    assert_eq!(
        runtime_temporary_space(&rows, "embedding-native-source"),
        Some(StatementTemporaryWork::SpaceUsage {
            source: "embedding-native-source".into(),
            bytes_per_page: 16_384,
            usage_pages_total: 7.5,
        })
    );
    assert_eq!(rows, before, "the raw averages, units and counts remain authoritative");
}

#[test]
fn measured_zero_survives_but_missing_or_zero_execution_observations_remain_unknown() {
    let zero = runtime_temporary_space(&[row("3", Some(0.0))], "native").unwrap();
    assert!(zero.has_measurements() && !zero.has_observed_work());
    assert!(matches!(zero, StatementTemporaryWork::SpaceUsage { usage_pages_total: 0.0, .. }));
    for rows in [vec![], vec![row("0", Some(10.0))], vec![row("3", None)]] {
        assert!(runtime_temporary_space(&rows, "native").is_none());
    }
}

#[test]
fn one_missing_contributing_fragment_withholds_the_sum_in_either_order() {
    let present = row("2", Some(1.25));
    let absent = row("10", None);
    for rows in [[present.clone(), absent.clone()], [absent, present]] {
        assert!(runtime_temporary_space(&rows, "native").is_none());
    }
}

#[test]
fn only_declared_temporary_space_pages_qualify_without_peak_or_native_name_inference() {
    for key in [RuntimeMetricKey::PeakMemory, RuntimeMetricKey::Native("mssql.avg_tempdb_space_used".into())] {
        let mut native = row("2", Some(1.25));
        native.metrics[0].key = key;
        assert!(runtime_temporary_space(&[native], "native").is_none());
    }
    for unit in [
        RuntimeMetricUnit::Bytes,
        RuntimeMetricUnit::Milliseconds,
        RuntimeMetricUnit::Pages { bytes_per_page: 1234 },
        RuntimeMetricUnit::Pages { bytes_per_page: 0 },
    ] {
        let mut native = row("2", Some(1.25));
        native.metrics[0].unit = unit;
        assert!(runtime_temporary_space(&[native], "native").is_none());
    }
    let mut incompatible = row("10", Some(0.5));
    incompatible.metrics[0].unit = RuntimeMetricUnit::Pages { bytes_per_page: 8192 };
    assert!(runtime_temporary_space(&[row("2", Some(1.25)), incompatible], "native").is_none());
}

#[test]
fn inexact_execution_counts_overflow_and_invalid_native_numbers_cannot_become_space_use() {
    for executions in ["9007199254740993", "18446744073709551615", "18446744073709551616", "-1", "1.5"] {
        assert!(runtime_temporary_space(&[row(executions, Some(1.0))], "native").is_none());
    }
    assert!(runtime_temporary_space(&[row("9007199254740992", Some(0.0))], "native").is_some());
    assert!(runtime_temporary_space(&[row("9007199254740992", Some(0.0)), row("1", Some(0.0))], "native").is_none());
    for average in [-1.0, f64::NAN, f64::INFINITY, f64::MAX] {
        assert!(runtime_temporary_space(&[row("2", Some(average))], "native").is_none());
    }
    assert!(runtime_temporary_space(&[row("1", Some(f64::MAX)), row("1", Some(f64::MAX))], "native").is_none());
}

#[test]
fn malformed_sources_and_rows_do_not_produce_a_projection() {
    let long_source = "x".repeat(513);
    for source in ["", " ", "native\nsource", long_source.as_str()] {
        assert!(runtime_temporary_space(&[row("2", Some(1.25))], source).is_none());
    }
    let mut malformed = row("2", Some(1.25));
    malformed.key.native.id.clear();
    assert!(runtime_temporary_space(&[malformed], "native").is_none());
}
