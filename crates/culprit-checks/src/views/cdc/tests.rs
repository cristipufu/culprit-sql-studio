use super::*;
use culprit_core::{DatabaseIdentity, Engine, ResetMarker};

fn context() -> NativeReadContext {
    NativeReadContext {
        target: DatabaseIdentity::server(Engine::Mssql, "db", "original.invalid", 1433, None).into(),
        partition: SourcePartition::InstanceLocal {
            member: Some(SharedResourceIdentity {
                engine: Engine::Mssql,
                kind: SharedResourceKind::Instance,
                namespace: NativeIdentity::new("machine"),
                native: NativeIdentity::new("member"),
            }),
            epoch: Some("original-process-and-db".into()),
        },
        observed_from: "2020-01-01T00:00:00Z".into(),
        observed_until: "2020-01-01T00:00:01Z".into(),
        database_writable: Some(true),
    }
}
fn observation(data: MssqlCdcData, position: u32) -> OperationalObservation {
    let point = MssqlCdcObservation {
        context: context(),
        database_id: 5,
        observed_at: "2020-01-01T00:00:00.5Z".into(),
        position,
        reset: ResetMarker::Unknown,
        data,
    };
    OperationalObservation {
        source: point.source().into(),
        key: point.key().unwrap(),
        fact: OperationalFact::MssqlCdc(Box::new(point)),
    }
}
fn original() -> (CollectionDescription, Vec<OperationalObservation>) {
    let status = MssqlCdcStatus {
        native_major: 16,
        native_edition: 3,
        enabled: Some(true),
        permission: MssqlCdcPermission::ViewDatabasePerformanceState,
        has_permission: Some(true),
        scan_layout_available: true,
        error_layout_available: true,
    };
    let scan = |session_id, transactions: &str| MssqlCdcScanSession {
        session_id,
        started_without_offset: Some("2019-12-31T21:00:00".into()),
        ended_without_offset: None,
        native_phase: Some("7 Last scan".into()),
        duration_seconds: Some("0".into()),
        error_count: Some("0".into()),
        transactions: Some(transactions.into()),
        log_records: Some("18446744073709551615".into()),
        commands: None,
        failed_sessions: Some("1".into()),
        latency_seconds: Some("3".into()),
        last_commit_without_offset: None,
        last_cdc_commit_without_offset: Some("2019-12-31T21:00:00.123".into()),
        start_lsn: Some("00000016:00000058:0001".into()),
        current_lsn: Some("0".into()),
        end_lsn: None,
    };
    let error = MssqlCdcError {
        session_id: 7,
        phase_number: Some(2),
        entered_without_offset: Some("2019-12-31T21:00:00.999".into()),
        error_number: 2628,
        severity: 16,
        state: 1,
        start_lsn: Some("00000016:00000058:0001".into()),
        begin_lsn: None,
        sequence_value: Some("0".into()),
    };
    let rows = vec![
        observation(MssqlCdcData::Status(status), 1),
        observation(MssqlCdcData::ScanSession(Box::new(scan(0, "1000"))), 1),
        observation(MssqlCdcData::ScanSession(Box::new(scan(7, "2"))), 2),
        observation(MssqlCdcData::Error(error), 1),
    ];
    let owner = context();
    let source = CollectionDescription {
        target: owner.target,
        partition: owner.partition,
        source: "mssql-cdc".into(),
        continuity: SourceContinuity::Unverified,
        observation: SourceObservation::Snapshot(StateSnapshotDescription {
            dataset: SnapshotDataset::Cdc,
            observed_from: owner.observed_from,
            observed_until: owner.observed_until,
            records: rows.len() as u32,
            sources: vec![
                StateSourceCoverage::collected(MSSQL_CDC_STATUS_SOURCE.into(), 1),
                StateSourceCoverage::collected(MSSQL_CDC_SCANS_SOURCE.into(), 2),
                StateSourceCoverage::collected(MSSQL_CDC_ERRORS_SOURCE.into(), 1),
            ],
        }),
    };
    (source, rows)
}
fn report(source: &CollectionDescription, rows: &[OperationalObservation], read_complete: bool) -> Result<CdcReport> {
    explain(CdcEvidence { source, observations: &rows.iter().collect::<Vec<_>>(), read_complete })
}
#[test]
fn genuine_native_error_and_scan_points_keep_independent_roles_units_and_clocks() {
    let (source, rows) = original();
    let report = report(&source, &rows, true).unwrap();
    assert_eq!(report.source_state, SourceState::Collected);
    assert_eq!(report.enabled, Some(true));
    assert_eq!(report.entries.len(), 3);
    assert_eq!(report.entries[0].kind, CdcEntryKind::Error);
    let aggregate = report.entries.iter().find(|entry| entry.kind == CdcEntryKind::Aggregate).unwrap();
    let scan = report.entries.iter().find(|entry| entry.kind == CdcEntryKind::ScanSession).unwrap();
    assert_eq!(aggregate.fields.iter().find(|field| field.key == "tran_count").unwrap().value.as_deref(), Some("1000"));
    assert_eq!(scan.fields.iter().find(|field| field.key == "tran_count").unwrap().value.as_deref(), Some("2"));
    assert_eq!(scan.fields.iter().find(|field| field.key == "end_time").unwrap().value, None);
    assert_eq!(
        scan.fields.iter().find(|field| field.key == "log_record_count").unwrap().value.as_deref(),
        Some("18446744073709551615")
    );
    assert!(report.entries[0].fields.iter().any(|field| field.value.as_deref() == Some("2019-12-31T21:00:00.999")));
    let json = serde_json::to_string(&report).unwrap();
    assert!(!json.contains("00000016:00000058:0001"));
    for field in ["current_lag", "new_errors", "severity\":\"critical", "causal_share", "query_id"] {
        assert!(!json.contains(field), "{field}");
    }
}
#[test]
fn duplicates_and_partial_body_are_original_lower_bounds_without_counter_deltas() {
    let (mut source, mut rows) = original();
    let mut duplicate = rows[3].clone();
    let OperationalFact::MssqlCdc(point) = &mut duplicate.fact else { unreachable!() };
    point.position = 2;
    duplicate.key = point.key().unwrap();
    rows.push(duplicate);
    let SourceObservation::Snapshot(header) = &mut source.observation else { unreachable!() };
    header.records += 1;
    header.sources[2] = StateSourceCoverage::observed(MSSQL_CDC_ERRORS_SOURCE.into(), 2, false);
    let full = report(&source, &rows, true).unwrap();
    assert_eq!(full.entries.iter().filter(|entry| entry.kind == CdcEntryKind::Error).count(), 2);
    assert_eq!(full.source_state, SourceState::Partial);
    let partial = report(&source, &rows[3..], false).unwrap();
    assert_eq!(partial.enabled, None);
    assert_eq!(partial.entries.len(), 2);
    assert_eq!(partial.source_state, SourceState::Partial);
    assert!(partial.limitations.iter().any(|value| value == &t!("view.cdc.limit.partial")));
}
#[test]
fn empty_retained_errors_disabled_and_unavailable_never_become_a_healthy_result() {
    let (mut source, mut rows) = original();
    rows.pop();
    let SourceObservation::Snapshot(header) = &mut source.observation else { unreachable!() };
    header.records -= 1;
    header.sources[2] = StateSourceCoverage::collected(MSSQL_CDC_ERRORS_SOURCE.into(), 0);
    let empty = report(&source, &rows, true).unwrap();
    assert!(empty.entries.iter().all(|entry| entry.kind != CdcEntryKind::Error));
    assert_eq!(empty.summary, t!("view.cdc.summary.recorded"));
    assert!(!empty.limitations.is_empty());
    rows.truncate(1);
    let OperationalFact::MssqlCdc(point) = &mut rows[0].fact else { unreachable!() };
    let MssqlCdcData::Status(status) = &mut point.data else { unreachable!() };
    status.enabled = Some(false);
    let SourceObservation::Snapshot(header) = &mut source.observation else { unreachable!() };
    header.records = 1;
    header.sources[1] = StateSourceCoverage::not_applicable(MSSQL_CDC_SCANS_SOURCE.into());
    header.sources[2] = StateSourceCoverage::not_applicable(MSSQL_CDC_ERRORS_SOURCE.into());
    let disabled = report(&source, &rows, true).unwrap();
    assert_eq!(disabled.enabled, Some(false));
    assert_eq!(disabled.summary, t!("view.cdc.summary.disabled"));
    assert!(disabled.entries.is_empty());
}
#[test]
fn denied_layout_null_and_read_only_preserve_distinct_original_qualifications() {
    for code in [ErrorCode::PermissionDenied, ErrorCode::Unsupported, ErrorCode::QueryFailed] {
        let (mut source, mut rows) = original();
        rows.truncate(1);
        let OperationalFact::MssqlCdc(point) = &mut rows[0].fact else { unreachable!() };
        let MssqlCdcData::Status(status) = &mut point.data else { unreachable!() };
        status.has_permission = Some(false);
        let SourceObservation::Snapshot(header) = &mut source.observation else { unreachable!() };
        header.records = 1;
        for coverage in header.sources.iter_mut().skip(1) {
            *coverage = StateSourceCoverage::unavailable(coverage.source.clone(), code, None);
        }
        let denied = report(&source, &rows, true).unwrap();
        assert_eq!(denied.source_state, SourceState::Partial);
        assert!(denied.entries.is_empty());
        if code == ErrorCode::PermissionDenied {
            assert!(denied.limitations.iter().any(|value| value.contains("VIEW DATABASE PERFORMANCE STATE")));
        }
        let missing = report(&source, &[], false).unwrap();
        assert_eq!(missing.enabled, None);
        assert_eq!(missing.source_state, SourceState::Unavailable);
        assert!(!missing.limitations.iter().any(|value| value.contains("VIEW DATABASE STATE")));
    }
    let (source, mut rows) = original();
    for row in &mut rows {
        let OperationalFact::MssqlCdc(point) = &mut row.fact else { unreachable!() };
        point.context.database_writable = Some(false);
    }
    let readonly = report(&source, &rows, true).unwrap();
    assert!(readonly.limitations.contains(&t!("view.cdc.limit.read_only")));
}
#[test]
fn wrong_engine_owner_scope_clock_database_and_invented_reset_are_refused() {
    for change in ["engine", "member", "epoch", "scope", "clock", "database", "reset", "dataset", "missing-source"] {
        let (mut source, mut rows) = original();
        let OperationalFact::MssqlCdc(point) = &mut rows[0].fact else { unreachable!() };
        match change {
            "engine" => {
                let CollectionTarget::Database(database) = &mut source.target else { unreachable!() };
                database.engine = Engine::Postgres;
                let SourcePartition::InstanceLocal { member: Some(member), .. } = &mut source.partition else {
                    unreachable!()
                };
                member.engine = Engine::Postgres;
            }
            "member" => {
                let SourcePartition::InstanceLocal { member: Some(member), .. } = &mut point.context.partition else {
                    unreachable!()
                };
                member.native.id = "other".into();
            }
            "epoch" => {
                point.context.partition = SourcePartition::InstanceLocal { member: None, epoch: None };
            }
            "scope" => {
                source.partition = SourcePartition::Target;
            }
            "clock" => {
                point.observed_at = "2020-01-01T00:00:02Z".into();
            }
            "database" => {
                point.database_id = 6;
            }
            "reset" => {
                point.reset = ResetMarker::Generation("not-cdc-reset".into());
            }
            "dataset" => {
                let SourceObservation::Snapshot(header) = &mut source.observation else { unreachable!() };
                header.dataset = SnapshotDataset::Operational;
            }
            _ => {
                let SourceObservation::Snapshot(header) = &mut source.observation else { unreachable!() };
                header.sources.remove(0);
                header.records -= 1;
                rows.remove(0);
            }
        }
        assert!(report(&source, &rows, true).is_err(), "{change}");
    }
}
