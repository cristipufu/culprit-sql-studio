use super::*;

fn blocks() -> StatementTemporaryWork {
    StatementTemporaryWork::BlockIo {
        source: "pg_stat_statements".into(),
        bytes_per_block: Some(16_384),
        read_blocks: Some("9007199254740993".into()),
        written_blocks: Some(u64::MAX.to_string()),
        read_time_ms: Some(0.0),
        write_time_ms: None,
    }
}

#[test]
fn native_counts_units_and_missing_timings_survive_round_trip_without_float_conversion() {
    let original = blocks();
    assert!(original.is_valid() && original.has_observed_work());
    let encoded = serde_json::to_string(&original).unwrap();
    assert!(encoded.contains("9007199254740993") && encoded.contains("18446744073709551615"));
    let decoded: StatementTemporaryWork = serde_json::from_str(&encoded).unwrap();
    assert_eq!(decoded, original);
}

#[test]
fn zero_and_absent_native_values_never_acquire_positive_work_or_historical_coverage() {
    let mut value = blocks();
    let StatementTemporaryWork::BlockIo { read_blocks, written_blocks, read_time_ms, bytes_per_block, .. } = &mut value
    else {
        unreachable!()
    };
    *read_blocks = Some("0".into());
    *written_blocks = None;
    *read_time_ms = Some(0.0);
    *bytes_per_block = None;
    assert!(value.is_valid());
    assert!(value.has_measurements(), "the source did report a native zero");
    assert!(!value.has_observed_work());
}

#[test]
fn source_and_unit_metadata_do_not_establish_a_measured_counter() {
    let mut blocks = StatementTemporaryWork::BlockIo {
        source: "native".into(),
        bytes_per_block: Some(8192),
        read_blocks: None,
        written_blocks: None,
        read_time_ms: None,
        write_time_ms: None,
    };
    let tables = StatementTemporaryWork::TableCreation {
        source: "native".into(),
        created_tables: None,
        disk_tables: None,
        sort_merge_passes: None,
    };
    for work in [&blocks, &tables] {
        assert!(work.is_valid(), "unknown values can be retained without becoming evidence");
        assert!(!work.has_measurements() && !work.has_observed_work());
    }
    let StatementTemporaryWork::BlockIo { bytes_per_block, .. } = &mut blocks else { unreachable!() };
    *bytes_per_block = None;
    assert!(!blocks.has_measurements());
}

#[test]
fn table_and_space_usage_facts_keep_their_own_meaning_and_units() {
    let tables = StatementTemporaryWork::TableCreation {
        source: "performance_schema.events_statements_summary_by_digest".into(),
        created_tables: Some("4".into()),
        disk_tables: Some("0".into()),
        sort_merge_passes: None,
    };
    let space_usage = StatementTemporaryWork::SpaceUsage {
        source: "sys.query_store_runtime_stats.avg_tempdb_space_used".into(),
        bytes_per_page: 8192,
        usage_pages_total: 1.5,
    };
    for value in [tables, space_usage] {
        assert!(value.is_valid() && value.has_observed_work());
        assert!(!serde_json::to_string(&value).unwrap().contains("written_blocks"));
    }
}

#[test]
fn invalid_native_values_cannot_become_observed_work() {
    for raw in ["-1", "NaN", "18446744073709551616"] {
        let mut value = blocks();
        let StatementTemporaryWork::BlockIo { written_blocks, .. } = &mut value else { unreachable!() };
        *written_blocks = Some(raw.into());
        assert!(!value.is_valid() && !value.has_observed_work());
    }
    for raw in [f64::NAN, f64::INFINITY, -0.1] {
        let mut value = blocks();
        let StatementTemporaryWork::BlockIo { write_time_ms, .. } = &mut value else { unreachable!() };
        *write_time_ms = Some(raw);
        assert!(!value.is_valid() && !value.has_observed_work());
    }
}
