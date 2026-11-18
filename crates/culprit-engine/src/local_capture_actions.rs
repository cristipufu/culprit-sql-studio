//! An explicitly installed local owner supplies originals without granting native collection rights.
use crate::{LocalCaptureReader, LocalCaptureRequest, LocalCapturedAnalysis, LocalMemoryClerks, Workspace};
use culprit_core::{Report, Result, SnapshotOptions, telemetry::DeliveryProblem};
use std::path::Path;

impl Workspace {
    /// A saved connection authorizes lookup, while the injected reader authenticates the capture's journal owner.
    /// The request's operation includes queueing and local verification; this facade creates no replacement owner.
    pub async fn analyze_local_capture(
        &self,
        reader: &dyn LocalCaptureReader,
        request: LocalCaptureRequest<'_>,
    ) -> Result<LocalCapturedAnalysis> {
        self.store.get(request.connection)?;
        let connection = request.connection.to_owned();
        let requested = request.analysis;
        let result = reader.analyze(request).await?;
        self.store.get(&connection)?;
        if result.kind() != requested {
            return Err(DeliveryProblem::InvalidContract.error());
        }
        Ok(result)
    }

    /// Attach the exact original database capture only after the ordinary native scan has settled.
    pub async fn scan_with_captured_hyperscale_log_components(
        &self,
        name: &str,
        options: &SnapshotOptions,
        query_log: Option<&Path>,
        captured: crate::LocalHyperscaleLogComponents,
    ) -> Result<Report> {
        if captured.original().connection() != name {
            return Err(DeliveryProblem::InvalidContract.error());
        }
        let evidence = crate::scan::CapturedScanEvidence::hyperscale_log_components(captured.into_read())?;
        Box::pin(self.scan_observation(name, options, query_log, Some(evidence)))
            .await
            .map(|reports| *reports.requested)
    }
    /// Consume one opaque authenticated result, retaining all original rows for normal persistence/reanalysis.
    /// Its shared-server scope and historical clocks remain independent of the scan's database and populations.
    pub async fn scan_with_captured_memory_clerks(
        &self,
        name: &str,
        options: &SnapshotOptions,
        query_log: Option<&Path>,
        captured: LocalMemoryClerks,
    ) -> Result<Report> {
        if captured.original().connection() != name {
            return Err(DeliveryProblem::InvalidContract.error());
        }
        let evidence = crate::scan::CapturedScanEvidence::memory_clerks(captured.into_read())?;
        Box::pin(self.scan_observation(name, options, query_log, Some(evidence)))
            .await
            .map(|reports| *reports.requested)
    }
}
