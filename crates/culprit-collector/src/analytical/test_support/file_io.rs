use super::*;
use culprit_core::{FileIoContext, FileKind, FileScope, IoAccounting, ResetMarker, SourceState};

/// Native counters and independent catalog gauges. The large queue values fit native bigint; logical catalog
/// sizes remain realistic int-page values. These points establish no delta, pressure grade or reset lifetime.
pub fn file_io_evidence_cases() -> Vec<(CollectionDescription, Vec<TelemetryRecord>)> {
    let normal = normal_files();
    [
        (normal.clone(), SourceState::Collected),
        (temporary_files(), SourceState::Collected),
        (hyperscale_files(), SourceState::Collected),
        (vec![normal[0].clone(), normal[3].clone()], SourceState::Partial),
        (vec![], SourceState::Empty),
        (vec![], SourceState::Unavailable),
    ]
    .into_iter()
    .map(|(files, state)| capture(files, state))
    .collect()
}

fn normal_files() -> Vec<FileIoCounters> {
    let mut data = file("7", "app", "1", FileKind::Data, FileScope::Database);
    data.queued_read_ms = Some("9007199254740993".into());
    data.read_ms = Some("9007199254741093".into());
    data.logical_size = Some(FileSize::Bytes { bytes: "8388608".into() });
    data.maximum_size = Some(FileSizeLimit::Bytes { bytes: "2147483648".into() });
    data.size_on_disk_bytes = Some("9437184".into());

    let mut log = file("7", "app", "2", FileKind::Log, FileScope::Database);
    log.queued_read_ms = Some("0".into());
    log.queued_write_ms = Some("9223372036854775807".into());
    log.write_ms = Some("9223372036854775807".into());
    log.logical_size = Some(FileSize::Bytes { bytes: "4194304".into() });
    log.maximum_size = Some(FileSizeLimit::NoGrowth);
    log.size_on_disk_bytes = Some("4194304".into());

    let mut container = file("7", "app", "3", FileKind::Other, FileScope::Database);
    container.native_type = Some("2".into());
    container.logical_size = Some(FileSize::NotApplicable);
    container.maximum_size = Some(FileSizeLimit::Unlimited);
    container.size_on_disk_bytes = Some("0".into());
    let mut unknown = file("7", "app", "4", FileKind::Data, FileScope::Database);
    unknown.queued_read_ms = None;
    unknown.queued_write_ms = None;
    unknown.read_ms = None;
    unknown.write_ms = None;
    unknown.reads = "0".into();
    unknown.writes = "0".into();
    unknown.logical_size = None;
    unknown.maximum_size = None;
    unknown.size_on_disk_bytes = None;
    vec![data, log, container, unknown]
}

fn temporary_files() -> Vec<FileIoCounters> {
    let mut data = file("2", "tempdb", "1", FileKind::Temp, FileScope::Server);
    data.logical_size = Some(FileSize::Bytes { bytes: "8388608".into() });
    data.maximum_size = Some(FileSizeLimit::Unlimited);
    data.size_on_disk_bytes = Some("8388608".into());
    let mut log = file("2", "tempdb", "2", FileKind::Log, FileScope::Server);
    log.context.write = IoAccounting::NonDurableLog;
    log.logical_size = Some(FileSize::Bytes { bytes: "4194304".into() });
    log.maximum_size = Some(FileSizeLimit::NoGrowth);
    log.size_on_disk_bytes = Some("4194304".into());
    vec![data, log]
}

fn hyperscale_files() -> Vec<FileIoCounters> {
    let replica = Some("01234567-89ab-cdef-0123-456789abcdef".into());
    let mut data = file("7", "app", "1", FileKind::Data, FileScope::Database);
    data.context = FileIoContext {
        read: IoAccounting::PageServerResponse,
        write: IoAccounting::Simulated,
        replica_id: replica.clone(),
    };
    data.logical_size = None;
    data.maximum_size = None;
    data.size_on_disk_bytes = Some("1073741824".into());
    let mut log = file("7", "app", "2", FileKind::Log, FileScope::Database);
    log.context = FileIoContext {
        read: IoAccounting::ReplicaLogAccounting,
        write: IoAccounting::ReplicaLogAccounting,
        replica_id: replica,
    };
    log.logical_size = None;
    log.maximum_size = None;
    log.size_on_disk_bytes = Some("0".into());
    let mut cache = file("0", "", "1", FileKind::Other, FileScope::Server);
    cache.database = None;
    cache.native_name = None;
    cache.native_type = None;
    cache.context = FileIoContext { read: IoAccounting::CachePage, write: IoAccounting::Unknown, replica_id: None };
    cache.logical_size = None;
    cache.maximum_size = None;
    cache.size_on_disk_bytes = Some("268435456".into());
    cache.queued_read_ms = None;
    cache.queued_write_ms = None;
    vec![data, log, cache]
}

fn file(database_id: &str, database: &str, file_id: &str, kind: FileKind, scope: FileScope) -> FileIoCounters {
    FileIoCounters {
        native_database_id: database_id.into(),
        database: Some(database.into()),
        file_id: file_id.into(),
        native_name: Some(format!("native_file_{database_id}_{file_id}")),
        native_type: Some(if kind == FileKind::Log { "1" } else { "0" }.into()),
        kind,
        context: FileIoContext::STORAGE,
        scope,
        reset: ResetMarker::Unknown,
        reads: "10".into(),
        read_ms: Some("100".into()),
        queued_read_ms: Some("0".into()),
        read_bytes: Some("81920".into()),
        writes: "5".into(),
        write_ms: Some("50".into()),
        queued_write_ms: Some("0".into()),
        write_bytes: Some("40960".into()),
        logical_size: None,
        maximum_size: None,
        size_on_disk_bytes: None,
        sample_uptime_ms: None,
    }
}

fn capture(files: Vec<FileIoCounters>, state: SourceState) -> (CollectionDescription, Vec<TelemetryRecord>) {
    let source_name = culprit_core::source_id::FILE_LATENCY;
    let records = files
        .into_iter()
        .map(|file| {
            assert!(file.is_valid());
            TelemetryRecord::Operational(OperationalObservation {
                source: source_name.into(),
                key: format!("{}:{}", file.native_database_id, file.file_id),
                fact: OperationalFact::FileIo(Box::new(file)),
            })
        })
        .collect::<Vec<_>>();
    let coverage = if state == SourceState::Unavailable {
        StateSourceCoverage::unavailable(source_name.into(), culprit_core::ErrorCode::PermissionDenied, None)
    } else {
        StateSourceCoverage::observed(source_name.into(), records.len() as u32, state != SourceState::Partial)
    };
    assert_eq!(coverage.state, state);
    let source = CollectionDescription {
        target: culprit_core::DatabaseIdentity::server(
            culprit_core::Engine::Mssql,
            "app",
            "fixture.invalid",
            1433,
            None,
        )
        .into(),
        partition: SourcePartition::Target,
        source: source_name.into(),
        continuity: SourceContinuity::Unverified,
        observation: SourceObservation::Snapshot(StateSnapshotDescription {
            dataset: SnapshotDataset::Operational,
            observed_from: "2020-01-01T12:00:00Z".into(),
            observed_until: "2020-01-01T12:00:01Z".into(),
            records: records.len() as u32,
            sources: vec![coverage],
        }),
    };
    assert!(source.partition_is_consistent());
    (source, records)
}
