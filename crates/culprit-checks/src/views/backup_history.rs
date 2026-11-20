//! One original-source backup interpretation. Retained records establish neither recovery readiness nor query cause.
use culprit_core::{BackupHistoryEntry, BackupHistoryReport, ErrorCode, Result, SourceState, format, t, telemetry::*};
use std::collections::BTreeSet;

pub struct BackupHistoryEvidence<'a> {
    pub source: &'a CollectionDescription,
    pub observations: &'a [&'a OperationalObservation],
    pub read_complete: bool,
}
pub fn explain_captured(read: &CapturedOperationalRead) -> Result<BackupHistoryReport> {
    if !read.is_valid() {
        return Err(invalid());
    }
    explain(BackupHistoryEvidence {
        source: &read.description,
        observations: &read.observations.iter().collect::<Vec<_>>(),
        read_complete: read.read_complete,
    })
}
pub fn explain(evidence: BackupHistoryEvidence<'_>) -> Result<BackupHistoryReport> {
    let qualified = validate(&evidence)?;
    let source_state = if qualified.coverage.state == SourceState::Unavailable {
        SourceState::Unavailable
    } else {
        SourceState::Partial
    };
    let summary = if source_state == SourceState::Unavailable {
        t!("view.backup_history.summary.unavailable")
    } else if qualified.points.is_empty() {
        t!("view.backup_history.summary.empty")
    } else {
        t!("view.backup_history.summary.recorded", count = format::digits(qualified.points.len().to_string()))
    };
    let limitations = limitations(qualified.scope, qualified.coverage, evidence.read_complete);
    let entries = qualified.points.into_iter().map(|(row, point)| entry(row, point)).collect();
    Ok(BackupHistoryReport {
        context: qualified.context,
        scope: qualified.scope.clone(),
        source: qualified.coverage.clone(),
        source_state,
        read_complete: evidence.read_complete,
        title: t!("view.backup_history.title"),
        summary,
        limitations,
        entries,
    })
}
struct QualifiedEvidence<'a> {
    context: NativeReadContext,
    scope: &'a MssqlBackupHistoryScope,
    coverage: &'a StateSourceCoverage,
    points: Vec<(&'a OperationalObservation, &'a MssqlBackupHistoryObservation)>,
}
fn validate<'a>(evidence: &BackupHistoryEvidence<'a>) -> Result<QualifiedEvidence<'a>> {
    let context = backup_history_context(evidence.source)?;
    let SourceObservation::BackupHistory { scope, snapshot, .. } = &evidence.source.observation else {
        return Err(invalid());
    };
    let [coverage] = snapshot.sources.as_slice() else {
        return Err(invalid());
    };
    let mut points = Vec::new();
    let mut keys = BTreeSet::new();
    let mut positions = BTreeSet::new();
    for row in evidence.observations {
        let OperationalFact::MssqlBackupHistory(point) = &row.fact else {
            return Err(invalid());
        };
        if !row.is_valid()
            || !point.fits_source(evidence.source)
            || coverage.state != SourceState::Partial
            || !keys.insert(&row.key)
            || !positions.insert(point.position)
        {
            return Err(invalid());
        }
        points.push((*row, point.as_ref()));
    }
    if points.len() > coverage.records as usize
        || evidence.read_complete && points.len() != coverage.records as usize
        || !backup_history_roster_is_unique(points.iter().map(|(_, point)| &point.record))
    {
        return Err(invalid());
    }
    points.sort_by_key(|(_, point)| point.position);
    Ok(QualifiedEvidence { context, scope, coverage, points })
}
fn limitations(scope: &MssqlBackupHistoryScope, coverage: &StateSourceCoverage, read_complete: bool) -> Vec<String> {
    let mut words = vec![
        t!("view.backup_history.limit.retained"),
        t!("view.backup_history.limit.clocks"),
        t!("view.backup_history.limit.recoverability"),
    ];
    match scope.profile {
        MssqlBackupHistoryProfile::AzureSqlDatabase => words.push(match scope.hyperscale {
            Some(true) => t!("view.backup_history.limit.hyperscale"),
            None => t!("view.backup_history.limit.deployment"),
            Some(false) => t!("view.backup_history.limit.azure_retention"),
        }),
        MssqlBackupHistoryProfile::SqlServerMsdb | MssqlBackupHistoryProfile::ManagedInstanceMsdb => {
            words.extend([
                t!("view.backup_history.limit.msdb"),
                t!("view.backup_history.limit.time_zone"),
                t!("view.backup_history.limit.initiator"),
            ]);
            if scope.profile == MssqlBackupHistoryProfile::ManagedInstanceMsdb {
                words.push(t!("view.backup_history.limit.mi"));
            }
        }
    }
    if !read_complete || coverage.stopped == Some(CollectionStop::RowLimit) {
        words.push(t!("view.backup_history.limit.partial"));
    }
    if let Some(stopped) = coverage.stopped {
        words.push(stopped.error().message);
    }
    if coverage.error_code == Some(ErrorCode::PermissionDenied) {
        words.push(t!("view.backup_history.limit.permission"));
    }
    words
}
fn entry(row: &OperationalObservation, point: &MssqlBackupHistoryObservation) -> BackupHistoryEntry {
    let title = match point.record.backup_type() {
        Some(MssqlBackupType::Database) => t!("view.backup_history.entry.database"),
        Some(MssqlBackupType::DifferentialDatabase) => t!("view.backup_history.entry.differential"),
        Some(MssqlBackupType::Log) => t!("view.backup_history.entry.log"),
        Some(MssqlBackupType::File | MssqlBackupType::DifferentialFile) => t!("view.backup_history.entry.file"),
        Some(MssqlBackupType::Partial | MssqlBackupType::DifferentialPartial) => {
            t!("view.backup_history.entry.partial")
        }
        None => t!("view.backup_history.entry.unknown"),
    };
    let explanation = if matches!(&point.record,MssqlBackupRecord::Msdb(backup) if backup.flags.damaged==Some(true)) {
        t!("view.backup_history.entry.damaged")
    } else {
        t!("view.backup_history.entry.recorded")
    };
    BackupHistoryEntry {
        observation_key: row.key.clone(),
        observed_at: point.observed_at.clone(),
        native: point.record.clone(),
        title,
        explanation,
        investigation: t!("view.backup_history.investigate"),
        verify: t!("view.backup_history.verify"),
    }
}
fn invalid() -> culprit_core::Error {
    CollectionStop::InvalidPage.error()
}
#[cfg(test)]
mod tests;
