use super::*;

fn read() -> StorageRead {
    super::super::tests::read()
}
fn file(read: &mut StorageRead) -> &mut MssqlDatabaseFile {
    let OperationalFact::MssqlDatabaseFile(file) = &mut read.observations[1].fact else { unreachable!() };
    file
}
fn add_file(read: &mut StorageRead, id: &str, native_type: u8, pages: &str, used: Option<&str>) {
    let mut file = file(read).clone();
    file.file_id = id.into();
    file.logical_name = format!("file-{id}");
    file.native_type = native_type.to_string();
    file.used_pages = used.map(str::to_owned);
    file.size_pages = pages.into();
    let profile = MssqlDatabaseFileProfile::from_native(native_type, 0, Some(false));
    file.kind = profile.kind;
    file.size_basis = profile.size_basis;
    read.observations.push(OperationalObservation {
        source: culprit_core::source_id::FILE_CAPACITY.into(),
        key: file.key().unwrap(),
        fact: OperationalFact::MssqlDatabaseFile(Box::new(file)),
    });
    read.sources[0].records += 1;
}
#[test]
fn disjoint_file_rows_supply_exact_type_counts_and_independent_quantities_without_adding_the_group() {
    let mut read = read();
    add_file(&mut read, "2", 0, "64", Some("1"));
    add_file(&mut read, "3", 1, "32", None);
    add_file(&mut read, "4", 2, "256", None);
    let original = read.clone();
    let summary = explain(&read).unwrap();
    assert_eq!(
        (summary.data_file_count, summary.log_file_count, summary.other_file_count),
        (Some(2), Some(1), Some(1))
    );
    assert_eq!(summary.data_allocated_bytes.as_deref(), Some("1572864"));
    assert_eq!(summary.data_used_bytes.as_deref(), Some("8192"));
    assert_eq!(summary.log_catalog_allocated_bytes.as_deref(), Some("262144"));
    assert_eq!(summary.native_database_id.as_deref(), Some("5"));
    assert_eq!(summary.observed_at.as_deref(), Some("2020-01-01T12:00:00.500000000Z"));
    assert_eq!(read, original, "interpretation retains raw originals");
    assert!(!summary.data_allocated_bytes.as_deref().unwrap().contains("9007199254740993"));
}
#[test]
fn unknown_null_snapshot_and_not_applicable_bytes_do_not_erase_known_file_counts_or_other_quantities() {
    for case in ["null-used", "unknown-basis", "snapshot", "size-not-applicable", "contradictory-used"] {
        let mut read = read();
        let file = file(&mut read);
        match case {
            "null-used" => file.used_pages = None,
            "unknown-basis" => {
                file.database_is_snapshot = None;
                file.size_basis = StorageFileSizeBasis::Unknown;
            }
            "snapshot" => {
                file.database_is_snapshot = Some(true);
                file.size_basis = StorageFileSizeBasis::SnapshotMaximum;
            }
            "size-not-applicable" => file.size_pages = "0".into(),
            _ => file.used_pages = Some("129".into()),
        }
        assert!(read.is_valid(), "{case}: raw native values stay visible");
        let summary = explain(&read).unwrap();
        assert_eq!(
            (summary.data_file_count, summary.log_file_count, summary.other_file_count),
            (Some(1), Some(0), Some(0)),
            "{case}"
        );
        assert_eq!(summary.log_catalog_allocated_bytes.as_deref(), Some("0"), "complete census has no type1 entry");
        assert_eq!(
            summary.data_allocated_bytes.as_deref(),
            matches!(case, "null-used" | "contradictory-used").then_some("1048576"),
            "{case}"
        );
        assert_eq!(
            summary.data_used_bytes.as_deref(),
            (!matches!(case, "null-used" | "contradictory-used")).then_some("0"),
            "{case}"
        );
        assert!(summary.limitations.contains(&t!("view.storage.file_summary.limit.quantities")));
    }
}
#[test]
fn partial_missing_denied_or_stopped_file_sources_do_not_promote_a_retained_prefix_to_a_total() {
    for case in ["native-partial", "missing-body", "denied", "stopped"] {
        let mut read = read();
        match case {
            "native-partial" => {
                read.sources[0] = StateSourceCoverage::observed(culprit_core::source_id::FILE_CAPACITY.into(), 2, false)
            }
            "missing-body" => {
                read.observations.remove(0);
                read.read_complete = false;
            }
            "denied" => {
                read.observations.clear();
                read.sources[0] = StateSourceCoverage::unavailable(
                    culprit_core::source_id::FILE_CAPACITY.into(),
                    culprit_core::ErrorCode::PermissionDenied,
                    None,
                );
            }
            _ => read.sources[0].stopped = Some(CollectionStop::RowLimit),
        }
        let original = read.clone();
        assert!(read.is_valid(), "{case}");
        let summary = explain(&read).unwrap();
        assert_eq!(summary.data_file_count, None, "{case}");
        assert_eq!(summary.log_file_count, None, "{case}");
        assert!(summary.observed_at.is_none() && summary.native_database_id.is_none());
        assert!(
            summary.data_allocated_bytes.is_none()
                && summary.data_used_bytes.is_none()
                && summary.log_catalog_allocated_bytes.is_none()
        );
        assert_eq!(read, original);
    }
}
#[test]
fn mixed_native_database_ids_or_census_clocks_withdraw_all_counts_while_preserving_original_rows() {
    for case in ["database-id", "file-clock", "group-clock"] {
        let mut read = read();
        add_file(&mut read, "2", 0, "64", Some("0"));
        match case {
            "database-id" => {
                let OperationalFact::MssqlDatabaseFile(file) = &mut read.observations[2].fact else { unreachable!() };
                file.native_database_id = "6".into();
            }
            "file-clock" => {
                let OperationalFact::MssqlDatabaseFile(file) = &mut read.observations[2].fact else { unreachable!() };
                file.observed_at = "2020-01-01T12:00:00.6Z".into();
            }
            _ => {
                let OperationalFact::Space(group) = &mut read.observations[0].fact else { unreachable!() };
                group.observed_at = "2020-01-01T12:00:00.6Z".into();
            }
        }
        let original = read.clone();
        assert!(read.is_valid());
        let report = super::super::explain(&read).unwrap();
        let summary = report.file_summary.unwrap();
        assert!(summary.data_file_count.is_none(), "{case}");
        assert_eq!(report.entries.len(), 3);
        assert_eq!(read, original);
    }
}
#[test]
fn missing_local_authority_is_not_repaired_by_equal_names_or_native_file_ids() {
    let mut read = read();
    read.context = ObservationReadContext::Native {
        context: Box::new(NativeReadContext {
            target: read.target.clone(),
            partition: SourcePartition::InstanceLocal { member: None, epoch: None },
            observed_from: "2020-01-01T12:00:00Z".into(),
            observed_until: "2020-01-01T12:00:01Z".into(),
            database_writable: Some(true),
        }),
    };
    assert!(read.is_valid());
    assert!(explain(&read).unwrap().data_file_count.is_none());
}
#[test]
fn a_different_utc_representation_at_the_same_exact_instant_is_not_a_second_census() {
    let mut read = read();
    add_file(&mut read, "2", 0, "64", Some("0"));
    let OperationalFact::MssqlDatabaseFile(file) = &mut read.observations[2].fact else { unreachable!() };
    file.observed_at = "2020-01-01T12:00:00.5000000Z".into();
    assert_eq!(explain(&read).unwrap().data_file_count, Some(2));
}

#[test]
fn valid_large_original_census_uses_checked_wide_page_sums_before_any_presentation_cap() {
    let mut read = read();
    file(&mut read).size_pages = "2147483647".into();
    for id in 2..=513 {
        add_file(&mut read, &id.to_string(), 0, "2147483647", Some("0"));
    }
    assert!(read.is_valid());
    let report = super::super::explain(&read).unwrap();
    assert_eq!(report.presented_rows, 256);
    assert!(report.entries_limited);
    let summary = report.file_summary.unwrap();
    assert_eq!(summary.data_file_count, Some(513));
    assert_eq!(summary.data_allocated_bytes.as_deref(), Some("9024791436582912"));
    assert_eq!(summary.data_used_bytes.as_deref(), Some("0"));
}

#[test]
fn missing_log_body_does_not_withdraw_a_separately_complete_original_file_census() {
    let mut read = read();
    add_file(&mut read, "2", 1, "32", None);
    read.sources.push(StateSourceCoverage::collected(culprit_core::source_id::LOG_CAPACITY.into(), 1));
    read.read_complete = false;
    assert!(read.is_valid());
    let report = super::super::explain(&read).unwrap();
    assert_eq!(report.source_state, SourceState::Partial);
    assert_eq!(report.native_rows, 4);
    assert_eq!(report.observed_rows, 3);
    let summary = report.file_summary.unwrap();
    assert_eq!(summary.source_state, SourceState::Collected);
    assert_eq!((summary.data_file_count, summary.log_file_count), (Some(1), Some(1)));
    assert_eq!(summary.log_catalog_allocated_bytes.as_deref(), Some("262144"));
}
