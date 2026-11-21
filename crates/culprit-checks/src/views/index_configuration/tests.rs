use super::*;
use culprit_core::DatabaseIdentity;
use serde_json::json;
fn configuration() -> MssqlIndexConfiguration {
    serde_json::from_value(json!({
        "table":{"database_id":5,"object_id":100,"created_without_offset":"2026-11-10T07:00:00.123"},
        "schema":"dbo","table_name":"Orders","table_modified_without_offset":"2026-11-10T07:01:00.123",
        "index_id":2,"name":"ix_orders","type_code":2,
        "options":{"fill_factor_availability":"available","fill_factor":0,"padded_availability":"available","padded":false,
            "ignore_duplicate_key_availability":"available","ignore_duplicate_key":false,"allow_row_locks_availability":"available","allow_row_locks":true,"allow_page_locks_availability":"available","allow_page_locks":true},
        "partition_identity_availability":"available","data_compression_availability":"available","data_compression_description_availability":"available",
        "xml_compression_availability":"unsupported","xml_compression_description_availability":"unsupported","partitions_complete":true,
        "partitions":[
            {"object_id":100,"index_id":2,"partition_id":"9007199254740993","partition_number":1,"hobt_id":"9007199254740995","data_compression":1,"data_compression_description":"ROW","xml_compression":null,"xml_compression_description":null},
            {"object_id":100,"index_id":2,"partition_id":"9007199254740994","partition_number":2,"hobt_id":"9007199254740996","data_compression":2,"data_compression_description":"PAGE","xml_compression":null,"xml_compression_description":null}
        ]
    })).unwrap()
}
fn row(configuration: MssqlIndexConfiguration) -> OperationalObservation {
    let index = MssqlIndexSnapshot {
        table: configuration.table.clone(),
        schema: configuration.schema.clone(),
        table_name: configuration.table_name.clone(),
        table_modified_without_offset: configuration.table_modified_without_offset.clone(),
        index_id: configuration.index_id,
        name: configuration.name.clone().unwrap(),
        type_code: 2,
        native_type: "NONCLUSTERED".into(),
        memory_optimized: false,
        unique: false,
        primary_key: false,
        disabled: false,
        columns: vec![],
        columnstore: None,
        configuration: Some(Box::new(configuration)),
        filter: None,
        reserved_bytes: None,
        usage: MssqlIndexUsage::NotCollected,
    };
    let key = index.key().unwrap();
    let fact = OperationalFact::MssqlIndex(Box::new(index));
    OperationalObservation { source: source_id::INDEXES.into(), key, fact }
}
fn source(count: u32) -> CollectionDescription {
    CollectionDescription {
        target: DatabaseIdentity::server(Engine::Mssql, "original-database", "original-route.invalid", 1433, None)
            .into(),
        source: "index-state".into(),
        partition: SourcePartition::Target,
        continuity: SourceContinuity::Unverified,
        observation: SourceObservation::Snapshot(StateSnapshotDescription {
            dataset: SnapshotDataset::Indexes,
            observed_from: "2026-11-11T12:00:00.000000000Z".into(),
            observed_until: "2026-11-11T12:00:01.000000000Z".into(),
            records: count,
            sources: vec![StateSourceCoverage::collected(source_id::INDEXES.into(), count)],
        }),
    }
}
fn outcome(state: SourceState) -> SourceOutcome {
    SourceOutcome { source: source_id::INDEXES.into(), state, reason: None, reason_message: None }
}
fn current() -> Snapshot {
    let mut snapshot: Snapshot =
        serde_json::from_str(include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../fixtures/mssql-demo.json")))
            .unwrap();
    snapshot.indexes.truncate(1);
    let index = &mut snapshot.indexes[0];
    index.schema = "dbo".into();
    index.table = "Orders".into();
    index.name = "ix_orders".into();
    index.mssql_configuration = Some(Box::new(configuration()));
    index.mssql_columnstore = None;
    let mut heap = configuration();
    heap.index_id = 0;
    heap.type_code = 0;
    heap.name = None;
    heap.options = None;
    for partition in &mut heap.partitions {
        partition.index_id = 0;
    }
    snapshot.tables.truncate(1);
    let table = &mut snapshot.tables[0];
    table.schema = "dbo".into();
    table.name = "Orders".into();
    table.is_heap = Some(true);
    table.mssql_heap_configuration = Some(Box::new(heap));
    snapshot.sources.retain(|source| ![source_id::TABLES, source_id::INDEXES].contains(&source.source.as_str()));
    for source in [source_id::TABLES, source_id::INDEXES] {
        snapshot.sources.push(SourceOutcome {
            source: source.into(),
            state: SourceState::Collected,
            reason: None,
            reason_message: None,
        });
    }
    snapshot
}
#[test]
fn current_families_keep_independent_scope_and_no_configuration_finding() {
    let input = current();
    let indexes = explain_snapshot(&input, SnapshotDataset::Indexes).unwrap().unwrap();
    let heaps = explain_snapshot(&input, SnapshotDataset::Tables).unwrap().unwrap();
    assert_eq!(indexes.entries[0].index_id, 2);
    assert_eq!(heaps.entries[0].index_id, 0);
    assert!(indexes.context.is_none() && heaps.context.is_none());
    assert!(indexes.native_rows.is_none());
    let mut changed = input.clone();
    changed.sources.iter_mut().find(|source| source.source == source_id::TABLES).unwrap().state =
        SourceState::Unavailable;
    assert!(explain_snapshot(&changed, SnapshotDataset::Tables).is_err());
    assert!(explain_snapshot(&changed, SnapshotDataset::Indexes).unwrap().is_some());
    let mut without = input.clone();
    without.indexes[0].mssql_configuration = None;
    without.tables[0].mssql_heap_configuration = None;
    assert_eq!(
        serde_json::to_value(crate::analyze(input).findings).unwrap(),
        serde_json::to_value(crate::analyze(without).findings).unwrap()
    );
}
#[test]
fn native_values_and_mixed_partitions_keep_their_own_state_and_offset_free_time() {
    let configuration = configuration();
    let report = entry(&configuration, Some("original")).unwrap();
    assert_eq!(report.fields[0].text, t!("view.index_configuration.native_default"));
    assert_eq!(report.native_table.created_without_offset, "2026-11-10T07:00:00.123");
    assert_eq!(report.partitions[0].partition_id, "9007199254740993");
    assert_ne!(report.partitions[0].fields[0].text, report.partitions[1].fields[0].text);
    assert_eq!(report.partitions[0].fields[3].availability, MssqlCatalogAvailability::Unsupported);
    assert!(report.limitations.contains(&t!("view.index_configuration.limit.configuration")));
}
#[test]
fn known_null_denied_and_future_native_values_are_not_repaired_or_graded() {
    let mut native = configuration();
    native.options.as_mut().unwrap().fill_factor = None;
    native.partitions[0].data_compression = Some(255);
    native.partitions[0].data_compression_description = Some("FUTURE".into());
    let report = entry(&native, None).unwrap();
    assert_eq!(report.fields[0].text, t!("view.index_configuration.native_null"));
    assert!(report.partitions[0].fields[0].text.contains("255"));
    assert_eq!(report.partitions[0].fields[1].text, "FUTURE");
    native.options.as_mut().unwrap().fill_factor_availability = MssqlCatalogAvailability::PermissionDenied;
    let denied = entry(&native, None).unwrap();
    assert_eq!(denied.fields[0].availability, MssqlCatalogAvailability::PermissionDenied);
    assert_eq!(denied.fields[0].text, t!("view.index_configuration.availability", state = "denied"));
    assert_eq!(denied.fields[3].availability, MssqlCatalogAvailability::Available);
}
#[test]
fn original_scope_count_and_native_generation_admission_precede_any_configuration_text() {
    let original = row(configuration());
    let source = source(1);
    let source_outcome = outcome(SourceState::Collected);
    let report = explain(MssqlCatalogEvidence {
        description: &source,
        observations: &[&original],
        source: &source_outcome,
        read_complete: true,
    })
    .unwrap();
    assert_eq!(report.context.unwrap().target, source.target);
    assert_eq!(report.native_rows, Some(1));
    for cause in ["dataset", "engine", "source", "duplicate", "generation", "database"] {
        let mut changed_source = source.clone();
        let mut changed = original.clone();
        let mut another = original.clone();
        match cause {
            "dataset" => {
                let SourceObservation::Snapshot(header) = &mut changed_source.observation else { panic!() };
                header.dataset = SnapshotDataset::Tables;
            }
            "engine" => {
                let CollectionTarget::Database(database) = &mut changed_source.target else { panic!() };
                database.engine = Engine::Postgres;
            }
            "source" => changed.source = source_id::TABLES.into(),
            "generation" => {
                let OperationalFact::MssqlIndex(index) = &mut changed.fact else { panic!() };
                index.table.created_without_offset = "2026-11-10T07:02:00.123".into();
            }
            "database" => {
                let OperationalFact::MssqlIndex(index) = &mut another.fact else { panic!() };
                index.table.database_id = 6;
                index.configuration.as_mut().unwrap().table.database_id = 6;
                another.key = index.key().unwrap();
            }
            _ => {}
        }
        if matches!(cause, "duplicate" | "database") {
            let SourceObservation::Snapshot(header) = &mut changed_source.observation else { panic!() };
            header.records = 2;
            header.sources[0].records = 2;
        }
        let rows = if cause == "duplicate" {
            vec![&changed, &changed]
        } else if cause == "database" {
            vec![&changed, &another]
        } else {
            vec![&changed]
        };
        assert!(
            explain(MssqlCatalogEvidence {
                description: &changed_source,
                observations: &rows,
                source: &source_outcome,
                read_complete: true
            })
            .is_err(),
            "{cause}"
        );
    }
}
#[test]
fn incomplete_original_and_presentation_prefix_remain_distinct_from_complete_native_scope() {
    let original = row(configuration());
    let source = source(2);
    let source_outcome = outcome(SourceState::Partial);
    let report = explain(MssqlCatalogEvidence {
        description: &source,
        observations: &[&original],
        source: &source_outcome,
        read_complete: false,
    })
    .unwrap();
    assert_eq!(report.native_rows, Some(2));
    assert_eq!(report.observed_rows, 1);
    assert_eq!(report.read_complete, Some(false));
    let mut native = configuration();
    native.partitions = (1..=MAX_MSSQL_CONFIGURATION_PARTITIONS)
        .map(|number| {
            let mut partition = native.partitions[0].clone();
            partition.partition_id = number.to_string();
            partition.partition_number = number as u32;
            partition
        })
        .collect();
    native.partitions_complete = false;
    let entry = entry(&native, None).unwrap();
    assert_eq!(entry.retained_partitions, MAX_MSSQL_CONFIGURATION_PARTITIONS as u32);
    assert_eq!(entry.presented_partitions, MAX_PARTITIONS as u32);
    assert!(entry.partitions_limited && !entry.partitions_complete);
}
