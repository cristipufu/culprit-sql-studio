use super::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TableFullTextCase {
    Enabled,
    Disabled,
    NativeNulls,
    Absent,
    Unsupported,
    Denied,
    Unknown,
    Partial,
    SourceDenied,
}
impl TableFullTextCase {
    pub const ALL: [Self; 9] = [
        Self::Enabled,
        Self::Disabled,
        Self::NativeNulls,
        Self::Absent,
        Self::Unsupported,
        Self::Denied,
        Self::Unknown,
        Self::Partial,
        Self::SourceDenied,
    ];
}

pub fn table_full_text_evidence(case: TableFullTextCase) -> (CollectionDescription, Vec<TelemetryRecord>) {
    let (mut source, _) = database_properties_evidence(DatabasePropertiesCase::Exact);
    source.source = "table-state".into();
    source.partition = SourcePartition::Target;
    let (availability, index) = match case {
        TableFullTextCase::Unsupported => (MssqlCatalogAvailability::Unsupported, None),
        TableFullTextCase::Denied => (MssqlCatalogAvailability::PermissionDenied, None),
        TableFullTextCase::Unknown => (MssqlCatalogAvailability::Unknown, None),
        TableFullTextCase::Absent | TableFullTextCase::SourceDenied => (MssqlCatalogAvailability::Available, None),
        _ => (
            MssqlCatalogAvailability::Available,
            Some(MssqlFullTextIndex {
                unique_index_id: Some(1),
                catalog_id: Some(3),
                enabled: if case == TableFullTextCase::NativeNulls {
                    None
                } else {
                    Some(case != TableFullTextCase::Disabled)
                },
                change_tracking: if case == TableFullTextCase::NativeNulls {
                    None
                } else {
                    Some(MssqlFullTextChangeTracking::Manual)
                },
                last_crawl_completed: if case == TableFullTextCase::NativeNulls { None } else { Some(true) },
            }),
        ),
    };
    let table = MssqlTableSnapshot {
        identity: MssqlObjectIdentity {
            database_id: 7,
            object_id: 100,
            created_without_offset: "2020-01-01T09:00:00.003".into(),
        },
        schema: "dbo".into(),
        name: "Documents".into(),
        modified_without_offset: "2020-01-01T09:00:00.003".into(),
        estimated_rows: None,
        reserved_bytes: None,
        data_bytes: None,
        index_bytes: None,
        in_row_bytes: None,
        lob_bytes: None,
        row_overflow_bytes: None,
        has_primary_key: Some(true),
        is_heap: Some(false),
        size_source: MssqlTableSizeSource::Unavailable,
        tracked_by_cdc: Some(false),
        full_text: MssqlTableFullText { availability, index },
        storage_configuration: MssqlTableStorageConfiguration {
            memory_optimized: Some(false),
            durability: Some(0),
            durability_desc: Some("SCHEMA_AND_DATA".into()),
        },
    };
    assert!(table.is_valid());
    let records = if case == TableFullTextCase::SourceDenied {
        vec![]
    } else {
        vec![TelemetryRecord::Operational(OperationalObservation {
            source: culprit_core::source_id::TABLES.into(),
            key: table.identity.key().unwrap(),
            fact: OperationalFact::MssqlTable(Box::new(table)),
        })]
    };
    let coverage = if case == TableFullTextCase::SourceDenied {
        StateSourceCoverage::unavailable(
            culprit_core::source_id::TABLES.into(),
            culprit_core::ErrorCode::PermissionDenied,
            None,
        )
    } else {
        StateSourceCoverage::observed(
            culprit_core::source_id::TABLES.into(),
            records.len() as u32,
            case != TableFullTextCase::Partial,
        )
    };
    let original = source.observation.snapshot().unwrap();
    source.observation = SourceObservation::Snapshot(StateSnapshotDescription {
        dataset: SnapshotDataset::Tables,
        observed_from: original.observed_from.clone(),
        observed_until: original.observed_until.clone(),
        records: records.len() as u32,
        sources: vec![coverage],
    });
    assert!(source.observation.snapshot().unwrap().is_valid());
    for record in &records {
        assert!(record.is_valid());
    }
    (source, records)
}

pub fn assert_table_metadata_capture(
    capture: &crate::analytical::evidence::EvidenceCapture,
    source: &CollectionDescription,
    records: &[TelemetryRecord],
) {
    assert_eq!(capture.source(), source);
    assert!(capture.is_read_complete());
    assert_eq!(capture.records().len(), records.len());
    for (actual, expected) in capture.records().iter().zip(records) {
        assert_eq!(&actual.row().record, expected);
    }
    assert_eq!(capture.metrics().recovery_objects, 0);
}
