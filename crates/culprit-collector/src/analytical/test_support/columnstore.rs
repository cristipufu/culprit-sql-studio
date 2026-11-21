//! One persisted index-configuration corpus for SDK and both offline destinations, with no native reads.
use super::*;
use culprit_core::Engine;

pub fn columnstore_configuration_evidence_cases() -> Vec<(CollectionDescription, Vec<TelemetryRecord>)> {
    [
        (MssqlCatalogAvailability::Available, None),
        (MssqlCatalogAvailability::Available, Some(0)),
        (MssqlCatalogAvailability::Available, Some(15)),
        (MssqlCatalogAvailability::Unsupported, None),
        (MssqlCatalogAvailability::PermissionDenied, None),
        (MssqlCatalogAvailability::Unknown, None),
    ]
    .into_iter()
    .map(|(availability, minutes)| {
        let table = MssqlObjectIdentity {
            database_id: 7,
            object_id: 100,
            created_without_offset: "2026-11-10T07:00:00.123".into(),
        };
        let columns = (1..=2)
            .map(|id| MssqlCatalogIndexColumn {
                index_column_id: id,
                column_id: id,
                name: format!("column_{id}"),
                key_ordinal: 0,
                descending: false,
                included: true,
            })
            .collect::<Vec<_>>();
        let configuration = MssqlColumnstoreConfiguration {
            table: table.clone(),
            index_id: 1,
            schema: "dbo".into(),
            table_name: "events".into(),
            name: "cci_events".into(),
            table_modified_without_offset: "2026-11-10T07:01:00.123".into(),
            compression_delay_availability: availability,
            compression_delay_minutes: minutes,
            column_store_order_availability: availability,
            data_clustering_availability: MssqlCatalogAvailability::Unsupported,
            columns: columns
                .iter()
                .map(|column| MssqlColumnstoreColumn {
                    index_column_id: column.index_column_id,
                    column_id: column.column_id,
                    name: column.name.clone(),
                    column_store_order_ordinal: (availability == MssqlCatalogAvailability::Available)
                        .then_some(if column.column_id == 1 { 1 } else { 0 }),
                    data_clustering_ordinal: None,
                })
                .collect(),
        };
        let index = MssqlIndexSnapshot {
            parent_kind: MssqlIndexParentKind::Table,
            parent_object: table,
            parent_schema: "dbo".into(),
            parent_name: "events".into(),
            index_id: 1,
            name: "cci_events".into(),
            parent_modified_without_offset: configuration.table_modified_without_offset.clone(),
            type_code: 5,
            native_type: "CLUSTERED COLUMNSTORE".into(),
            parent_memory_optimized: Some(false),
            unique: false,
            primary_key: false,
            disabled: false,
            columns,
            configuration: None,
            columnstore: Some(Box::new(configuration)),
            filter: None,
            reserved_bytes: None,
            usage: MssqlIndexUsage::NotCollected,
        };
        assert!(index.is_valid());
        let record = TelemetryRecord::Operational(OperationalObservation {
            source: culprit_core::source_id::INDEXES.into(),
            key: index.key().unwrap(),
            fact: OperationalFact::MssqlIndex(Box::new(index)),
        });
        let source = CollectionDescription {
            partition: SourcePartition::Target,
            target: culprit_core::DatabaseIdentity::server(Engine::Mssql, "db", "fixture.invalid", 1433, None).into(),
            source: "index-state".into(),
            continuity: SourceContinuity::Unverified,
            observation: SourceObservation::Snapshot(StateSnapshotDescription {
                dataset: SnapshotDataset::Indexes,
                observed_from: "2026-11-20T12:00:00Z".into(),
                observed_until: "2026-11-20T12:00:01Z".into(),
                records: 1,
                sources: vec![StateSourceCoverage::collected(culprit_core::source_id::INDEXES.into(), 1)],
            }),
        };
        (source, vec![record])
    })
    .collect()
}
