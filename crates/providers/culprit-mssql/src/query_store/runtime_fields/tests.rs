use super::*;
use crate::{resources::test_support::replace, value::Val};
use culprit_core::telemetry::*;
fn decoded(row: &Rec) -> RuntimeMeasurement {
    let TelemetryRecord::QueryRuntime(row) =
        super::super::decode::record(row, TelemetryStream::QueryRuntime, StatementReferences::default()).unwrap()
    else {
        panic!()
    };
    assert!(row.is_valid());
    row
}
#[test]
fn every_original_statistic_family_maps_native_bigints_floats_units_and_physical_operations_independently() {
    let mut row = super::super::tests::runtime_row(1000);
    let families = [
        ("duration", Key::Duration, Unit::Microseconds),
        ("cpu_time", Key::CpuTime, Unit::Microseconds),
        ("logical_io_reads", Key::LogicalReads, Unit::Pages { bytes_per_page: 8192 }),
        ("logical_io_writes", Key::LogicalWrites, Unit::Pages { bytes_per_page: 8192 }),
        ("physical_io_reads", Key::PhysicalReads, Unit::Pages { bytes_per_page: 8192 }),
        ("clr_time", Key::Native("mssql.clr-time".into()), Unit::Microseconds),
        ("dop", Key::Parallelism, Unit::Ratio),
        ("query_max_used_memory", Key::PeakMemory, Unit::Pages { bytes_per_page: 8192 }),
        ("rowcount", Key::Rows, Unit::Count),
        ("log_bytes_used", Key::LogBytes, Unit::Bytes),
        ("tempdb_space_used", Key::TemporaryStorage, Unit::Pages { bytes_per_page: 8192 }),
        ("num_physical_io_reads", Key::PhysicalReadOperations, Unit::Count),
    ];
    for (index, (name, _, _)) in families.iter().enumerate() {
        for (stat, value) in [
            ("avg", Val::Float(index as f64 + 0.25)),
            ("last", Val::Int(9007199254740993 + index as i64)),
            ("min", Val::Int(index as i64)),
            ("max", Val::Int(i64::MAX - index as i64)),
        ] {
            row = replace(&row, &[(&format!("{stat}_{name}"), value)]);
        }
        if *name != "num_physical_io_reads" {
            row = replace(&row, &[(&format!("stdev_{name}"), Val::Float(index as f64 + 0.75))]);
        }
    }
    let original = decoded(&row);
    assert_eq!(original.metrics.len(), 12);
    for (index, (_, key, unit)) in families.iter().enumerate() {
        let metric = original.metric(key).unwrap();
        assert_eq!(metric.unit, *unit);
        assert_eq!(metric.average, Some(index as f64 + 0.25));
        assert_eq!(metric.last, Some(MeasurementValue::Integer((9007199254740993u64 + index as u64).to_string())));
        assert_eq!(metric.minimum, Some(MeasurementValue::Integer(index.to_string())));
        assert_eq!(metric.maximum, Some(MeasurementValue::Integer((i64::MAX - index as i64).to_string())));
        assert_eq!(
            metric.standard_deviation,
            if *key == Key::PhysicalReadOperations { None } else { Some(index as f64 + 0.75) }
        );
    }
    assert_eq!(original.key.native.qualifiers["execution_type"], "3");
    assert_eq!(original.key.native.qualifiers["replica_group_id"], "7");
    assert_eq!(original.key.native.qualifiers["plan_id"], "4");
    assert!(!original.key.cohort.unwrap().qualifiers.contains_key("plan_id"));
}
#[test]
fn missing_mean_retains_exact_endpoints_count_and_all_five_independent_page_server_statistics() {
    let row = replace(
        &super::super::tests::runtime_row(1000),
        &[
            ("avg_duration", Val::Null),
            ("last_duration", Val::Int(9007199254740993)),
            ("page_server_read_availability", Val::Int(1)),
            ("avg_page_server_io_reads", Val::Float(1.25)),
            ("last_page_server_io_reads", Val::Int(9007199254740993)),
            ("min_page_server_io_reads", Val::Int(0)),
            ("max_page_server_io_reads", Val::Int(i64::MAX)),
            ("stdev_page_server_io_reads", Val::Float(0.75)),
        ],
    );
    let original = decoded(&row);
    assert_eq!(original.executions, "1");
    let duration = original.metric(&Key::Duration).unwrap();
    assert_eq!(duration.average, None);
    assert_eq!(duration.last, Some(MeasurementValue::Integer("9007199254740993".into())));
    assert_eq!(duration.minimum, Some(MeasurementValue::Integer("1000".into())));
    let pages = original.page_server_reads.unwrap();
    assert_eq!(pages.average_pages, Some(1.25));
    assert_eq!(pages.last_pages.as_deref(), Some("9007199254740993"));
    assert_eq!(pages.minimum_pages.as_deref(), Some("0"));
    assert_eq!(pages.maximum_pages, Some(i64::MAX.to_string()));
    assert_eq!(pages.standard_deviation_pages, Some(0.75));
}
#[test]
fn optional_native_layouts_compile_typed_nulls_without_references_to_unsupported_columns() {
    let mut layout =
        super::super::layout::NativeLayout::decode(&super::super::window_sql::tests::layout_row()).unwrap();
    for supported in [false, true] {
        layout.log_bytes = supported;
        layout.tempdb = supported;
        let sql = super::super::catalog_sql::runtime(None, &layout);
        for family in ["log_bytes_used", "tempdb_space_used"] {
            for statistic in ["avg", "last", "min", "max", "stdev"] {
                let name = format!("{statistic}_{family}");
                assert_eq!(sql.contains(&format!("r.{name}")), supported);
                if !supported {
                    let ty = if statistic == "avg" || statistic == "stdev" { "float" } else { "bigint" };
                    assert!(sql.contains(&format!("CAST(NULL AS {ty}) AS {name}")));
                }
            }
        }
        assert!(sql.contains("r.last_duration"));
        assert!(sql.contains("r.avg_num_physical_io_reads"));
        assert!(!sql.contains("stdev_num_physical_io_reads"));
        assert!(sql.contains("r.runtime_stats_interval_id=@P2"));
        assert!(sql.contains("SELECT TOP (@P1)"));
    }
}
#[test]
fn omitted_or_malformed_native_statistics_fail_instead_of_inventing_null_or_zero() {
    for (field, value) in [
        ("last_cpu_time", Val::Float(1.5)),
        ("max_rowcount", Val::Int(-1)),
        ("stdev_dop", Val::Float(f64::INFINITY)),
        ("avg_cpu_time", Val::Float(f64::NAN)),
    ] {
        let row = replace(&super::super::tests::runtime_row(1000), &[(field, value)]);
        assert!(
            super::super::decode::record(&row, TelemetryStream::QueryRuntime, StatementReferences::default()).is_err()
        );
    }
    let row = super::super::tests::runtime_row(1000);
    let values = row
        .columns()
        .filter(|(field, _)| *field != "last_cpu_time")
        .map(|(field, value)| (field, value.clone()))
        .collect::<Vec<_>>();
    assert!(
        super::super::decode::record(&Rec::of(&values), TelemetryStream::QueryRuntime, StatementReferences::default())
            .is_err()
    );
}
