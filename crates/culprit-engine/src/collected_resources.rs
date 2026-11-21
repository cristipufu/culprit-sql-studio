//! Authenticated resource observations need neither a live provider nor invented query ownership.
use culprit_collector::analytical::evidence::{EvidenceCapture, EvidenceFamily};
use culprit_core::{
    Engine, Result, SourceOutcome, SourceState, collected_resource_analysis::CollectedRetainedFailureAnalysis,
    operations::MemoryFailureSnapshot, telemetry::*,
};
mod source_overhead;
pub use source_overhead::source_overhead;
mod state;
use state::CapturedState;
pub(crate) use state::read_outcome;
mod activity;
mod blocking;
pub(crate) mod references;
pub use blocking::blocking;
mod cached_plans;
pub use cached_plans::cached_plans;
mod cache_entry_memory;
pub use cache_entry_memory::cache_entry_memory;
mod backup_history;
pub use backup_history::backup_history;
mod cdc;
mod change_feed;
pub use change_feed::change_feed;
mod columnstore_rowgroups;
mod full_text_populations;
pub use full_text_populations::full_text_populations;
mod table_memory;
pub use columnstore_rowgroups::columnstore_rowgroups;
pub use table_memory::table_memory;
mod cleanup;
pub use cdc::cdc;
pub use cleanup::cleanup;
mod tuning;
pub use tuning::tuning;
mod compilation;
mod completed_compilation;
pub use completed_compilation::{CompletedCompilationAnalysis, completed_compilation};
mod completed_spills;
pub use compilation::compilation;
pub use completed_spills::completed_spills;
mod compiler_admission;
mod instance_resources;
pub use instance_resources::instance_resources;
pub(crate) mod table_full_text;
pub use table_full_text::table_full_text;
pub(crate) mod table_temporal;
pub use table_temporal::table_temporal;
mod database_properties;
mod suspect_pages;
pub use suspect_pages::suspect_pages;
mod memory_clerks;
mod tempdb_metadata_memory;
pub use database_properties::database_properties;
pub use memory_clerks::memory_clerks;
pub use tempdb_metadata_memory::tempdb_metadata_memory;
mod counter_ratios;
mod performance_counters;
pub use performance_counters::performance_counters;
mod wait_counters;
pub use compiler_admission::compiler_admission;
pub use counter_ratios::counter_ratios;
pub use wait_counters::wait_counters;
mod maintenance_progress;
pub use maintenance_progress::maintenance_progress;
mod maintenance_blocking;
pub use maintenance_blocking::maintenance_blocking;
mod current_request_work;
pub use current_request_work::current_request_work;
mod jobs;
pub use jobs::jobs;
mod replication;
pub(crate) use replication::explain_read as explain_replication_read;
pub use replication::replication;
mod configured_log_rates;
mod connection_outcomes;
mod hyperscale_log_components;
pub use configured_log_rates::configured_log_rates;
pub use connection_outcomes::connection_outcomes;
pub use hyperscale_log_components::hyperscale_log_components;
mod maintenance;
pub use maintenance::maintenance;
mod request_memory;
pub use request_memory::request_memory;
mod session_memory;
pub use session_memory::session_memory;
mod page_waits;
pub use page_waits::page_waits;
pub(crate) mod page_server_reads;
pub use page_server_reads::page_server_reads;
pub(crate) mod query_wait_fragments;
pub use query_wait_fragments::query_wait_fragments;
mod page_wait_writers;
pub use page_wait_writers::page_wait_writers;
mod page_task_holders;
pub use page_task_holders::page_task_holders;
mod statement_work;
pub use statement_work::statement_work;
mod statement_grants;
pub use statement_grants::statement_grants;
mod task_space;
pub use task_space::task_space;
mod runtime_distributions;
pub use runtime_distributions::runtime_distributions;

/// Positions identify captured rows, never incidents across observations. Native offset-free event time remains
/// distinct from UTC capture and delivery clocks.
pub fn memory_failures(capture: &EvidenceCapture) -> Result<CollectedRetainedFailureAnalysis> {
    let input = CapturedState::new(capture, SnapshotDataset::MemoryFailures)?;
    let resource = input.shared(&[SharedResourceKind::Instance, SharedResourceKind::Pool])?;
    if resource.engine != Engine::Mssql {
        return Err(DeliveryProblem::UnsupportedSchema.error());
    }
    let mut observations = input
        .rows()?
        .into_iter()
        .map(|observation| {
            let OperationalFact::MssqlOutOfMemory(failure) = &observation.fact else {
                return Err(invalid());
            };
            Ok(failure.as_ref().clone())
        })
        .collect::<Result<Vec<_>>>()?;
    observations.sort_by_key(|observation| observation.position);
    let source = input.outcome(observations.len())?;
    let report = {
        let evidence = MemoryFailureSnapshot {
            source: input.coverage.source.clone(),
            observed_from: input.native.observed_from.clone(),
            observed_until: input.native.observed_until.clone(),
            scope: capture.source().target.measurement_scope(),
            resource: Some(resource.clone()),
            observations,
        };
        culprit_checks::memory_failures::explain(&evidence, Some(&source))?
    };
    Ok(CollectedRetainedFailureAnalysis { context: crate::collected_context::from_capture(capture), report })
}

/// Only exact declared operational rows from this authenticated shared capture are admitted. Current sessions and
/// neighboring captures are never consulted to fill missing query, resource-owner or native-clock information.
pub fn native_failures(capture: &EvidenceCapture) -> Result<CollectedRetainedFailureAnalysis> {
    let input = CapturedState::new(capture, SnapshotDataset::NativeFailures)?;
    let resource = input.shared(&[SharedResourceKind::Instance])?;
    let mut observations = input
        .rows()?
        .into_iter()
        .map(|observation| match &observation.fact {
            OperationalFact::MysqlErrorLog(row) => Ok(NativeFailureRecord::MysqlErrorLog(row.as_ref().clone())),
            OperationalFact::PostgresLog(row) => Ok(NativeFailureRecord::PostgresLog(row.clone())),
            _ => Err(invalid()),
        })
        .collect::<Result<Vec<_>>>()?;
    observations.sort_by_key(NativeFailureRecord::position);
    let source = input.outcome(observations.len())?;
    let report = {
        let evidence = NativeFailureSnapshot {
            source: input.coverage.source.clone(),
            observed_from: input.native.observed_from.clone(),
            observed_until: input.native.observed_until.clone(),
            resource: Some(resource.clone()),
            observations,
        };
        culprit_checks::native_failures::explain(&evidence, Some(&source))?
    };
    Ok(CollectedRetainedFailureAnalysis { context: crate::collected_context::from_capture(capture), report })
}

fn invalid() -> culprit_core::Error {
    DeliveryProblem::Integrity.error()
}

mod query_store_definitions;

mod session_census;
mod storage;
pub use session_census::session_census;
pub use storage::storage;

mod group_replication;
pub use group_replication::group_replication;

mod galera;
pub use galera::galera;

mod pool_settings;
pub use pool_settings::pool_settings;

mod managed_instance_resources;
pub use managed_instance_resources::managed_instance_resources;
