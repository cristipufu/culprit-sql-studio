use super::*;
use crate::{FileIo, FileKind, FileScope};

fn log(context: FileIoContext) -> FileIo {
    FileIo {
        name: "native-log".into(),
        kind: FileKind::Log,
        context,
        database: Some("shop".into()),
        reads: 0.0,
        read_ms: Some(0.0),
        read_bytes: Some(0.0),
        writes: 1000.0,
        write_ms: Some(30000.0),
        write_bytes: Some(8192000.0),
        scope: Some(FileScope::Database),
        stats_reset: Some("2026-11-13T00:00:00Z".into()),
        write_operation: None,
    }
}

#[test]
fn native_accounting_keys_and_direction_validation_are_explicit() {
    for value in [
        IoAccounting::StorageOperation,
        IoAccounting::CachePage,
        IoAccounting::PageServerResponse,
        IoAccounting::LogServiceWrite,
        IoAccounting::Simulated,
        IoAccounting::ReplicaLogAccounting,
        IoAccounting::NonDurableLog,
        IoAccounting::Unknown,
    ] {
        assert_eq!(serde_json::to_value(value).unwrap(), value.key());
    }
    for read in [IoAccounting::LogServiceWrite, IoAccounting::Simulated, IoAccounting::NonDurableLog] {
        assert!(!FileIoContext { read, ..FileIoContext::STORAGE }.is_valid());
    }
    assert!(!FileIoContext { write: IoAccounting::PageServerResponse, ..FileIoContext::STORAGE }.is_valid());
    for id in ["", "replica\nforged", &"r".repeat(257)] {
        assert!(!FileIoContext { replica_id: Some(id.into()), ..FileIoContext::STORAGE }.is_valid());
    }
    assert!(FileIoContext { replica_id: Some("native-replica:17".into()), ..FileIoContext::STORAGE }.is_valid());
}

#[test]
fn logs_from_different_accounting_domains_or_replicas_never_merge() {
    let physical = log(FileIoContext::STORAGE);
    let contexts = [
        FileIoContext { write: IoAccounting::LogServiceWrite, ..FileIoContext::UNKNOWN },
        FileIoContext { write: IoAccounting::Simulated, ..FileIoContext::STORAGE },
        FileIoContext { replica_id: Some("replica-a".into()), ..FileIoContext::STORAGE },
        FileIoContext::UNKNOWN,
    ];
    for context in contexts {
        let other = log(context);
        let files = [physical.clone(), other.clone()];
        let merged = crate::signals::merge_logs(&files);
        assert_eq!(merged.len(), 2);
        assert_eq!(crate::signals::merged_from(&merged[0], &files), [physical.reference()]);
        assert_eq!(crate::signals::merged_from(&merged[1], &files), [other.reference()]);
        assert_ne!(physical.reference().key().unwrap(), other.reference().key().unwrap());
    }
}

#[test]
fn source_reference_identity_excludes_counters_but_keeps_native_period_and_replica() {
    let original = log(FileIoContext::STORAGE);
    let mut later = original.clone();
    later.writes = 5000.0;
    later.write_ms = Some(30001.0);
    assert_eq!(original.reference().key().unwrap(), later.reference().key().unwrap());
    later.stats_reset = None;
    assert_ne!(original.reference().key().unwrap(), later.reference().key().unwrap());
    let mut changed = serde_json::to_value(original).unwrap();
    changed.as_object_mut().unwrap().remove("context");
    assert!(serde_json::from_value::<FileIo>(changed).is_err(), "unknown old semantics are not upgraded to storage");
}

#[test]
fn file_coverage_uses_its_own_period_and_localizes_independently_of_engine() {
    let fixture = include_str!("../../../../fixtures/mssql-demo.report.json");
    let report: crate::Report = serde_json::from_str(fixture).unwrap();
    let mut file = log(FileIoContext::STORAGE);
    file.stats_reset = None;
    for engine in crate::Engine::ALL {
        let mut snapshot = report.snapshot.clone();
        snapshot.server.engine = engine;
        assert_eq!(file.coverage(&snapshot), "shop, over an unknown statistics period");
        let translated = crate::i18n::scope("fr", || file.coverage(&snapshot));
        assert_ne!(translated, file.coverage(&snapshot));
    }
}
