use super::*;
use culprit_core::{CheckEvidence, FileIoContext, FileWriteOperation, IoAccounting, SourceOutcome, SourceState};

fn file(name: &str) -> FileIo {
    FileIo {
        name: name.into(),
        kind: FileKind::Data,
        context: FileIoContext::STORAGE,
        database: Some("shop".into()),
        reads: 4_000.0,
        read_ms: Some(120_000.0),
        read_bytes: Some(4_000.0 * 8_192.0),
        writes: 4_000.0,
        write_ms: Some(120_000.0),
        write_bytes: Some(4_000.0 * 8_192.0),
        scope: Some(FileScope::Database),
        stats_reset: Some("2026-11-12T10:00:00Z".into()),
        write_operation: Some(FileWriteOperation::Write),
    }
}

fn snapshot(files: Vec<FileIo>) -> Snapshot {
    let mut snapshot = crate::waits::snapshot_with(Engine::Mssql);
    snapshot.server.database = "shop".into();
    snapshot.taken_at = "2026-11-13T10:00:00Z".into();
    snapshot.pressure.get_or_insert_default().files = files;
    snapshot.sources.retain(|source| !culprit_core::same_source(&source.source, culprit_core::source_id::FILE_LATENCY));
    snapshot.sources.push(SourceOutcome {
        source: culprit_core::source_id::FILE_LATENCY.into(),
        state: SourceState::Collected,
        reason: None,
        reason_message: None,
    });
    snapshot
}

#[test]
fn physical_storage_advice_requires_native_operation_meaning_on_every_engine() {
    for engine in Engine::ALL {
        for accounting in [
            IoAccounting::StorageOperation,
            IoAccounting::CachePage,
            IoAccounting::PageServerResponse,
            IoAccounting::ReplicaLogAccounting,
            IoAccounting::Unknown,
        ] {
            let mut row = file("native-data");
            row.context.read = accounting;
            let mut snapshot = snapshot(vec![row.clone()]);
            snapshot.server.engine = engine;
            let cases = slow_files(&snapshot);
            assert_eq!(cases.len(), usize::from(accounting == IoAccounting::StorageOperation));
            assert_eq!(snapshot.pressure.as_ref().unwrap().files, [row]);
        }
        for accounting in [
            IoAccounting::StorageOperation,
            IoAccounting::CachePage,
            IoAccounting::LogServiceWrite,
            IoAccounting::Simulated,
            IoAccounting::ReplicaLogAccounting,
            IoAccounting::NonDurableLog,
            IoAccounting::Unknown,
        ] {
            let mut row = file("native-log");
            row.kind = FileKind::Log;
            row.context.write = accounting;
            let mut snapshot = snapshot(vec![row]);
            snapshot.server.engine = engine;
            assert_eq!(slow_files(&snapshot).len(), usize::from(accounting == IoAccounting::StorageOperation));
        }
    }
}

#[test]
fn unlike_replica_scope_reset_and_accounting_counters_never_cross_the_operation_floor_together() {
    let mut first = file("data-a");
    first.reads = 500.0;
    first.read_ms = Some(15_000.0);
    let mut second = first.clone();
    second.name = "data-b".into();
    assert_eq!(slow_groups(&[first.clone(), second.clone()]).len(), 1);
    for change in ["replica", "scope", "reset", "write-accounting", "write-operation"] {
        let mut changed = second.clone();
        match change {
            "replica" => changed.context.replica_id = Some("native-reader-replica".into()),
            "scope" => changed.scope = Some(FileScope::Server),
            "reset" => changed.stats_reset = None,
            "write-accounting" => changed.context.write = IoAccounting::Unknown,
            "write-operation" => changed.write_operation = Some(FileWriteOperation::Flush),
            _ => unreachable!(),
        }
        assert!(slow_groups(&[first.clone(), changed]).is_empty(), "{change}");
    }
}

#[test]
fn file_rate_uses_its_own_explicit_reset_and_scope_instead_of_the_database_counter_clock() {
    let mut snapshot = snapshot(vec![file("data")]);
    assert_eq!(slow_files(&snapshot)[0].severity, Severity::Medium);
    snapshot.pressure.as_mut().unwrap().files[0].stats_reset = None;
    assert_eq!(slow_files(&snapshot)[0].severity, Severity::Low);
    snapshot.pressure.as_mut().unwrap().files[0].stats_reset = Some("2026-11-12T10:00:00Z".into());
    snapshot.pressure.as_mut().unwrap().files[0].scope = Some(FileScope::Server);
    assert_eq!(slow_files(&snapshot)[0].severity, Severity::Low);
    snapshot.pressure.as_mut().unwrap().files[0].scope = None;
    assert_eq!(slow_files(&snapshot)[0].severity, Severity::Low);
}

#[test]
fn native_context_defines_finding_identity_and_references_without_translated_group_names() {
    let mut first = file("private-native-file");
    first.kind = FileKind::Log;
    let mut second = first.clone();
    second.context.replica_id = Some("replica-b".into());
    let snapshot = snapshot(vec![first.clone(), second.clone()]);
    let cases = slow_files(&snapshot);
    assert_eq!(cases.len(), 2);
    assert_ne!(cases[0].key, cases[1].key);
    let keys = cases.iter().map(|case| case.key.clone()).collect::<Vec<_>>();
    let findings = cases.into_iter().map(|case| slow_storage_finding(&snapshot, case)).collect::<Vec<_>>();
    for (finding, native) in findings.iter().zip([first, second]) {
        assert_eq!(finding.facts.storage_files, [native.reference()]);
        assert!(!finding.id.contains("private-native-file"));
        assert!(!finding.summary.contains("every commit"));
    }
    let mut later = snapshot.clone();
    later.pressure.as_mut().unwrap().files.iter_mut().for_each(|row| {
        row.writes *= 2.0;
        row.write_ms = row.write_ms.map(|value| value * 2.0);
    });
    assert_eq!(slow_files(&later).iter().map(|case| case.key.clone()).collect::<Vec<_>>(), keys);
}

#[test]
fn invalid_native_values_and_aggregate_overflow_do_not_authorize_storage_advice() {
    for (reads, time) in [(f64::INFINITY, 30_000.0), (1_000.0, f64::NAN), (1_000.0, -1.0)] {
        let mut row = file("invalid");
        row.reads = reads;
        row.read_ms = Some(time);
        assert!(slow_groups(&[row]).is_empty());
    }
    let mut first = file("overflow-a");
    first.reads = f64::MAX;
    first.read_ms = Some(f64::MAX);
    let mut second = first.clone();
    second.name = "overflow-b".into();
    assert!(slow_groups(&[first.clone(), second.clone()]).is_empty());
    assert!(matches!(SlowStorageCheck.evidence(&snapshot(vec![first, second])), CheckEvidence::Insufficient(_)));
    let mut first = file("time-overflow-a");
    first.read_ms = Some(f64::MAX);
    let mut second = first.clone();
    second.name = "time-overflow-b".into();
    let observed = snapshot(vec![first, second]);
    assert!(slow_files(&observed).is_empty());
    assert!(matches!(SlowStorageCheck.evidence(&observed), CheckEvidence::Insufficient(_)));
}

#[test]
fn native_log_grouping_keeps_each_reference_once_and_zero_operation_unknown_timing_cannot_hide_a_positive() {
    let mut active = file("native-log-a");
    active.kind = FileKind::Log;
    let mut idle = active.clone();
    idle.name = "native-idle-log".into();
    idle.writes = 0.0;
    idle.write_ms = None;
    let observed = snapshot(vec![active.clone(), idle]);
    assert!(matches!(SlowStorageCheck.evidence(&observed), CheckEvidence::Available));
    let cases = slow_files(&observed);
    assert_eq!(cases.len(), 1);
    assert_eq!(cases[0].references, [active.reference()]);
    assert_eq!(cases[0].ops, active.writes);
    assert_eq!(cases[0].avg_ms, 30.0);
    let mut second = active.clone();
    second.name = "native-log-b".into();
    let together = snapshot(vec![active.clone(), second.clone()]);
    let cases = slow_files(&together);
    assert_eq!(cases.len(), 1);
    assert_eq!(cases[0].references, [active.reference(), second.reference()]);
    assert_eq!(cases[0].ops, active.writes + second.writes);
}

#[test]
fn complete_storage_assessment_accepts_native_zeroes_and_explicitly_nondurable_log_exclusion() {
    let mut idle = file("idle");
    idle.reads = 0.0;
    idle.read_ms = Some(0.0);
    let mut non_durable = file("temporary-log");
    non_durable.kind = FileKind::Log;
    non_durable.context.write = IoAccounting::NonDurableLog;
    let observed = snapshot(vec![file("data"), idle, non_durable]);
    assert!(matches!(SlowStorageCheck.evidence(&observed), CheckEvidence::Available));
    let mut first = file("quiet-a");
    first.reads = 500.0;
    first.read_ms = Some(15_000.0);
    let mut second = first.clone();
    second.name = "quiet-b".into();
    let combined = snapshot(vec![first, second]);
    assert!(matches!(SlowStorageCheck.evidence(&combined), CheckEvidence::Available));
    assert_eq!(slow_files(&combined).len(), 1, "the operation floor applies to the same qualified population");
}

#[test]
fn mixed_unqualified_population_retains_physical_positives_but_cannot_look_healthy() {
    for accounting in [IoAccounting::CachePage, IoAccounting::PageServerResponse, IoAccounting::Unknown] {
        let mut unsupported = file("native-other");
        unsupported.context.read = accounting;
        let snapshot = snapshot(vec![file("physical"), unsupported]);
        assert_eq!(slow_files(&snapshot).len(), 1);
        assert!(matches!(SlowStorageCheck.evidence(&snapshot), CheckEvidence::Insufficient(_)));
    }
    let mut no_reads = file("idle");
    no_reads.reads = 0.0;
    no_reads.read_ms = Some(0.0);
    assert!(matches!(SlowStorageCheck.evidence(&snapshot(vec![no_reads])), CheckEvidence::Insufficient(_)));
    let mut too_few = file("too-few");
    too_few.reads = 999.0;
    assert!(matches!(SlowStorageCheck.evidence(&snapshot(vec![too_few])), CheckEvidence::Insufficient(_)));
}

#[test]
fn native_file_source_failure_partial_and_missing_outcome_prevent_complete_storage_assessment() {
    let mut snapshot = snapshot(vec![file("physical")]);
    for state in [SourceState::Partial, SourceState::Unavailable, SourceState::Empty] {
        let outcome =
            snapshot.sources.iter_mut().find(|source| source.source == culprit_core::source_id::FILE_LATENCY).unwrap();
        outcome.state = state;
        assert!(!matches!(SlowStorageCheck.evidence(&snapshot), CheckEvidence::Available), "{state:?}");
        assert_eq!(slow_files(&snapshot).len(), 1, "partial source metadata does not erase a measured positive");
    }
    snapshot.sources.retain(|source| source.source != culprit_core::source_id::FILE_LATENCY);
    assert!(matches!(SlowStorageCheck.evidence(&snapshot), CheckEvidence::Insufficient(_)));
}
