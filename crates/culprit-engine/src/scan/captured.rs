//! Independently authenticated originals attach once, after the scan's native owner has settled.
use culprit_core::{
    Result, Snapshot,
    telemetry::{CapturedOperationalRead, DeliveryProblem, memory_clerk_candidate},
};

#[cfg(feature = "collector")]
use culprit_core::telemetry::{CachedRuntimeRead, CachedRuntimeReadOrigin};

pub(crate) enum CapturedScanEvidence {
    ChangeCapture {
        family: culprit_core::telemetry::MssqlChangeCaptureFamily,
        read: Box<CapturedOperationalRead>,
    },
    RuntimeFragments(Box<culprit_core::telemetry::CapturedQueryHistoryRead>),
    QueryWaitFragments(Box<culprit_core::telemetry::CapturedQueryHistoryRead>),
    ManagedInstanceResources(Box<culprit_core::telemetry::CapturedManagedInstanceResourceRead>),
    #[cfg(feature = "collector")]
    CachedRuntime(Box<CachedRuntimeRead>),
    MemoryClerks(Box<CapturedOperationalRead>),
    TempdbMetadataMemory(Box<CapturedOperationalRead>),
    TableMemory(Box<CapturedOperationalRead>),
    GroupReplication(Box<CapturedOperationalRead>),
    Galera(Box<CapturedOperationalRead>),
    PoolSettings(Box<culprit_core::telemetry::CapturedPoolSettingsRead>),
    FullTextPopulations(Box<CapturedOperationalRead>),
    DatabasePerformanceCounters(Box<CapturedOperationalRead>),
    HyperscaleLogComponents(Box<CapturedOperationalRead>),
}
impl CapturedScanEvidence {
    pub(crate) fn change_capture(
        family: culprit_core::telemetry::MssqlChangeCaptureFamily,
        read: Box<CapturedOperationalRead>,
    ) -> Result<Self> {
        match family {
            culprit_core::telemetry::MssqlChangeCaptureFamily::Cdc => {
                culprit_checks::cdc::explain_captured(&read)?;
            }
            culprit_core::telemetry::MssqlChangeCaptureFamily::ChangeFeed => {
                culprit_checks::change_feed::explain_captured(&read)?;
            }
        }
        Ok(Self::ChangeCapture { family, read })
    }

    pub(crate) fn runtime_fragments(read: Box<culprit_core::telemetry::CapturedQueryHistoryRead>) -> Result<Self> {
        culprit_checks::query_runtime_fragments::explain(&read)?;
        Ok(Self::RuntimeFragments(read))
    }

    pub(crate) fn query_wait_fragments(read: Box<culprit_core::telemetry::CapturedQueryHistoryRead>) -> Result<Self> {
        culprit_checks::query_wait_fragments::explain(&read)?;
        Ok(Self::QueryWaitFragments(read))
    }

    pub(crate) fn managed_instance_resources(
        read: Box<culprit_core::telemetry::CapturedManagedInstanceResourceRead>,
    ) -> Result<Self> {
        culprit_checks::managed_instance_resources::explain_captured(&read)?;
        Ok(Self::ManagedInstanceResources(read))
    }
    pub(crate) fn pool_settings(read: Box<culprit_core::telemetry::CapturedPoolSettingsRead>) -> Result<Self> {
        culprit_checks::pool_settings::explain_captured(&read)?;
        Ok(Self::PoolSettings(read))
    }
    pub(crate) fn database_performance_counters(read: Box<CapturedOperationalRead>) -> Result<Self> {
        culprit_checks::performance_counters::explain_database_captured(&read)?;
        Ok(Self::DatabasePerformanceCounters(read))
    }

    pub(crate) fn group_replication(read: Box<CapturedOperationalRead>) -> Result<Self> {
        culprit_checks::group_replication::explain_captured(&read)?;
        Ok(Self::GroupReplication(read))
    }

    pub(crate) fn galera(read: Box<CapturedOperationalRead>) -> Result<Self> {
        culprit_checks::galera::explain_captured(&read)?;
        Ok(Self::Galera(read))
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
            Self::ChangeCapture { family, read } => {
                let originals = match family {
                    culprit_core::telemetry::MssqlChangeCaptureFamily::Cdc => &mut snapshot.operations.cdc_history,
                    culprit_core::telemetry::MssqlChangeCaptureFamily::ChangeFeed => {
                        &mut snapshot.operations.change_feed_history
                    }
                };
                culprit_core::telemetry::append_recorded_operational_read(originals, read)?;
            }

            Self::RuntimeFragments(read) => {
                culprit_checks::query_runtime_fragments::explain(&read)?;
                retain_history_original(&mut snapshot.operations.runtime_fragments, read)?;
            }
            Self::QueryWaitFragments(read) => {
                culprit_checks::query_wait_fragments::explain(&read)?;
                retain_history_original(&mut snapshot.operations.query_wait_fragments, read)?;
            }
            Self::ManagedInstanceResources(read) => {
                culprit_checks::managed_instance_resources::explain_captured(&read)?;
                if snapshot.operations.managed_instance_resources.as_deref().is_some_and(|original|!matches!(original,culprit_core::telemetry::RecordedManagedInstanceResourceRead::Captured {read:previous} if previous.as_ref()==read.as_ref())) {return Err(DeliveryProblem::Conflict.error())}
                snapshot.operations.managed_instance_resources =
                    Some(Box::new(culprit_core::telemetry::RecordedManagedInstanceResourceRead::Captured { read }));
            }
            Self::PoolSettings(read) => {
                culprit_checks::pool_settings::explain_captured(&read)?;
                if snapshot.operations.pool_settings.as_deref().is_some_and(|original| {
                    !matches!(
                        original,
                        culprit_core::telemetry::RecordedPoolSettingsRead::Captured { read: previous }
                            if previous.as_ref() == read.as_ref()
                    )
                }) {
                    return Err(DeliveryProblem::Conflict.error());
                }
                snapshot.operations.pool_settings =
                    Some(Box::new(culprit_core::telemetry::RecordedPoolSettingsRead::Captured { read }));
            }
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
            Self::Galera(read) => {
                // Exact saved connection authorizes this historical attachment; no current route/member join.
                culprit_core::telemetry::append_recorded_operational_read(&mut snapshot.operations.galera, read)?;
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

#[cfg(all(test, feature = "collector"))]
mod query_wait_tests;

#[cfg(all(test, feature = "collector"))]
mod change_capture_tests;
fn retain_history_original(
    slot: &mut Option<Box<culprit_core::telemetry::RecordedQueryHistoryRead>>,
    read: Box<culprit_core::telemetry::CapturedQueryHistoryRead>,
) -> Result<()> {
    use culprit_core::telemetry::RecordedQueryHistoryRead;
    if !read.is_valid() {
        return Err(DeliveryProblem::InvalidContract.error());
    }
    if slot.as_deref().is_some_and(|original|!matches!(original,RecordedQueryHistoryRead::Captured {read:before} if before.as_ref()==read.as_ref())) {return Err(DeliveryProblem::Conflict.error())}
    *slot = Some(Box::new(RecordedQueryHistoryRead::Captured { read }));
    Ok(())
}
