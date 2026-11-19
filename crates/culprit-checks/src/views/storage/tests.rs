use super::*;
pub(super) fn read() -> StorageRead {
    let fixture: serde_json::Value =
        serde_json::from_str(include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../fixtures/storage.native.json")))
            .unwrap();
    let space = serde_json::from_value(fixture["space"].clone()).unwrap();
    let file: MssqlDatabaseFile = serde_json::from_value(fixture["file"].clone()).unwrap();
    StorageRead {
        target: culprit_core::DatabaseIdentity::server(
            culprit_core::Engine::Mssql,
            "sample",
            "fixture.invalid",
            1433,
            None,
        )
        .into(),
        context: ObservationReadContext::Collector {
            observed_from: "2020-01-01T12:00:00Z".into(),
            observed_until: "2020-01-01T12:00:01Z".into(),
        },
        sources: vec![StateSourceCoverage::collected(culprit_core::source_id::FILE_CAPACITY.into(), 2)],
        observations: vec![
            OperationalObservation {
                source: culprit_core::source_id::FILE_CAPACITY.into(),
                key: "filegroup:1".into(),
                fact: OperationalFact::Space(space),
            },
            OperationalObservation {
                source: culprit_core::source_id::FILE_CAPACITY.into(),
                key: file.key().unwrap(),
                fact: OperationalFact::MssqlDatabaseFile(Box::new(file)),
            },
        ],
        read_complete: true,
    }
}
#[test]
fn exact_quantities_zero_unknown_and_native_size_populations_are_not_summed_or_graded() {
    let mut read = read();
    let OperationalFact::MssqlDatabaseFile(file) = &mut read.observations[1].fact else { panic!() };
    file.used_pages = None;
    file.native_type = "2".into();
    file.kind = culprit_core::FileKind::Other;
    file.type_description = Some("FILESTREAM".into());
    file.size_basis = StorageFileSizeBasis::ContainerUsed;
    let report = explain(&read).unwrap();
    assert_eq!(report.entries.len(), 2);
    assert_eq!(
        report.entries[0].fields.iter().find(|f| f.key == "total_bytes").unwrap().value.as_deref(),
        Some("9007199254740993")
    );
    assert_eq!(report.entries[0].fields.iter().find(|f| f.key == "used_bytes").unwrap().value.as_deref(), Some("0"));
    assert!(report.entries[1].fields.iter().find(|f| f.key == "used_pages").unwrap().value.is_none());
    assert_eq!(report.entries[1].explanation, t!("view.storage.basis.container"));
    assert!(serde_json::to_value(report).unwrap().get("severity").is_none());
}
#[test]
fn saved_storage_reanalysis_is_identical_and_sharing_withholds_private_originals_with_an_explicit_gap() {
    let mut snapshot: Snapshot =
        serde_json::from_str(include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../fixtures/mssql-demo.json")))
            .unwrap();
    let mut read = read();
    let OperationalFact::MssqlDatabaseFile(file) = &mut read.observations[1].fact else { panic!() };
    file.file_guid = Some("private-native-file-guid-canary".into());
    snapshot.operations.storage = Some(Box::new(read));
    let original = explain_snapshot(&snapshot).unwrap().unwrap();
    let restored: Snapshot = serde_json::from_slice(&serde_json::to_vec(&snapshot).unwrap()).unwrap();
    let replay = explain_snapshot(&restored).unwrap().unwrap();
    assert_eq!(serde_json::to_value(original).unwrap(), serde_json::to_value(replay).unwrap());
    let shared = culprit_core::redact::redact_snapshot(&snapshot);
    assert!(shared.operations.storage.is_none());
    assert!(!serde_json::to_string(&shared).unwrap().contains("private-native-file-guid-canary"));
    assert!(
        shared.sources.iter().any(|source| source.source == culprit_core::source_id::FILE_CAPACITY
            && source.state == SourceState::Unavailable)
    );
}
#[test]
fn original_native_count_partial_bodies_and_oversized_declared_counts_remain_distinct() {
    let mut read = read();
    read.observations.remove(0);
    read.read_complete = false;
    let report = explain(&read).unwrap();
    assert_eq!(report.native_rows, 2);
    assert_eq!(report.observed_rows, 1);
    assert_eq!(report.source_state, SourceState::Partial);
    assert_eq!(report.native_sources[0].records, 2);
    read.sources[0].records = u32::MAX;
    assert!(!read.is_valid());
    assert!(explain(&read).is_err());
}

#[test]
fn a_saved_file_cannot_advertise_a_size_meaning_or_usage_its_native_fields_do_not_support() {
    for change in ["filestream", "snapshot", "unavailable_snapshot", "offline_usage"] {
        let mut read = read();
        let OperationalFact::MssqlDatabaseFile(file) = &mut read.observations[1].fact else { panic!() };
        match change {
            "filestream" => {
                file.native_type = "2".into();
                file.kind = culprit_core::FileKind::Other;
                file.used_pages = None;
            }
            "snapshot" => file.database_is_snapshot = Some(true),
            "unavailable_snapshot" => {
                file.database_is_snapshot = None;
                file.is_sparse = Some(true);
            }
            _ => file.native_state = "6".into(),
        }
        let saved = serde_json::to_vec(&read).unwrap();
        let restored: StorageRead = serde_json::from_slice(&saved).unwrap();
        assert!(explain(&restored).is_err(), "{change}");
    }
}
