use super::*;

fn blocks() -> StatementTemporaryWork {
    StatementTemporaryWork::BlockIo {
        source: "native-query-statistics".into(),
        bytes_per_block: Some(16_384),
        read_blocks: Some("0".into()),
        written_blocks: Some("18446744073709551615".into()),
        read_time_ms: Some(0.0),
        write_time_ms: Some(100_000.25),
    }
}

#[test]
fn native_counts_and_parallel_resource_time_remain_unclamped_independent_measurements() {
    let work = blocks();
    let report = explain(evidence(&work).unwrap());
    let read = report.metrics.iter().find(|metric| metric.is(key!("view.temporary_work.read_blocks"))).unwrap();
    assert_eq!(read.text.as_deref(), Some("0"));
    let writes = report.metrics.iter().find(|metric| metric.is(key!("view.temporary_work.written_blocks"))).unwrap();
    assert_eq!(writes.text.as_deref(), Some("18446744073709551615"));
    let write_time = report.metrics.iter().find(|metric| metric.is(key!("view.temporary_work.write_time"))).unwrap();
    assert_eq!(write_time.value, 100_000.25);
    assert_eq!(write_time.unit, Unit::Ms);
    assert_eq!(report.limitations[0], t!("view.temporary_work.blocks.limit"));
}

#[test]
fn table_counts_and_native_zero_do_not_turn_into_byte_time_or_memory_requirements() {
    let work = StatementTemporaryWork::TableCreation {
        source: "native-digest".into(),
        created_tables: Some("4".into()),
        disk_tables: Some("0".into()),
        sort_merge_passes: Some("2".into()),
    };
    let report = explain(evidence(&work).unwrap());
    assert_eq!(report.metrics.len(), 3);
    assert!(report.metrics.iter().all(|metric| metric.unit == Unit::Text));
    assert_eq!(
        report
            .metrics
            .iter()
            .find(|metric| metric.is(key!("view.temporary_work.disk_tables")))
            .unwrap()
            .text
            .as_deref(),
        Some("0")
    );
    assert_eq!(report.investigation, t!("view.temporary_work.investigation"));
    assert_eq!(report.limitations[0], t!("view.temporary_work.tables.limit"));
}

#[test]
fn weighted_space_observations_never_become_writes_or_a_simultaneous_peak() {
    let work = StatementTemporaryWork::SpaceUsage {
        source: "native-runtime-space-use".into(),
        bytes_per_page: 8192,
        usage_pages_total: 1.5,
    };
    let report = explain(evidence(&work).unwrap());
    assert_eq!(report.metrics[0].value, 1.5);
    assert_eq!(report.metrics[0].unit, Unit::Pages);
    assert!(report.metrics.iter().all(|metric| !metric.is(key!("view.temporary_work.written_blocks"))));
    assert_eq!(report.summary, t!("view.temporary_work.space.summary"));
}

#[test]
fn invalid_native_facts_cannot_be_worded_and_absent_units_or_metrics_stay_absent() {
    let mut work = blocks();
    let StatementTemporaryWork::BlockIo { bytes_per_block, read_blocks, read_time_ms, write_time_ms, .. } = &mut work
    else {
        unreachable!()
    };
    *bytes_per_block = None;
    *read_blocks = None;
    *read_time_ms = None;
    *write_time_ms = None;
    let report = explain(evidence(&work).unwrap());
    assert_eq!(report.metrics.len(), 1);
    let StatementTemporaryWork::BlockIo { written_blocks, .. } = &mut work else { unreachable!() };
    *written_blocks = Some("-1".into());
    assert!(evidence(&work).is_none());
}

#[test]
fn native_explanations_localize_without_interpreting_the_source_identifier() {
    let work = blocks();
    for language in ["en", "es", "fr", "de"] {
        culprit_core::i18n::scope(language, || {
            let report = explain(evidence(&work).unwrap());
            assert_eq!(report.title, t!("view.temporary_work.blocks.title"));
            assert_eq!(report.metrics[1].text.as_deref(), Some("18446744073709551615"));
            assert_eq!(report.verify, t!("view.temporary_work.verify"));
        });
    }
}

#[test]
fn metadata_only_unknown_objects_are_not_worded_as_recorded_temporary_measurements() {
    let mut work = StatementTemporaryWork::BlockIo {
        source: "native".into(),
        bytes_per_block: Some(8192),
        read_blocks: None,
        written_blocks: None,
        read_time_ms: None,
        write_time_ms: None,
    };
    assert!(evidence(&work).is_none());
    let StatementTemporaryWork::BlockIo { written_blocks, .. } = &mut work else { unreachable!() };
    *written_blocks = Some("0".into());
    let report = explain(evidence(&work).unwrap());
    assert_eq!(report.metrics[0].text.as_deref(), Some("0"));
    let tables = StatementTemporaryWork::TableCreation {
        source: "native".into(),
        created_tables: None,
        disk_tables: None,
        sort_merge_passes: None,
    };
    assert!(evidence(&tables).is_none());
}
