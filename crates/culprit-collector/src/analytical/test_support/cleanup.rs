//! Source-qualified cleanup points shared by both warehouse readers and offline analysis tests.
use super::*;
use culprit_core::{Engine, ErrorCode, MeasurementScope, source_id};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CleanupCase {
    PersistentVersions,
    NativeNulls,
    UnavailableColumns,
    EmptyHorizon,
    UnattributedParticipant,
    UnavailableNativeSources,
    UnrelatedOperationalCounter,
    QualifiedEmptyRetention,
    UnqualifiedEmptyRetention,
    TiedRetention,
    UnorderedRetention,
    PartialRetention,
    UnavailableRetention,
}

pub fn cleanup_evidence_cases() -> Vec<(CleanupCase, CollectionDescription, Vec<TelemetryRecord>)> {
    use CleanupCase::*;
    [
        PersistentVersions,
        NativeNulls,
        UnavailableColumns,
        EmptyHorizon,
        UnattributedParticipant,
        UnavailableNativeSources,
        UnrelatedOperationalCounter,
        QualifiedEmptyRetention,
        UnqualifiedEmptyRetention,
        TiedRetention,
        UnorderedRetention,
        PartialRetention,
        UnavailableRetention,
    ]
    .into_iter()
    .map(|case| {
        let (source, rows) = cleanup_evidence(case);
        (case, source, rows)
    })
    .collect()
}

pub fn cleanup_tamper_cases() -> Vec<(CleanupCase, &'static str, &'static str, serde_json::Value)> {
    use serde_json::json;
    vec![
        (
            CleanupCase::PersistentVersions,
            "mssql-persistent-version-store",
            "/pages_skipped_for_oldest_active_transaction",
            json!("9007199254740994"),
        ),
        (
            CleanupCase::PersistentVersions,
            "mssql-persistent-version-store",
            "/off_row_cleanup/started_without_offset",
            json!("2020-01-01T13:00:00.1234567Z"),
        ),
        (
            CleanupCase::PersistentVersions,
            "mssql-cleanup-horizon",
            "/oldest_confirmed/transaction/generation",
            json!("2020-01-01T12:00:01.003"),
        ),
        (
            CleanupCase::TiedRetention,
            "retention-coverage",
            "/qualified_horizons",
            json!(["data", "catalog", "shared-catalog"]),
        ),
        (CleanupCase::TiedRetention, "retention", "/horizons/0/transactions_behind", json!("0")),
        (CleanupCase::TiedRetention, "retention", "/unresolved_session_id", json!("100")),
    ]
}

pub fn cleanup_evidence(case: CleanupCase) -> (CollectionDescription, Vec<TelemetryRecord>) {
    match case {
        CleanupCase::QualifiedEmptyRetention => {
            complete_retention_inventory(super::retention_coverage_only_evidence(vec![
                CleanupHorizon::Data,
                CleanupHorizon::Catalog,
            ]))
        }
        CleanupCase::UnqualifiedEmptyRetention => {
            complete_retention_inventory(super::retention_coverage_only_evidence(vec![]))
        }
        CleanupCase::TiedRetention
        | CleanupCase::UnorderedRetention
        | CleanupCase::PartialRetention
        | CleanupCase::UnavailableRetention => retention(case),
        _ => native(case),
    }
}

fn native(case: CleanupCase) -> (CollectionDescription, Vec<TelemetryRecord>) {
    let (mut manifest, _) = fixture();
    let CollectionTarget::Database(database) = &mut manifest.source.target else { unreachable!() };
    database.engine = Engine::Mssql;
    let mut persistent = persistent_versions();
    if matches!(case, CleanupCase::NativeNulls | CleanupCase::UnavailableColumns) {
        persistent.pages_skipped_for_oldest_active_transaction = None;
        persistent.pages_skipped_for_long_snapshot_scan = None;
        persistent.pages_skipped_for_online_index_rebuild = None;
        persistent.pages_skipped_for_oldest_aborted_transaction = None;
    }
    if case == CleanupCase::UnavailableColumns {
        persistent.available_columns.retain(|name| {
            !matches!(
                name.as_str(),
                "pvs_off_row_page_skipped_oldest_active_xdesid"
                    | "pvs_off_row_page_skipped_min_useful_xts"
                    | "pvs_off_row_page_skipped_oldest_snapshot"
                    | "pvs_off_row_page_skipped_oldest_aborted_xdesid"
            )
        });
    }
    let mut horizon = MssqlCleanupHorizon {
        oldest_confirmed: Some(Box::new(transaction("57", "1001"))),
        oldest_unattributed_versioning: None,
    };
    if case == CleanupCase::UnattributedParticipant {
        let mut unknown = transaction("58", "1002");
        unknown.log_record_count = Some("0".into());
        unknown.log_bytes_used = None;
        unknown.snapshot_isolation = Some(false);
        horizon.oldest_unattributed_versioning = Some(Box::new(unknown));
    }
    if case == CleanupCase::EmptyHorizon {
        horizon.oldest_confirmed = None;
    }
    let mut rows = vec![
        TelemetryRecord::Operational(OperationalObservation {
            source: source_id::PERSISTENT_VERSION_STORE.into(),
            key: "persistent-version-store".into(),
            fact: OperationalFact::MssqlPersistentVersionStore(Box::new(persistent)),
        }),
        TelemetryRecord::Operational(OperationalObservation {
            source: source_id::OPEN_TRANSACTIONS.into(),
            key: "oldest-cleanup-transaction".into(),
            fact: OperationalFact::MssqlCleanupHorizon(horizon),
        }),
    ];
    let mut sources = vec![
        StateSourceCoverage::collected(source_id::PERSISTENT_VERSION_STORE.into(), 1),
        StateSourceCoverage::observed(
            source_id::OPEN_TRANSACTIONS.into(),
            1,
            case != CleanupCase::UnattributedParticipant,
        ),
    ];
    if case == CleanupCase::EmptyHorizon {
        rows.remove(0);
        sources[0] = StateSourceCoverage::not_applicable(source_id::PERSISTENT_VERSION_STORE.into());
    }
    if case == CleanupCase::UnavailableNativeSources {
        rows.clear();
        for source in &mut sources {
            *source = StateSourceCoverage::unavailable(source.source.clone(), ErrorCode::PermissionDenied, None);
        }
    }
    if case == CleanupCase::UnrelatedOperationalCounter {
        rows.push(TelemetryRecord::Operational(OperationalObservation {
            source: "unrelated-operational".into(),
            key: "unrelated-counter".into(),
            fact: OperationalFact::Counter(NativeCounterSample::gauge(
                "unrelated-counter",
                17,
                CounterUnit::Count,
                MeasurementScope::Database,
                "unrelated_native_view",
                "native_value",
            )),
        }));
        sources.push(StateSourceCoverage::collected("unrelated-operational".into(), 1));
    }
    manifest.source.source = "retained-native-cleanup-points".into();
    manifest.source.observation = SourceObservation::Snapshot(StateSnapshotDescription {
        dataset: SnapshotDataset::Operational,
        observed_from: "2020-01-01T12:00:00Z".into(),
        observed_until: "2020-01-01T12:00:01Z".into(),
        records: rows.len() as u32,
        sources,
    });
    (manifest.source, rows)
}

fn persistent_versions() -> MssqlPersistentVersionStore {
    let mut columns = [
        "persistent_version_store_size_kb",
        "pvs_filegroup_id",
        "online_index_version_store_size_kb",
        "current_aborted_transaction_count",
        "oldest_active_transaction_id",
        "oldest_aborted_transaction_id",
        "min_transaction_timestamp",
        "online_index_min_transaction_timestamp",
        "secondary_low_water_mark",
        "offrow_version_cleaner_start_time",
        "offrow_version_cleaner_end_time",
        "aborted_version_cleaner_start_time",
        "aborted_version_cleaner_end_time",
        "pvs_off_row_page_skipped_low_water_mark",
        "pvs_off_row_page_skipped_transaction_not_cleaned",
        "pvs_off_row_page_skipped_oldest_active_xdesid",
        "pvs_off_row_page_skipped_min_useful_xts",
        "pvs_off_row_page_skipped_oldest_snapshot",
        "pvs_off_row_page_skipped_oldest_aborted_xdesid",
    ]
    .into_iter()
    .map(str::to_string)
    .collect::<Vec<_>>();
    columns.sort();
    MssqlPersistentVersionStore {
        available_columns: columns,
        filegroup_id: Some(1),
        off_row_kibibytes: Some("9007199254740993".into()),
        online_index_kibibytes: Some("0".into()),
        aborted_transactions: Some("5".into()),
        oldest_active_transaction_id: Some("1001".into()),
        oldest_aborted_transaction_id: Some("-1".into()),
        minimum_transaction_timestamp: Some("-9223372036854775808".into()),
        online_index_minimum_timestamp: Some("0".into()),
        secondary_low_water_mark: Some("9007199254740993".into()),
        off_row_cleanup: MssqlVersionCleanupSweep {
            started_without_offset: Some("2020-01-01T13:00:00.1234567".into()),
            ended_without_offset: None,
        },
        aborted_cleanup: MssqlVersionCleanupSweep { started_without_offset: None, ended_without_offset: None },
        pages_skipped_for_secondaries: Some("9007199254740993".into()),
        pages_skipped_for_aborted_transactions: Some("7".into()),
        pages_skipped_for_oldest_active_transaction: Some("9007199254740993".into()),
        pages_skipped_for_long_snapshot_scan: Some("0".into()),
        pages_skipped_for_online_index_rebuild: None,
        pages_skipped_for_oldest_aborted_transaction: Some("9".into()),
    }
}

fn transaction(session: &str, transaction: &str) -> MssqlCleanupTransaction {
    MssqlCleanupTransaction {
        session: NativeActivityIdentity { id: session.into(), generation: "2020-01-01T10:00:00.003".into() },
        transaction: NativeActivityIdentity { id: transaction.into(), generation: "2020-01-01T12:00:00.003".into() },
        observed_time_without_offset: "2020-01-01T13:00:00.003".into(),
        age_seconds: Some("3600".into()),
        program: Some("application-worker".into()),
        native_status: Some("sleeping".into()),
        log_record_count: Some("1".into()),
        log_bytes_used: Some("9007199254740993".into()),
        begin_lsn: Some("9999999999999999999999999".into()),
        row_version_participant: true,
        snapshot_isolation: Some(false),
        transaction_sequence_number: Some("9007199254740993".into()),
        versioning_age_seconds: Some("12".into()),
    }
}

fn retention(case: CleanupCase) -> (CollectionDescription, Vec<TelemetryRecord>) {
    let (mut source, mut rows) = super::retention_evidence();
    let TelemetryRecord::Operational(first) = &mut rows[1] else { unreachable!() };
    let OperationalFact::Retention(holder) = &mut first.fact else { unreachable!() };
    holder.horizons[0].native_value = "100000000".into();
    holder.horizons[1].native_value = "50000000".into();
    holder.horizons[1].transactions_behind = Some("250000000".into());
    let mut tied = holder.as_ref().clone();
    tied.identity.id = "second-native-slot".into();
    tied.name = "second-native-slot".into();
    tied.horizons.retain(|horizon| horizon.native_name == "xmin");
    rows.push(TelemetryRecord::Operational(OperationalObservation {
        source: tied.source.clone(),
        key: tied.key().unwrap(),
        fact: OperationalFact::Retention(Box::new(tied)),
    }));
    if case == CleanupCase::UnorderedRetention {
        let TelemetryRecord::Operational(row) = &rows[2] else { unreachable!() };
        let OperationalFact::Retention(holder) = &row.fact else { unreachable!() };
        let mut unknown = holder.as_ref().clone();
        unknown.identity.id = "unorderable-native-slot".into();
        unknown.name = "unorderable-native-slot".into();
        unknown.horizons[0].transactions_behind = None;
        rows.push(TelemetryRecord::Operational(OperationalObservation {
            source: unknown.source.clone(),
            key: unknown.key().unwrap(),
            fact: OperationalFact::Retention(Box::new(unknown)),
        }));
    }
    let SourceObservation::Snapshot(native) = &mut source.observation else { unreachable!() };
    native.records = rows.len() as u32;
    native.sources[1] = StateSourceCoverage::observed(
        "pg_replication_slots".into(),
        rows.len() as u32 - 1,
        case != CleanupCase::PartialRetention,
    );
    if case == CleanupCase::UnavailableRetention {
        rows.truncate(1);
        native.records = 1;
        native.sources[1] =
            StateSourceCoverage::unavailable("pg_replication_slots".into(), ErrorCode::PermissionDenied, None);
    }
    complete_retention_inventory((source, rows))
}

fn complete_retention_inventory(
    (mut source, mut rows): (CollectionDescription, Vec<TelemetryRecord>),
) -> (CollectionDescription, Vec<TelemetryRecord>) {
    let TelemetryRecord::Operational(row) = &mut rows[0] else { unreachable!() };
    let OperationalFact::RetentionCoverage(coverage) = &mut row.fact else { unreachable!() };
    coverage.native_sources =
        vec!["pg_stat_activity".into(), "pg_replication_slots".into(), "pg_prepared_xacts".into()];
    row.key = coverage.key().unwrap();
    let SourceObservation::Snapshot(native) = &mut source.observation else { unreachable!() };
    for name in ["pg_stat_activity", "pg_prepared_xacts"] {
        native.sources.push(StateSourceCoverage::collected(name.into(), 0));
    }
    (source, rows)
}

pub fn assert_cleanup_capture(
    capture: &crate::analytical::evidence::EvidenceCapture,
    source: &CollectionDescription,
    records: &[TelemetryRecord],
) {
    assert!(capture.is_read_complete(), "{:?}", capture.gaps());
    assert_eq!(capture.source(), source);
    assert_eq!(capture.records().len(), records.len());
    for record in records {
        let expected = ExportedRecord::new(source, capture.capture(), record.clone()).unwrap();
        assert_eq!(capture.records().iter().filter(|row| row.row() == &expected).count(), 1);
    }
}
