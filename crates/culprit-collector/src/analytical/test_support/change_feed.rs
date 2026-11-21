//! Synthetic original change-feed points; these do not attest native support or collection cost.
use super::*;
use culprit_core::ErrorCode;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChangeFeedCase {
    ProgressAndError,
    Empty,
    PermissionDenied,
    UnknownPermission,
    Unsupported,
    ScanFailure,
    PartialErrors,
    NativeNulls,
    UnknownTask,
    Secondary,
}
impl ChangeFeedCase {
    pub const ALL: [Self; 10] = [
        Self::ProgressAndError,
        Self::Empty,
        Self::PermissionDenied,
        Self::UnknownPermission,
        Self::Unsupported,
        Self::ScanFailure,
        Self::PartialErrors,
        Self::NativeNulls,
        Self::UnknownTask,
        Self::Secondary,
    ];
}
pub fn change_feed_evidence(case: ChangeFeedCase) -> (CollectionDescription, Vec<TelemetryRecord>) {
    let context = super::change_capture::context(case != ChangeFeedCase::Secondary);
    let mut availability = MssqlChangeFeedAvailability {
        native_major: 16,
        native_edition: 3,
        has_database_state: Some(true),
        has_database_performance_state: Some(false),
        scan_layout_available: true,
        error_layout_available: true,
    };
    let gap = match case {
        ChangeFeedCase::PermissionDenied => {
            availability.has_database_state = Some(false);
            Some(ErrorCode::PermissionDenied)
        }
        ChangeFeedCase::UnknownPermission => {
            availability.has_database_state = None;
            availability.has_database_performance_state = None;
            Some(CollectionStop::SourceUnavailable.error().code)
        }
        ChangeFeedCase::Unsupported => {
            availability.scan_layout_available = false;
            availability.error_layout_available = false;
            Some(ErrorCode::Unsupported)
        }
        ChangeFeedCase::Secondary => Some(ErrorCode::Unsupported),
        _ => None,
    };
    let mut data = vec![(MssqlChangeFeedData::Availability(availability), 1)];
    let mut sources = vec![StateSourceCoverage::collected(MSSQL_CHANGE_FEED_STATUS_SOURCE.into(), 1)];
    for name in [MSSQL_CHANGE_FEED_SCANS_SOURCE, MSSQL_CHANGE_FEED_ERRORS_SOURCE] {
        if let Some(code) = gap {
            sources.push(StateSourceCoverage::unavailable(name.into(), code, None));
            continue;
        }
        if case == ChangeFeedCase::Empty {
            sources.push(StateSourceCoverage::collected(name.into(), 0));
            continue;
        }
        if name == MSSQL_CHANGE_FEED_SCANS_SOURCE {
            if case == ChangeFeedCase::ScanFailure {
                sources.push(StateSourceCoverage::unavailable(name.into(), ErrorCode::QueryFailed, None));
                continue;
            }
            for (position, session_id) in [(1, 0), (2, 7)] {
                let missing = case == ChangeFeedCase::NativeNulls;
                data.push((
                    MssqlChangeFeedData::ScanSession(Box::new(MssqlChangeFeedScanSession {
                        session_id,
                        started_without_offset: Some("2019-12-31T23:00:00".into()),
                        ended_without_offset: None,
                        native_phase: Some(
                            if session_id == 0 { "Aggregate" } else { "7: Last scan, publish and commit" }.into(),
                        ),
                        duration_seconds: Some("0".into()),
                        error_count: Some("1".into()),
                        transactions: (!missing).then(|| "9007199254740993".into()),
                        log_records: Some("8".into()),
                        commands: None,
                        schema_changes: Some("0".into()),
                        empty_scans: Some("2".into()),
                        failed_sessions: Some("1".into()),
                        latency_seconds: (!missing).then(|| "3".into()),
                        processed_commit_without_offset: None,
                        batch_start_lsn: Some("0".into()),
                        currently_processed_lsn: Some("0".into()),
                        batch_end_lsn: None,
                        processed_commit_lsn: None,
                    })),
                    position,
                ));
            }
            sources.push(StateSourceCoverage::collected(name.into(), 2));
        } else {
            for (position, session_id, task) in [(1, 7, 3), (2, 0, 5)] {
                let missing = case == ChangeFeedCase::NativeNulls || task == 5;
                data.push((
                    MssqlChangeFeedData::Error(Box::new(MssqlChangeFeedError {
                        session_id,
                        source_task: if case == ChangeFeedCase::UnknownTask { Some(255) } else { Some(task) },
                        table_id: (!missing).then_some(410),
                        table_group_id: (!missing).then(|| "10000000-0000-0000-0000-000000000001".into()),
                        capture_phase_number: (!missing).then_some(7),
                        entered_without_offset: Some("2019-12-31T23:00:00.999".into()),
                        error_number: 2628,
                        severity: 16,
                        state: 1,
                        command_id: (!missing).then_some(0),
                        batch_start_lsn: Some("0".into()),
                        batch_end_lsn: Some("0".into()),
                        transaction_begin_lsn: None,
                        transaction_commit_lsn: None,
                        sequence_value: Some("00000016:00000058:0001".into()),
                    })),
                    position,
                ));
            }
            sources.push(StateSourceCoverage::observed(name.into(), 2, case != ChangeFeedCase::PartialErrors));
        }
    }
    let rows = data
        .into_iter()
        .map(|(data, position)| {
            let point = MssqlChangeFeedObservation {
                context: context.clone(),
                database_id: 5,
                observed_at: "2020-01-01T00:00:00.500Z".into(),
                position,
                reset: culprit_core::ResetMarker::Unknown,
                data,
            };
            TelemetryRecord::Operational(OperationalObservation {
                source: point.source().into(),
                key: point.key().unwrap(),
                fact: OperationalFact::MssqlChangeFeed(Box::new(point)),
            })
        })
        .collect::<Vec<_>>();
    let source = CollectionDescription {
        target: context.target,
        partition: context.partition,
        source: "mssql-change-feed".into(),
        continuity: SourceContinuity::Unverified,
        observation: SourceObservation::Snapshot(StateSnapshotDescription {
            dataset: SnapshotDataset::ChangeFeed,
            observed_from: context.observed_from,
            observed_until: context.observed_until,
            records: rows.len() as u32,
            sources,
        }),
    };
    assert!(MssqlChangeCaptureFamily::ChangeFeed.valid_header(source.observation.snapshot().unwrap()));
    assert!(rows.iter().all(TelemetryRecord::is_valid));
    (source, rows)
}
