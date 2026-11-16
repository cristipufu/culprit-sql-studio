use super::*;

fn counters() -> FileIoCounters {
    FileIoCounters {
        native_database_id: "7".into(),
        database: Some("app".into()),
        file_id: "1".into(),
        native_name: Some("app_data".into()),
        native_type: Some("0".into()),
        kind: crate::FileKind::Data,
        context: crate::FileIoContext::STORAGE,
        scope: crate::FileScope::Database,
        reset: crate::ResetMarker::Unknown,
        reads: "10".into(),
        read_ms: Some("25".into()),
        queued_read_ms: Some("9007199254740993".into()),
        read_bytes: Some("81920".into()),
        writes: "0".into(),
        write_ms: Some("0".into()),
        queued_write_ms: Some("0".into()),
        write_bytes: Some("0".into()),
        logical_size: Some(FileSize::Bytes { bytes: "9007199254740993".into() }),
        maximum_size: Some(FileSizeLimit::Unlimited),
        size_on_disk_bytes: Some("4096".into()),
        sample_uptime_ms: Some("1000".into()),
    }
}

#[test]
fn native_governance_counters_and_independent_sizes_round_trip_without_float_conversion() {
    let original = counters();
    assert!(original.is_valid());
    let serialized = serde_json::to_string(&original).unwrap();
    assert!(serialized.contains("9007199254740993"));
    assert_eq!(serde_json::from_str::<FileIoCounters>(&serialized).unwrap(), original);
    assert_eq!(original.size_on_disk_bytes.as_deref(), Some("4096"));
    assert_ne!(original.logical_size, Some(FileSize::Bytes { bytes: "4096".into() }));
}

#[test]
fn unknown_zero_not_applicable_and_limit_sentinels_are_distinct_native_facts() {
    let mut raw = counters();
    raw.logical_size = None;
    raw.maximum_size = None;
    raw.queued_read_ms = None;
    assert!(raw.is_valid());
    assert_eq!(raw.queued_write_ms.as_deref(), Some("0"));
    for size in [FileSize::Bytes { bytes: "0".into() }, FileSize::NotApplicable] {
        raw.logical_size = Some(size.clone());
        assert!(raw.is_valid());
        assert_eq!(serde_json::from_value::<FileSize>(serde_json::to_value(&size).unwrap()).unwrap(), size);
    }
    for limit in [FileSizeLimit::NoGrowth, FileSizeLimit::Unlimited, FileSizeLimit::Bytes { bytes: "8192".into() }] {
        raw.maximum_size = Some(limit);
        assert!(raw.is_valid());
    }
    assert_ne!(
        serde_json::to_value(FileSizeLimit::NoGrowth).unwrap(),
        serde_json::to_value(FileSizeLimit::Unlimited).unwrap()
    );
}

#[test]
fn malformed_or_out_of_range_integers_cannot_be_admitted_as_governance_or_file_gauges() {
    for invalid in ["", "-1", "1.5", "NaN", "18446744073709551616"] {
        for field in ["queued-read", "queued-write", "logical", "maximum"] {
            let mut raw = counters();
            match field {
                "queued-read" => raw.queued_read_ms = Some(invalid.into()),
                "queued-write" => raw.queued_write_ms = Some(invalid.into()),
                "logical" => raw.logical_size = Some(FileSize::Bytes { bytes: invalid.into() }),
                "maximum" => raw.maximum_size = Some(FileSizeLimit::Bytes { bytes: invalid.into() }),
                _ => unreachable!(),
            }
            assert!(!raw.is_valid(), "{field}: {invalid}");
        }
    }
    assert!(!FileSizeLimit::Bytes { bytes: "0".into() }.is_valid());
    assert!(FileSize::Bytes { bytes: "0".into() }.is_valid(), "a source may retain an actual measured zero");
}
