//! An explicitly installed local owner supplies originals without granting native collection rights.
use crate::{LocalCaptureReader, LocalCaptureRequest, LocalCapturedAnalysis, LocalMemoryClerks, Workspace};
use culprit_core::{Report, Result, SnapshotOptions, telemetry::DeliveryProblem};
use std::path::Path;

impl Workspace {
    pub async fn scan_with_captured_numa_nodes(
        &self,
        name: &str,
        options: &SnapshotOptions,
        query_log: Option<&Path>,
        captured: crate::LocalSnapshotAnalysis,
    ) -> Result<Report> {
        if captured.original().connection() != name
            || captured.report().kind() != crate::LocalCaptureAnalysisKind::InstanceResources
        {
            return Err(DeliveryProblem::InvalidContract.error());
        }
        let evidence = crate::scan::CapturedScanEvidence::numa_nodes(captured.into_read())?;
        Box::pin(self.scan_observation(name, options, query_log, Some(evidence)))
            .await
            .map(|reports| *reports.requested)
    }

    /// Retain one authenticated catalog original after the ordinary scan settles, independently of current objects.
    pub async fn scan_with_captured_index_configuration(
        &self,
        name: &str,
        options: &SnapshotOptions,
        query_log: Option<&Path>,
        captured: crate::LocalSnapshotAnalysis,
    ) -> Result<Report> {
        if captured.original().connection() != name
            || captured.report().kind() != crate::LocalCaptureAnalysisKind::IndexConfiguration
        {
            return Err(DeliveryProblem::InvalidContract.error());
        }
        let evidence = crate::scan::CapturedScanEvidence::index_configuration(captured.into_read())?;
        Box::pin(self.scan_observation(name, options, query_log, Some(evidence)))
            .await
            .map(|reports| *reports.requested)
    }

    /// Attach the exact selected native runtime original after normal scan shutdown. It cannot replace the
    /// independently retained wait original or establish a relationship to current query IDs, members or values.
    pub async fn scan_with_captured_runtime_fragments(
        &self,
        name: &str,
        options: &SnapshotOptions,
        query_log: Option<&Path>,
        captured: crate::LocalRuntimeFragments,
    ) -> Result<Report> {
        if captured.connection() != name {
            return Err(DeliveryProblem::InvalidContract.error());
        }
        let evidence = crate::scan::CapturedScanEvidence::runtime_fragments(captured.into_read())?;
        Box::pin(self.scan_observation(name, options, query_log, Some(evidence)))
            .await
            .map(|reports| *reports.requested)
    }

    /// Explicit original history stays separate from the later scan's database/member and workload.
    pub async fn scan_with_captured_change_history(
        &self,
        name: &str,
        options: &SnapshotOptions,
        query_log: Option<&Path>,
        captured: crate::LocalSnapshotAnalysis,
    ) -> Result<Report> {
        if captured.original().connection() != name {
            return Err(DeliveryProblem::InvalidContract.error());
        }
        let family = match captured.report().kind() {
            crate::LocalCaptureAnalysisKind::Cdc => culprit_core::telemetry::MssqlChangeCaptureFamily::Cdc,
            crate::LocalCaptureAnalysisKind::ChangeFeed => {
                culprit_core::telemetry::MssqlChangeCaptureFamily::ChangeFeed
            }
            _ => return Err(DeliveryProblem::InvalidContract.error()),
        };
        let evidence = crate::scan::CapturedScanEvidence::change_capture(family, captured.into_read())?;
        Box::pin(self.scan_observation(name, options, query_log, Some(evidence)))
            .await
            .map(|reports| *reports.requested)
    }

    /// Reinterpret one selected original history capture after ordinary scan shutdown. No current query/table,
    /// routing member or database name supplies a relationship to that independent historical population.
    pub async fn scan_with_captured_query_wait_fragments(
        &self,
        name: &str,
        options: &SnapshotOptions,
        query_log: Option<&Path>,
        captured: crate::LocalQueryWaitFragments,
    ) -> Result<Report> {
        if captured.connection() != name {
            return Err(DeliveryProblem::InvalidContract.error());
        }
        let evidence = crate::scan::CapturedScanEvidence::query_wait_fragments(captured.into_read())?;
        Box::pin(self.scan_observation(name, options, query_log, Some(evidence)))
            .await
            .map(|reports| *reports.requested)
    }

    /// An authenticated shared-pool original remains independent of the later scanned database.
    pub async fn scan_with_captured_pool_settings(
        &self,
        name: &str,
        options: &SnapshotOptions,
        query_log: Option<&Path>,
        captured: crate::LocalPoolSettings,
    ) -> Result<Report> {
        if captured.connection() != name {
            return Err(DeliveryProblem::InvalidContract.error());
        }
        let evidence = crate::scan::CapturedScanEvidence::pool_settings(captured.into_read())?;
        Box::pin(self.scan_observation(name, options, query_log, Some(evidence)))
            .await
            .map(|reports| *reports.requested)
    }
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

    /// Retain one authenticated selected-database original after the normal scan owner settles. Its native
    /// database, member, role and clocks remain historical; the current scan establishes none of them.
    pub async fn scan_with_captured_database_performance_counters(
        &self,
        name: &str,
        options: &SnapshotOptions,
        query_log: Option<&Path>,
        captured: crate::LocalSnapshotAnalysis,
    ) -> Result<Report> {
        if captured.original().connection() != name
            || captured.report().kind() != crate::LocalCaptureAnalysisKind::PerformanceCounters
        {
            return Err(DeliveryProblem::InvalidContract.error());
        }
        let evidence = crate::scan::CapturedScanEvidence::database_performance_counters(captured.into_read())?;
        Box::pin(self.scan_observation(name, options, query_log, Some(evidence)))
            .await
            .map(|reports| *reports.requested)
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
    /// Retain the original selected native table and acquisition interval after the normal scan owner ends.
    /// The saved connection authorizes lookup; no current table-name, database or member association is inferred.
    pub async fn scan_with_captured_table_memory(
        &self,
        name: &str,
        options: &SnapshotOptions,
        query_log: Option<&Path>,
        captured: crate::LocalSnapshotAnalysis,
    ) -> Result<Report> {
        if captured.original().connection() != name
            || captured.report().kind() != crate::LocalCaptureAnalysisKind::TableMemory
        {
            return Err(DeliveryProblem::InvalidContract.error());
        }
        let evidence = crate::scan::CapturedScanEvidence::table_memory(captured.into_read())?;
        Box::pin(self.scan_observation(name, options, query_log, Some(evidence)))
            .await
            .map(|reports| *reports.requested)
    }
    pub async fn scan_with_captured_full_text_populations(
        &self,
        name: &str,
        options: &SnapshotOptions,
        query_log: Option<&Path>,
        captured: crate::LocalSnapshotAnalysis,
    ) -> Result<Report> {
        if captured.original().connection() != name
            || captured.report().kind() != crate::LocalCaptureAnalysisKind::FullTextPopulations
        {
            return Err(DeliveryProblem::InvalidContract.error());
        }
        let evidence = crate::scan::CapturedScanEvidence::full_text_populations(captured.into_read())?;
        Box::pin(self.scan_observation(name, options, query_log, Some(evidence)))
            .await
            .map(|reports| *reports.requested)
    }

    /// Attach independently scoped original Group Replication evidence without a new native read.
    pub async fn scan_with_captured_group_replication(
        &self,
        name: &str,
        options: &SnapshotOptions,
        query_log: Option<&Path>,
        captured: crate::LocalSnapshotAnalysis,
    ) -> Result<Report> {
        if captured.original().connection() != name
            || captured.report().kind() != crate::LocalCaptureAnalysisKind::GroupReplication
        {
            return Err(DeliveryProblem::InvalidContract.error());
        }
        let evidence = crate::scan::CapturedScanEvidence::group_replication(captured.into_read())?;
        Box::pin(self.scan_observation(name, options, query_log, Some(evidence)))
            .await
            .map(|reports| *reports.requested)
    }

    /// Attach independently scoped original Group Replication evidence without a new native read.
    pub async fn scan_with_captured_galera(
        &self,
        name: &str,
        options: &SnapshotOptions,
        query_log: Option<&Path>,
        captured: crate::LocalSnapshotAnalysis,
    ) -> Result<Report> {
        if captured.original().connection() != name
            || captured.report().kind() != crate::LocalCaptureAnalysisKind::Galera
        {
            return Err(DeliveryProblem::InvalidContract.error());
        }
        let evidence = crate::scan::CapturedScanEvidence::galera(captured.into_read())?;
        Box::pin(self.scan_observation(name, options, query_log, Some(evidence)))
            .await
            .map(|reports| *reports.requested)
    }

    /// Attach one original shared tempdb read after the normal scan owner ends. Current database identity and
    /// host clocks neither qualify nor relabel this historical shared instance/process observation.
    pub async fn scan_with_captured_tempdb_metadata_memory(
        &self,
        name: &str,
        options: &SnapshotOptions,
        query_log: Option<&Path>,
        captured: crate::LocalSnapshotAnalysis,
    ) -> Result<Report> {
        if captured.original().connection() != name
            || captured.report().kind() != crate::LocalCaptureAnalysisKind::TempdbMetadataMemory
        {
            return Err(DeliveryProblem::InvalidContract.error());
        }
        let evidence = crate::scan::CapturedScanEvidence::tempdb_metadata_memory(captured.into_read())?;
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

impl Workspace {
    pub async fn scan_with_captured_managed_instance_resources(
        &self,
        name: &str,
        options: &SnapshotOptions,
        query_log: Option<&Path>,
        captured: crate::LocalManagedInstanceResources,
    ) -> Result<Report> {
        if captured.connection() != name {
            return Err(DeliveryProblem::InvalidContract.error());
        }
        let evidence = crate::scan::CapturedScanEvidence::managed_instance_resources(captured.into_read())?;
        Box::pin(self.scan_observation(name, options, query_log, Some(evidence)))
            .await
            .map(|reports| *reports.requested)
    }
}
