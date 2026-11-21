//! A single original file roster and an independently timed log point; controlled facts are not native cost proof.
use super::*;
use culprit_core::{FileKind, FileScope, StorageReport};
const DATA_FILES_LARGE: u32 = 300;

pub(super) fn evidence(case: StorageCase) -> (CollectionDescription, Vec<TelemetryRecord>) {
    let (mut source, original) = storage_evidence(StorageCase::Exact);
    let TelemetryRecord::Operational(OperationalObservation {
        fact: OperationalFact::MssqlDatabaseFile(template), ..
    }) = &original[1]
    else {
        unreachable!()
    };
    let TelemetryRecord::Operational(OperationalObservation { fact: OperationalFact::Space(mut group), .. }) =
        original[0].clone()
    else {
        unreachable!()
    };
    let count = if case == StorageCase::FileCensusLarge { DATA_FILES_LARGE } else { 2 };
    let mut files = Vec::new();
    for id in 1..=count {
        let mut file = template.as_ref().clone();
        file.file_id = id.to_string();
        file.logical_name = format!("original-data-{id}");
        file.size_pages = if case == StorageCase::FileCensusLarge {
            "2147483647".into()
        } else if id == 1 {
            "128".into()
        } else {
            "64".into()
        };
        file.used_pages = Some(if case == StorageCase::FileCensusLarge {
            "0".into()
        } else if id == 1 {
            "32".into()
        } else {
            "16".into()
        });
        if case == StorageCase::FileCensusUnknownUsage && id == 2 {
            file.native_state = "6".into();
            file.state_description = Some("OFFLINE".into());
            file.used_pages = None;
        }
        files.push(file);
    }
    let mut log = template.as_ref().clone();
    log.file_id = "1001".into();
    log.logical_name = "original-log".into();
    log.kind = FileKind::Log;
    log.native_type = "1".into();
    log.type_description = Some("LOG".into());
    log.data_space_id = "0".into();
    log.filegroup_read_only = None;
    log.size_pages = "32".into();
    log.used_pages = None;
    files.push(log);
    let mut other = template.as_ref().clone();
    other.file_id = "1002".into();
    other.logical_name = "original-container".into();
    other.kind = FileKind::Other;
    other.native_type = "2".into();
    other.type_description = Some("FILESTREAM".into());
    other.data_space_id = "2".into();
    other.size_basis = StorageFileSizeBasis::ContainerUsed;
    other.size_pages = "256".into();
    other.used_pages = None;
    files.push(other);
    if case == StorageCase::FileCensusUnknownBasis {
        for file in &mut files {
            file.database_is_snapshot = None;
            file.size_basis = StorageFileSizeBasis::Unknown;
        }
    }
    let allocated = if case == StorageCase::FileCensusLarge {
        "5277655810867200"
    } else if case == StorageCase::FileCensusUnknownUsage {
        "1048576"
    } else {
        "1572864"
    };
    group.total_bytes = Some(allocated.into());
    group.used_bytes = Some(
        if case == StorageCase::FileCensusLarge {
            "0"
        } else if case == StorageCase::FileCensusUnknownUsage {
            "262144"
        } else {
            "393216"
        }
        .into(),
    );
    let mut log_point = group.clone();
    log_point.population = StoragePopulation::DatabaseTransactionLog;
    log_point.kind = FileKind::Log;
    log_point.scope = FileScope::Database;
    log_point.native_name = None;
    log_point.observed_at = "2020-01-01T12:00:00.75Z".into();
    log_point.total_bytes = Some("196608".into());
    log_point.used_bytes = Some("8192".into());
    let mut rows = if case == StorageCase::FileCensusLogDenied {
        vec![]
    } else {
        vec![point(culprit_core::source_id::LOG_CAPACITY, "database".into(), OperationalFact::Space(log_point))]
    };
    if case != StorageCase::FileCensusDenied {
        rows.push(point(culprit_core::source_id::FILE_CAPACITY, "filegroup:1".into(), OperationalFact::Space(group)));
        for file in files {
            if case == StorageCase::FileCensusPartial && file.file_id != "1" {
                continue;
            }
            rows.push(point(
                culprit_core::source_id::FILE_CAPACITY,
                file.key().unwrap(),
                OperationalFact::MssqlDatabaseFile(Box::new(file)),
            ));
        }
    }
    let SourceObservation::Snapshot(header) = &mut source.observation else { unreachable!() };
    let file_rows = rows
        .iter()
        .filter(
            |row| matches!(row,TelemetryRecord::Operational(row) if row.source==culprit_core::source_id::FILE_CAPACITY),
        )
        .count() as u32;
    header.records = rows.len() as u32;
    header.sources = vec![
        if case == StorageCase::FileCensusDenied {
            StateSourceCoverage::unavailable(
                culprit_core::source_id::FILE_CAPACITY.into(),
                culprit_core::ErrorCode::PermissionDenied,
                None,
            )
        } else {
            StateSourceCoverage::observed(
                culprit_core::source_id::FILE_CAPACITY.into(),
                file_rows,
                case != StorageCase::FileCensusPartial,
            )
        },
        if case == StorageCase::FileCensusLogDenied {
            StateSourceCoverage::unavailable(
                culprit_core::source_id::LOG_CAPACITY.into(),
                culprit_core::ErrorCode::PermissionDenied,
                None,
            )
        } else {
            StateSourceCoverage::collected(culprit_core::source_id::LOG_CAPACITY.into(), 1)
        },
    ];
    (source, rows)
}
fn point(source: &str, key: String, fact: OperationalFact) -> TelemetryRecord {
    TelemetryRecord::Operational(OperationalObservation { source: source.into(), key, fact })
}

/// Consumer acceptance over the exact same canonical original; no source query or aggregation is duplicated.
pub fn assert_storage_file_summary(report: &StorageReport, case: StorageCase) {
    if !matches!(
        case,
        StorageCase::FileCensus
            | StorageCase::FileCensusUnknownUsage
            | StorageCase::FileCensusUnknownBasis
            | StorageCase::FileCensusPartial
            | StorageCase::FileCensusDenied
            | StorageCase::FileCensusLogDenied
            | StorageCase::FileCensusLarge
    ) {
        return;
    }
    let summary = report.file_summary.as_ref().expect("selected database file source is explicit");
    assert_eq!(summary.source, culprit_core::source_id::FILE_CAPACITY);
    if matches!(case, StorageCase::FileCensusPartial | StorageCase::FileCensusDenied) {
        assert_eq!(summary.data_file_count, None);
        assert_eq!(summary.log_file_count, None);
        assert_eq!(summary.other_file_count, None);
        assert!(
            summary.data_allocated_bytes.is_none()
                && summary.data_used_bytes.is_none()
                && summary.log_catalog_allocated_bytes.is_none()
        );
        assert!(summary.observed_at.is_none() && summary.native_database_id.is_none());
        assert_eq!(
            summary.source_state,
            if case == StorageCase::FileCensusDenied {
                culprit_core::SourceState::Unavailable
            } else {
                culprit_core::SourceState::Partial
            }
        );
        return;
    }
    assert_eq!(summary.source_state, culprit_core::SourceState::Collected);
    assert_eq!(summary.data_file_count, Some(if case == StorageCase::FileCensusLarge { DATA_FILES_LARGE } else { 2 }));
    assert_eq!(summary.log_file_count, Some(1));
    assert_eq!(summary.other_file_count, Some(1));
    assert_eq!(summary.native_database_id.as_deref(), Some("5"));
    assert_eq!(summary.observed_at.as_deref(), Some("2020-01-01T12:00:00.500000000Z"));
    assert_eq!(
        summary.data_allocated_bytes.as_deref(),
        if case == StorageCase::FileCensusUnknownBasis {
            None
        } else {
            Some(if case == StorageCase::FileCensusLarge { "5277655810867200" } else { "1572864" })
        }
    );
    assert_eq!(
        summary.data_used_bytes.as_deref(),
        if case == StorageCase::FileCensusUnknownUsage {
            None
        } else {
            Some(if case == StorageCase::FileCensusLarge { "0" } else { "393216" })
        }
    );
    assert_eq!(
        summary.log_catalog_allocated_bytes.as_deref(),
        if case == StorageCase::FileCensusUnknownBasis { None } else { Some("262144") }
    );
    if case == StorageCase::FileCensusLogDenied {
        assert_eq!(report.source_state, culprit_core::SourceState::Partial);
    } else {
        let log = report.entries.iter().find(|entry| entry.source == culprit_core::source_id::LOG_CAPACITY).unwrap();
        assert_eq!(
            log.fields.iter().find(|field| field.key == "observed_at").unwrap().value.as_deref(),
            Some("2020-01-01T12:00:00.75Z")
        );
        assert_eq!(
            log.fields.iter().find(|field| field.key == "total_bytes").unwrap().value.as_deref(),
            Some("196608")
        );
        assert_eq!(log.fields.iter().find(|field| field.key == "used_bytes").unwrap().value.as_deref(), Some("8192"));
    }
    if case == StorageCase::FileCensusLarge {
        assert!(report.entries_limited);
        assert_eq!(report.presented_rows, 256);
    }
}
