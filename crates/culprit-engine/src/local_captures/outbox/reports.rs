//! Family adapters borrow the one canonical original read; native acquisition and local ownership are elsewhere.
use super::*;
use culprit_checks::{compilation::CompilationEvidence, performance_counters::PerformanceCounterEvidence};
use culprit_core::*;

pub(super) fn explain(kind: LocalCaptureAnalysisKind, read: &CapturedOperationalRead) -> Result<LocalSnapshotReport> {
    if !read.is_valid() {
        return Err(invalid());
    }
    let source = &read.description;
    let header = source.observation.snapshot().ok_or_else(invalid)?;
    if kind.snapshot_dataset() != Some(header.dataset) {
        return Err(DeliveryProblem::UnsupportedSchema.error());
    }
    let context = read.context()?;
    match kind {
        LocalCaptureAnalysisKind::PerformanceCounters => {
            let coverage = required_source(read, culprit_core::source_id::PERFORMANCE_COUNTERS)?;
            let rows = rows_for(read, coverage);
            Ok(LocalSnapshotReport::PerformanceCounters(Box::new(culprit_checks::performance_counters::explain(
                PerformanceCounterEvidence::Captured {
                    context: &context,
                    coverage,
                    observations: &rows,
                    read_complete: read.read_complete,
                },
            )?)))
        }
        LocalCaptureAnalysisKind::DatabaseProperties => {
            required_source(read, DATABASE_PROPERTIES_SOURCE)?;
            let native = DatabasePropertiesRead::from_source(source, read.observations.clone(), read.read_complete)?;
            Ok(LocalSnapshotReport::DatabaseProperties(Box::new(culprit_checks::database_properties::explain(
                &native,
            )?)))
        }
        LocalCaptureAnalysisKind::Compilation => {
            let (coverage, rows) = single_source(read)?;
            let outcome = crate::collected_resources::read_outcome(coverage, rows.len(), read.read_complete)?;
            Ok(LocalSnapshotReport::Compilation(Box::new(culprit_checks::compilation::explain(CompilationEvidence {
                context: Some(&context),
                observations: &rows,
                source: Some(&outcome),
                definitions: None,
            })?)))
        }
        LocalCaptureAnalysisKind::ConfiguredLogRates => {
            let coverage = required_source(read, CONFIGURED_LOG_RATES_SOURCE)?;
            let point = match rows_for(read, coverage).as_slice() {
                [] => None,
                [row] => match &row.fact {
                    OperationalFact::ConfiguredLogRateLimits(point) => Some(point.as_ref().clone()),
                    _ => return Err(invalid()),
                },
                _ => return Err(invalid()),
            };
            let native = ConfiguredLogRateRead::from_source(source, point)?;
            Ok(LocalSnapshotReport::ConfiguredLogRates(Box::new(culprit_checks::configured_log_rates::explain(
                &native,
                read.read_complete,
            )?)))
        }
        LocalCaptureAnalysisKind::ConnectionOutcomes => {
            let native = ConnectionOutcomeSnapshot::from_source(source, read.observations.clone())?;
            Ok(LocalSnapshotReport::ConnectionOutcomes(Box::new(culprit_checks::connection_outcomes::explain(
                &native,
                read.read_complete,
            )?)))
        }
        LocalCaptureAnalysisKind::Jobs => {
            let native = TimescaleJobsSnapshot::from_source(source, read.observations.clone())?;
            Ok(LocalSnapshotReport::Jobs(Box::new(culprit_checks::jobs::explain(&native, read.read_complete)?)))
        }
        LocalCaptureAnalysisKind::StatementWork => {
            let native = StatementCounterSnapshot::from_source(source, read.observations.clone())?;
            Ok(LocalSnapshotReport::StatementWork(Box::new(culprit_checks::statement_work::explain(
                &native,
                read.read_complete,
            )?)))
        }
        LocalCaptureAnalysisKind::RequestMemory => request_memory(read, &context),
        LocalCaptureAnalysisKind::TaskSpace => task_space(read),
        LocalCaptureAnalysisKind::CurrentRequestWork => current_request_work(read),
        LocalCaptureAnalysisKind::MaintenanceProgress => maintenance_progress(read),
        LocalCaptureAnalysisKind::CompletedSpills => {
            let native = CachedRuntimeRead {
                origin: CachedRuntimeReadOrigin::Captured {
                    capture: read.origin.capture.clone(),
                    source_sha256: read.origin.source_sha256.clone(),
                    positions: read.origin.positions.clone(),
                },
                description: source.clone(),
                observations: read.observations.clone(),
                read_complete: read.read_complete,
            };
            Ok(LocalSnapshotReport::CompletedSpills(Box::new(culprit_checks::completed_spills::explain_read(&native)?)))
        }
        LocalCaptureAnalysisKind::Storage => {
            Ok(LocalSnapshotReport::Storage(Box::new(culprit_checks::storage::explain_captured(read)?)))
        }
        LocalCaptureAnalysisKind::PageServerReads
        | LocalCaptureAnalysisKind::MemoryClerks
        | LocalCaptureAnalysisKind::HyperscaleLogComponents => Err(DeliveryProblem::UnsupportedSchema.error()),
    }
}

fn required_source<'a>(read: &'a CapturedOperationalRead, name: &str) -> Result<&'a StateSourceCoverage> {
    read.source_coverage(name).ok_or_else(|| DeliveryProblem::UnsupportedSchema.error())
}
fn rows_for<'a>(read: &'a CapturedOperationalRead, source: &StateSourceCoverage) -> Vec<&'a OperationalObservation> {
    read.observations.iter().filter(|row| row.source == source.source).collect()
}
fn single_source(read: &CapturedOperationalRead) -> Result<(&StateSourceCoverage, Vec<&OperationalObservation>)> {
    let header = read.description.observation.snapshot().ok_or_else(invalid)?;
    let [coverage] = header.sources.as_slice() else { return Err(invalid()) };
    Ok((coverage, rows_for(read, coverage)))
}

fn request_memory(read: &CapturedOperationalRead, context: &NativeReadContext) -> Result<LocalSnapshotReport> {
    let (coverage, rows) = single_source(read)?;
    let observations = rows
        .iter()
        .map(|row| {
            let OperationalFact::ExecutionGrant(point) = &row.fact else { return Err(invalid()) };
            Ok(point.as_ref())
        })
        .collect::<Result<Vec<_>>>()?;
    let source = crate::collected_resources::read_outcome(coverage, observations.len(), read.read_complete)?;
    let report = culprit_checks::request_memory::explain(
        culprit_checks::request_memory::RequestMemoryEvidence { context, observations: &observations },
        Some(&source),
    )?;
    Ok(LocalSnapshotReport::RequestMemory(Box::new(report)))
}

fn task_space(read: &CapturedOperationalRead) -> Result<LocalSnapshotReport> {
    let (coverage, rows) = single_source(read)?;
    let context = read.context()?;
    let database = context.target.database().ok_or_else(invalid)?;
    let bounds = ObservationBounds::new(&context.observed_from, &context.observed_until).ok_or_else(invalid)?;
    let observations = rows
        .iter()
        .map(|row| {
            let OperationalFact::TaskSpace(point) = &row.fact else { return Err(invalid()) };
            if !bounds.contains(&point.observed_at)
                || [&point.before_check, &point.after_check]
                    .into_iter()
                    .flatten()
                    .any(|check| !bounds.contains(&check.observed_at))
                || !point.fits_database_capture(&database.database)
            {
                return Err(invalid());
            }
            Ok(point.as_ref())
        })
        .collect::<Result<Vec<_>>>()?;
    let source = crate::collected_resources::read_outcome(coverage, observations.len(), read.read_complete)?;
    Ok(LocalSnapshotReport::TaskSpace(Box::new(culprit_checks::task_space::explain(&observations, Some(&source))?)))
}

fn current_request_work(read: &CapturedOperationalRead) -> Result<LocalSnapshotReport> {
    let source = &read.description;
    let SourceObservation::CurrentRequestWork { profile, snapshot: header, result } = &source.observation else {
        return Err(invalid());
    };
    if !result.is_valid_source(source, header) {
        return Err(invalid());
    }
    let (coverage, rows) = single_source(read)?;
    let observations = rows
        .iter()
        .map(|row| {
            let OperationalFact::CurrentRequestWork(point) = &row.fact else { return Err(invalid()) };
            if point.key()? != row.key || !point.fits_source_scope(source) {
                return Err(invalid());
            }
            Ok(point.as_ref().clone())
        })
        .collect::<Result<Vec<_>>>()?;
    let snapshot = CurrentRequestWorkSnapshot {
        profile: *profile,
        source: coverage.source.clone(),
        context: result.read_context(source, header),
        coverage: coverage.clone(),
        result: result.as_ref().clone(),
        observations,
    };
    Ok(LocalSnapshotReport::CurrentRequestWork(Box::new(culprit_checks::current_request_work::explain(
        &snapshot,
        read.read_complete,
    )?)))
}

fn maintenance_progress(read: &CapturedOperationalRead) -> Result<LocalSnapshotReport> {
    let source = &read.description;
    let SourceObservation::MaintenanceProgress { snapshot: header, result } = &source.observation else {
        return Err(invalid());
    };
    if !result.is_valid_source(source, header) {
        return Err(invalid());
    }
    let (coverage, rows) = single_source(read)?;
    let observations = rows
        .iter()
        .map(|row| {
            let OperationalFact::MaintenanceProgress(point) = &row.fact else { return Err(invalid()) };
            if point.key()? != row.key || !point.fits_source_scope(source) {
                return Err(invalid());
            }
            Ok(point.as_ref().clone())
        })
        .collect::<Result<Vec<_>>>()?;
    let snapshot = MaintenanceProgressSnapshot {
        source: coverage.source.clone(),
        context: result.read_context(source, header),
        coverage: coverage.clone(),
        result: result.as_ref().clone(),
        observations,
    };
    Ok(LocalSnapshotReport::MaintenanceProgress(Box::new(culprit_checks::maintenance_progress::explain(
        &snapshot,
        read.read_complete,
    )?)))
}
