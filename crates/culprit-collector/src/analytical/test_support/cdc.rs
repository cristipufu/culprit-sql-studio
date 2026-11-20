//! Synthetic original CDC conformance points. Native acceptance is a separate owned experiment.
use super::*;
use culprit_core::{DatabaseIdentity, ErrorCode};
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CdcCase {
    ErrorAndProgress,
    NoRetainedErrors,
    Disabled,
    PermissionDenied,
    Secondary,
    Unsupported,
    PartialErrors,
    UnknownFlag,
    FailedScans,
}
impl CdcCase {
    pub const ALL: [Self; 9] = [
        Self::ErrorAndProgress,
        Self::NoRetainedErrors,
        Self::Disabled,
        Self::PermissionDenied,
        Self::Secondary,
        Self::Unsupported,
        Self::PartialErrors,
        Self::UnknownFlag,
        Self::FailedScans,
    ];
}
pub fn cdc_evidence(case: CdcCase) -> (CollectionDescription, Vec<TelemetryRecord>) {
    let context = NativeReadContext {
        target: DatabaseIdentity::server(culprit_core::Engine::Mssql, "db", "original.invalid", 1433, None).into(),
        partition: SourcePartition::InstanceLocal {
            member: Some(SharedResourceIdentity {
                engine: culprit_core::Engine::Mssql,
                kind: SharedResourceKind::Instance,
                namespace: NativeIdentity::new("machine"),
                native: NativeIdentity::new("original-instance"),
            }),
            epoch: Some("original-process-and-db".into()),
        },
        observed_from: "2020-01-01T00:00:00Z".into(),
        observed_until: "2020-01-01T00:00:01Z".into(),
        database_writable: Some(case != CdcCase::Secondary),
    };
    let mut status = MssqlCdcStatus {
        native_major: 16,
        native_edition: 3,
        enabled: Some(true),
        permission: MssqlCdcPermission::ViewDatabasePerformanceState,
        has_permission: Some(true),
        scan_layout_available: true,
        error_layout_available: true,
    };
    let gap = match case {
        CdcCase::Disabled => {
            status.enabled = Some(false);
            None
        }
        CdcCase::PermissionDenied => {
            status.has_permission = Some(false);
            Some(ErrorCode::PermissionDenied)
        }
        CdcCase::Secondary => Some(ErrorCode::Unsupported),
        CdcCase::Unsupported => {
            status.scan_layout_available = false;
            status.error_layout_available = false;
            Some(ErrorCode::Unsupported)
        }
        CdcCase::UnknownFlag => {
            status.enabled = None;
            Some(ErrorCode::QueryFailed)
        }
        _ => None,
    };
    let mut data = vec![MssqlCdcData::Status(status)];
    let mut sources = vec![StateSourceCoverage::collected(MSSQL_CDC_STATUS_SOURCE.into(), 1)];
    for name in [MSSQL_CDC_SCANS_SOURCE, MSSQL_CDC_ERRORS_SOURCE] {
        if case == CdcCase::Disabled {
            sources.push(StateSourceCoverage::not_applicable(name.into()));
            continue;
        }
        if let Some(code) = gap {
            sources.push(StateSourceCoverage::unavailable(name.into(), code, None));
            continue;
        }
        if name == MSSQL_CDC_SCANS_SOURCE {
            if case == CdcCase::FailedScans {
                sources.push(StateSourceCoverage::unavailable(name.into(), ErrorCode::QueryFailed, None));
                continue;
            }
            data.push(MssqlCdcData::ScanSession(Box::new(MssqlCdcScanSession {
                session_id: 0,
                started_without_offset: Some("2019-12-31T23:00:00".into()),
                ended_without_offset: None,
                native_phase: Some("Aggregate".into()),
                duration_seconds: Some("0".into()),
                error_count: Some("1".into()),
                transactions: Some("2".into()),
                log_records: Some("8".into()),
                commands: None,
                failed_sessions: Some("1".into()),
                latency_seconds: Some("3".into()),
                last_commit_without_offset: None,
                last_cdc_commit_without_offset: None,
                start_lsn: Some("0".into()),
                current_lsn: Some("0".into()),
                end_lsn: None,
            })));
            sources.push(StateSourceCoverage::collected(name.into(), 1));
        } else if case == CdcCase::NoRetainedErrors {
            sources.push(StateSourceCoverage::collected(name.into(), 0));
        } else {
            data.push(MssqlCdcData::Error(MssqlCdcError {
                session_id: 7,
                phase_number: Some(2),
                entered_without_offset: Some("2019-12-31T23:00:00.999".into()),
                error_number: 2628,
                severity: 16,
                state: 1,
                start_lsn: Some("00000016:00000058:0001".into()),
                begin_lsn: None,
                sequence_value: Some("0".into()),
            }));
            sources.push(StateSourceCoverage::observed(name.into(), 1, case != CdcCase::PartialErrors));
        }
    }
    let rows = data
        .into_iter()
        .map(|data| {
            let point = MssqlCdcObservation {
                context: context.clone(),
                database_id: 5,
                observed_at: "2020-01-01T00:00:00.500Z".into(),
                position: 1,
                reset: culprit_core::ResetMarker::Unknown,
                data,
            };
            TelemetryRecord::Operational(OperationalObservation {
                source: point.source().into(),
                key: point.key().unwrap(),
                fact: OperationalFact::MssqlCdc(Box::new(point)),
            })
        })
        .collect::<Vec<_>>();
    let source = CollectionDescription {
        target: context.target,
        partition: context.partition,
        source: "mssql-cdc".into(),
        continuity: SourceContinuity::Unverified,
        observation: SourceObservation::Snapshot(StateSnapshotDescription {
            dataset: SnapshotDataset::Cdc,
            observed_from: context.observed_from,
            observed_until: context.observed_until,
            records: rows.len() as u32,
            sources,
        }),
    };
    assert!(cdc_header_is_valid(source.observation.snapshot().unwrap()));
    assert!(rows.iter().all(TelemetryRecord::is_valid));
    (source, rows)
}
