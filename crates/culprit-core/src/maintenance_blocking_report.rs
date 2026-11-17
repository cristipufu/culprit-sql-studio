//! Separate native maintenance and blocking reads retain their own evidence and never reconstruct an old owner.
use crate::{BlockingReport, MaintenanceProgressReport, Result, telemetry::*};
use serde::{Deserialize, Serialize};
use ts_rs::TS;

#[derive(Serialize, Deserialize, TS, Clone, Debug)]
#[ts(export)]
pub struct MaintenanceBlockingReport {
    pub original: DiagnosticEvidenceRef,
    /// Absent when the original point was not observed or its warehouse row is missing.
    pub maintenance_observation: Option<DiagnosticEvidenceRef>,
    pub maintenance: Box<MaintenanceProgressReport>,
    pub blocking: Box<BlockingReport>,
    pub title: String,
    pub summary: String,
    pub limitations: Vec<String>,
}

/// Structural linkage only: callers authenticate both source descriptors and their original receipts separately.
/// Native read bounds establish order; collector or delivery timestamps cannot replace them.
pub fn maintenance_blocking_focus<'a>(
    maintenance: &'a CollectionDescription,
    blocking: &CollectionDescription,
) -> Result<&'a ActivityRequestFocus> {
    let SourceObservation::MaintenanceProgress { snapshot: earlier, result: progress } = &maintenance.observation
    else {
        return Err(invalid());
    };
    let SourceObservation::FocusedBlocking { snapshot: later, result: closure } = &blocking.observation else {
        return Err(invalid());
    };
    let original = progress.focus.as_deref().ok_or_else(invalid)?;
    if !maintenance.observation.covers_request()
        || !blocking.observation.covers_request()
        || !maintenance.partition_is_consistent()
        || !blocking.partition_is_consistent()
        || !progress.is_valid_source(maintenance, earlier)
        || !closure.is_valid_source(blocking, later)
        || original != &closure.focus
        || maintenance.target.telemetry_id()? != blocking.target.telemetry_id()?
        || maintenance.partition != blocking.partition
        || progress.database_writable != later.database_writable
        || ObservationBounds::new(&earlier.observed_until, &later.at).is_none()
    {
        return Err(invalid());
    }
    Ok(original)
}

fn invalid() -> crate::Error {
    CollectionStop::InvalidPage.error()
}
