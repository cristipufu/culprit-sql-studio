//! Independently authenticated originals attach once, after the scan's native owner has settled.
use culprit_core::{
    Result, Snapshot,
    telemetry::{CapturedOperationalRead, DeliveryProblem, memory_clerk_candidate},
};

#[cfg(feature = "collector")]
use culprit_core::telemetry::{CachedRuntimeRead, CachedRuntimeReadOrigin};

pub(crate) enum CapturedScanEvidence {
    #[cfg(feature = "collector")]
    CachedRuntime(Box<CachedRuntimeRead>),
    MemoryClerks(Box<CapturedOperationalRead>),
    TempdbMetadataMemory(Box<CapturedOperationalRead>),
    TableMemory(Box<CapturedOperationalRead>),
    GroupReplication(Box<CapturedOperationalRead>),
    FullTextPopulations(Box<CapturedOperationalRead>),
    DatabasePerformanceCounters(Box<CapturedOperationalRead>),
    HyperscaleLogComponents(Box<CapturedOperationalRead>),
}
impl CapturedScanEvidence {
    pub(crate) fn database_performance_counters(read: Box<CapturedOperationalRead>) -> Result<Self> {
        culprit_checks::performance_counters::explain_database_captured(&read)?;
        Ok(Self::DatabasePerformanceCounters(read))
    }

    pub(crate) fn group_replication(read: Box<CapturedOperationalRead>) -> Result<Self> {
        culprit_checks::group_replication::explain_captured(&read)?;
        Ok(Self::GroupReplication(read))
    }

    pub(crate) fn table_memory(read: Box<CapturedOperationalRead>) -> Result<Self> {
        culprit_checks::table_memory::explain_captured(&read)?;
        Ok(Self::TableMemory(read))
    }
    pub(crate) fn full_text_populations(read: Box<CapturedOperationalRead>) -> Result<Self> {
        culprit_checks::full_text_populations::explain_captured(&read)?;
        Ok(Self::FullTextPopulations(read))
    }

    pub(crate) fn tempdb_metadata_memory(read: Box<CapturedOperationalRead>) -> Result<Self> {
        culprit_checks::tempdb_metadata_memory::explain_captured(&read)?;
        Ok(Self::TempdbMetadataMemory(read))
    }

    pub(crate) fn memory_clerks(read: Box<CapturedOperationalRead>) -> Result<Self> {
        // The pure view is the one owner of clerk-family admission; keep malformed originals out before scanning.
        culprit_checks::memory_clerks::explain_captured(&read)?;
        Ok(Self::MemoryClerks(read))
    }

    pub(crate) fn hyperscale_log_components(read: Box<CapturedOperationalRead>) -> Result<Self> {
        culprit_checks::hyperscale_log_components::explain_captured(&read)?;
        Ok(Self::HyperscaleLogComponents(read))
    }
    pub(crate) fn attach(self, snapshot: &mut Snapshot) -> Result<()> {
        match self {
            #[cfg(feature = "collector")]
            Self::CachedRuntime(read) => {
                if snapshot.operations.cached_runtime.is_some() {
                    return Err(DeliveryProblem::Conflict.error());
                }
                if !matches!(read.origin, CachedRuntimeReadOrigin::Captured { .. })
                    || !read.is_valid()
                    || !read.matches_native_database(snapshot)
                {
                    return Err(DeliveryProblem::InvalidContract.error());
                }
                snapshot.operations.cached_runtime = Some(read);
            }
            Self::HyperscaleLogComponents(read) => {
                if snapshot.operations.hyperscale_log_components.is_some() {
                    return Err(DeliveryProblem::Conflict.error());
                }
                if !read.is_valid() || !read.description.target.matches_native_database(snapshot) {
                    return Err(DeliveryProblem::InvalidContract.error());
                }
                snapshot.operations.hyperscale_log_components = Some(read);
            }
            Self::DatabasePerformanceCounters(read) => {
                if snapshot.operations.database_performance_counters.is_some() {
                    return Err(DeliveryProblem::Conflict.error());
                }
                // The saved connection authorizes attachment, not continuity with the current native member/DB.
                // The original is independently scoped and never enters shared counters or workload grading.
                snapshot.operations.database_performance_counters =
                    Some(culprit_core::telemetry::RecordedPerformanceCounters::Captured { read });
            }
            Self::GroupReplication(read) => {
                // Exact saved connection authorizes this historical attachment; no current route/member join.
                culprit_core::telemetry::append_recorded_operational_read(
                    &mut snapshot.operations.group_replication,
                    read,
                )?;
            }
            Self::TableMemory(read) => {
                // Saved-connection admission does not establish a match to current scan tables or native members.
                culprit_core::telemetry::append_recorded_operational_read(&mut snapshot.operations.table_memory, read)?;
            }
            Self::FullTextPopulations(read) => {
                // Saved-connection admission does not establish a match to current scan tables or native members.
                culprit_core::telemetry::append_recorded_operational_read(
                    &mut snapshot.operations.full_text_populations,
                    read,
                )?;
            }
            Self::TempdbMetadataMemory(read) => {
                if snapshot.operations.tempdb_metadata_memory.is_some() {
                    return Err(DeliveryProblem::Conflict.error());
                }
                // Keep the authenticated shared native owner, including an unknown current scan identity.
                snapshot.operations.tempdb_metadata_memory = Some(read);
            }
            Self::MemoryClerks(read) => {
                if snapshot.operations.memory_clerks.is_some()
                    || snapshot.pressure.as_ref().is_some_and(|pressure| {
                        pressure
                            .resource_counters
                            .as_deref()
                            .is_some_and(|sets| sets.iter().any(memory_clerk_candidate))
                    })
                {
                    return Err(DeliveryProblem::Conflict.error());
                }
                // The original SharedInstance describes historical server allocations. A current database scan
                // supplies no matching member proof and cannot turn this into database-local/current evidence.
                snapshot.operations.memory_clerks = Some(read);
            }
        }
        Ok(())
    }
}
