use super::*;
use culprit_core::{FileIoContext, FileScope, FileWriteOperation};

fn file(name: &str, kind: FileKind) -> FileIo {
    FileIo {
        name: name.into(),
        kind,
        context: FileIoContext::STORAGE,
        database: Some("shop".into()),
        reads: 2_000.0,
        read_ms: Some(2_000.0),
        read_bytes: None,
        writes: 2_000.0,
        write_ms: Some(4_000.0),
        write_bytes: None,
        scope: Some(FileScope::Database),
        stats_reset: Some("2026-11-12T10:00:00Z".into()),
        write_operation: Some(FileWriteOperation::Write),
    }
}

fn report(files: Vec<FileIo>) -> Report {
    let mut snapshot = crate::waits::snapshot_with(Engine::Mssql);
    snapshot.server.database = "shop".into();
    snapshot.pressure.get_or_insert_default().files = files;
    let mut report = crate::analyze(snapshot);
    report.findings.clear();
    report
}

#[test]
fn coherent_physical_read_and_log_populations_keep_their_native_operation_labels() {
    let data = file("data", FileKind::Data);
    let mut log = file("log", FileKind::Log);
    log.write_operation = Some(FileWriteOperation::Flush);
    let tile = storage(&report(vec![data, log]));
    assert_eq!(tile.status, TileStatus::Info);
    assert_eq!(tile.metrics.len(), 2);
    assert_eq!(tile.metrics[0].value, Some(1.0));
    assert_eq!(tile.metrics[1].value, Some(2.0));
    assert_eq!(tile.metrics[1].label, t!("view.dashboard.storage.metric.log_flush"));
}

#[test]
fn unlike_replica_accounting_reset_scope_and_file_kind_do_not_form_one_dashboard_average() {
    let first = file("native-a", FileKind::Data);
    for changed in ["replica", "accounting", "reset", "scope", "kind"] {
        let mut second = file("native-b", FileKind::Data);
        match changed {
            "replica" => second.context.replica_id = Some("replica-b".into()),
            "accounting" => second.context.read = IoAccounting::CachePage,
            "reset" => second.stats_reset = None,
            "scope" => second.scope = Some(FileScope::Server),
            "kind" => second.kind = FileKind::Temp,
            _ => unreachable!(),
        }
        assert!(coherent_latency(&[first.clone(), second.clone()], false).is_none(), "{changed}");
        let tile = storage(&report(vec![first.clone(), second]));
        assert!(tile.metrics.is_empty(), "{changed}");
        assert_eq!(tile.items.len(), 2, "native observations remain visible: {changed}");
        assert_ne!(tile.items[0].id, tile.items[1].id);
        assert_eq!(tile.status, TileStatus::Info);
    }
}

#[test]
fn one_log_flush_does_not_relabel_other_log_writes_as_flushes() {
    let first = file("log-a", FileKind::Log);
    let mut second = file("log-b", FileKind::Log);
    second.write_operation = Some(FileWriteOperation::Flush);
    let tile = storage(&report(vec![first, second]));
    assert!(tile.metrics.is_empty());
    assert_eq!(tile.items.len(), 2);
    assert!(tile.items.iter().any(|item| item.value.as_deref().is_some_and(|value| value.contains("per flush"))));
    assert!(tile.items.iter().any(|item| item.value.as_deref().is_some_and(|value| value.contains("per write"))));
    assert_eq!(tile.takeaway, t!("view.dashboard.storage.mixed"));
}

#[test]
fn non_storage_timings_are_visible_with_native_accounting_and_without_disk_speed_metrics() {
    for (kind, read, write) in [
        (FileKind::Data, IoAccounting::CachePage, IoAccounting::Unknown),
        (FileKind::Other, IoAccounting::CachePage, IoAccounting::Unknown),
        (FileKind::Data, IoAccounting::PageServerResponse, IoAccounting::Simulated),
        (FileKind::Log, IoAccounting::Unknown, IoAccounting::LogServiceWrite),
        (FileKind::Log, IoAccounting::Unknown, IoAccounting::ReplicaLogAccounting),
        (FileKind::Log, IoAccounting::Unknown, IoAccounting::NonDurableLog),
        (FileKind::Data, IoAccounting::Unknown, IoAccounting::Unknown),
    ] {
        let mut row = file("native", kind);
        row.context = FileIoContext { read, write, replica_id: Some("native-replica".into()) };
        let tile = storage(&report(vec![row]));
        assert_eq!(tile.takeaway, t!("view.dashboard.storage.native"));
        assert!(tile.metrics.is_empty());
        assert_eq!(tile.items.len(), 1);
        assert!(tile.items[0].value.as_deref().unwrap().contains("Native average"));
        assert!(tile.detail.as_deref().unwrap().contains("native-replica"));
        assert_eq!(tile.items[0].status, Some(TileStatus::Info));
    }
}

#[test]
fn coherent_quiet_log_files_use_native_total_time_and_zero_operation_rows_do_not_hide_their_average() {
    let mut first = file("redo-a", FileKind::Log);
    first.writes = 500.0;
    first.write_ms = Some(1_000.0);
    let mut second = first.clone();
    second.name = "redo-b".into();
    second.writes = 1_500.0;
    second.write_ms = Some(6_000.0);
    let mut idle = first.clone();
    idle.name = "idle-log".into();
    idle.writes = 0.0;
    idle.write_ms = None;
    let tile = storage(&report(vec![first, second, idle]));
    assert_eq!(tile.metrics.len(), 1);
    assert_eq!(tile.metrics[0].value, Some(3.5));
    assert_eq!(tile.metrics[0].label, t!("view.dashboard.storage.metric.log_write"));
    assert_eq!(tile.items.len(), 2, "raw positive native files retain their identities");
}
