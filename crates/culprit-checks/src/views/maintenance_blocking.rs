//! Compose two existing explanations without treating a later native graph as historical lock ownership.
use culprit_core::{
    MaintenanceBlockingReport, Result, maintenance_blocking_report::maintenance_blocking_focus, t, telemetry::*,
};

/// Storage authentication stays with the reader. The optional receipt must identify the exact retained point.
pub struct MaintenanceProgressEvidence<'a> {
    pub source: &'a CollectionDescription,
    pub snapshot: &'a MaintenanceProgressSnapshot,
    pub observation: Option<&'a DiagnosticEvidenceRef>,
    pub read_complete: bool,
}

pub fn explain(
    progress: MaintenanceProgressEvidence<'_>,
    blocking: &ActivityFrameEvidence<'_>,
) -> Result<MaintenanceBlockingReport> {
    let focus = maintenance_blocking_focus(progress.source, blocking.source())?;
    validate_progress(&progress)?;
    if blocking.rows().any(|row| row.reference.is_none()) {
        return Err(invalid());
    }
    let maintenance = super::maintenance_progress::explain(progress.snapshot, progress.read_complete)?;
    let rows = blocking.rows().collect::<Vec<_>>();
    let closure = super::focused_blocking::explain(blocking.source(), &rows, blocking.read_complete())?;
    let mut limitations = vec![
        t!("view.maintenance_blocking.limit.separate"),
        t!("view.maintenance_blocking.limit.owner"),
        t!("view.maintenance_blocking.limit.outcome"),
    ];
    if maintenance.binding == FocusedRequestBinding::UnverifiedOriginalScope
        || closure.binding == FocusedRequestBinding::UnverifiedOriginalScope
    {
        limitations.push(t!("view.blocking.limit.original_scope"));
    }
    Ok(MaintenanceBlockingReport {
        original: focus.parent.clone(),
        maintenance_observation: progress.observation.cloned(),
        maintenance: Box::new(maintenance),
        blocking: Box::new(closure),
        title: t!("view.maintenance_blocking.title"),
        summary: t!("view.maintenance_blocking.summary"),
        limitations,
    })
}

fn validate_progress(progress: &MaintenanceProgressEvidence<'_>) -> Result<()> {
    let SourceObservation::MaintenanceProgress { snapshot, result } = &progress.source.observation else {
        return Err(invalid());
    };
    let [coverage] = snapshot.sources.as_slice() else { return Err(invalid()) };
    if !progress.snapshot.is_valid_read(progress.read_complete)
        || progress.snapshot.source != coverage.source
        || progress.snapshot.context != result.read_context(progress.source, snapshot)
        || progress.snapshot.coverage != *coverage
        || progress.snapshot.result != **result
    {
        return Err(invalid());
    }
    match (progress.snapshot.observations.as_slice(), progress.observation) {
        ([], None) => Ok(()),
        ([point], Some(reference)) => {
            if !point.fits_source_scope(progress.source) || reference.row_ordinal != 0 {
                return Err(invalid());
            }
            reference.validate_canonical_record(
                progress.source,
                &TelemetryRecord::Operational(OperationalObservation {
                    source: progress.snapshot.source.clone(),
                    key: point.key()?,
                    fact: OperationalFact::MaintenanceProgress(Box::new(point.clone())),
                }),
            )?;
            Ok(())
        }
        _ => Err(invalid()),
    }
}

fn invalid() -> culprit_core::Error {
    CollectionStop::InvalidPage.error()
}

#[cfg(test)]
mod tests;
