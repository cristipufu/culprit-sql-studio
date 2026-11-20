//! Archive investigation resolves only query and recovery profiles; source databases and delivery stay out of scope.
use crate::{
    bounded_output,
    output::{Format, Out, observations},
};
use clap::{Args, Subcommand, ValueEnum};
use culprit_collector::{
    analytical::evidence::{EvidenceCapture, EvidenceFamily, EvidenceReadBudget, RecoveryPayloadBudget},
    capture::CollectionCancellation,
};
use culprit_core::{
    NativeRuntimePopulationCatalog, Result, format, t,
    telemetry::{DeliveryProblem, NativeIdentity, SourceObservation, valid_digest},
};
use culprit_engine::collector_service::{
    CollectorRuntime,
    evidence::{EvidenceConfig, EvidenceRequest, EvidenceService},
};
use serde::Serialize;
use std::{io::Write, path::PathBuf};
mod selection;

#[derive(Subcommand)]
pub(crate) enum EvidenceCommand {
    #[command(about=culprit_core::t!("evidence.cli.read"))]
    Read(ReadArgs),
    #[command(about=culprit_core::t!("evidence.cli.analyze"))]
    Analyze(AnalyzeArgs),
}

#[derive(Args)]
pub(crate) struct ReadArgs {
    #[command(flatten)]
    common: CommonArgs,
    #[arg(long="family",value_enum,required=true,value_delimiter=',',help=culprit_core::t!("evidence.cli.family"))]
    families: Vec<Family>,
}
#[derive(Args)]
pub(crate) struct AnalyzeArgs {
    #[command(flatten)]
    common: CommonArgs,
    #[arg(long,value_enum,help=culprit_core::t!("evidence.cli.analysis"))]
    analysis: Analysis,
    #[arg(long, value_name="ID", value_parser=crate::parse_workload_population, help=culprit_core::t!("evidence.cli.workload_population"))]
    workload_population: Option<NativeIdentity>,
    #[arg(long,value_name="SHA256",help=culprit_core::t!("evidence.cli.activity_range"))]
    activity_range: Option<String>,
    #[arg(long,value_name="SHA256",help=culprit_core::t!("evidence.cli.compilation_baseline"))]
    compilation_baseline: Option<String>,
    #[arg(long,value_name="SHA256",help=culprit_core::t!("evidence.cli.cache_baseline"))]
    cache_baseline: Option<String>,
    #[arg(long,value_name="SHA256",help=culprit_core::t!("evidence.cli.blocking_range"))]
    blocking_range: Option<String>,
    #[arg(long,value_name="SHA256",help=culprit_core::t!("evidence.cli.definitions_range"))]
    definitions_range: Option<String>,
}

impl AnalyzeArgs {
    fn validate_selection(&self) -> Result<()> {
        if let Some(population) = &self.workload_population {
            if !population.is_valid() {
                return Err(culprit_core::Error::invalid(t!("cli.scan.workload_population_invalid")));
            }
            if self.analysis != Analysis::Queries {
                return Err(culprit_core::Error::invalid(t!("evidence.cli.population_queries_only")));
            }
        }
        if let Some(range) = &self.activity_range {
            if self.analysis != Analysis::PageWaits {
                return Err(culprit_core::Error::invalid(t!("evidence.cli.activity_page_waits_only")));
            }
            if !valid_digest(range) || range == &self.common.range {
                return Err(DeliveryProblem::InvalidContract.error());
            }
        }
        if let Some(range) = &self.compilation_baseline {
            if self.analysis != Analysis::Compilation {
                return Err(culprit_core::Error::invalid(t!("evidence.cli.compilation_baseline_only")));
            }
            if !valid_digest(range) || range == &self.common.range {
                return Err(DeliveryProblem::InvalidContract.error());
            }
        }
        if let Some(range) = &self.cache_baseline {
            if self.analysis != Analysis::CacheEntryMemory {
                return Err(culprit_core::Error::invalid(t!("evidence.cli.cache_baseline_only")));
            }
            if !valid_digest(range) || range == &self.common.range {
                return Err(DeliveryProblem::InvalidContract.error());
            }
        }
        if let Some(range) = &self.blocking_range {
            if self.analysis != Analysis::MaintenanceBlocking {
                return Err(culprit_core::Error::invalid(t!("evidence.cli.blocking_range_only")));
            }
            if !valid_digest(range) || range == &self.common.range {
                return Err(DeliveryProblem::InvalidContract.error());
            }
        } else if self.analysis == Analysis::MaintenanceBlocking {
            return Err(culprit_core::Error::invalid(t!("evidence.cli.maintenance_blocking_required")));
        }
        if let Some(range) = &self.definitions_range {
            if selection::DefinitionKind::for_analysis(self.analysis).is_none() {
                return Err(culprit_core::Error::invalid(t!("evidence.cli.definitions_only")));
            }
            if !valid_digest(range)
                || range == &self.common.range && self.analysis != Analysis::PageServerReads
                || self.compilation_baseline.as_ref() == Some(range)
                || self.cache_baseline.as_ref() == Some(range)
            {
                return Err(DeliveryProblem::InvalidContract.error());
            }
        }
        Ok(())
    }
}

#[derive(Args)]
struct CommonArgs {
    #[arg(long,value_name="FILE",help=culprit_core::t!("evidence.cli.config"))]
    config: PathBuf,
    #[arg(long,value_name="SHA256",help=culprit_core::t!("evidence.cli.range"))]
    range: String,
    #[command(flatten)]
    budget: Budget,
    #[arg(long,default_value_t=bounded_output::DEFAULT_JSON_BYTES,help=culprit_core::t!("evidence.cli.max_output_bytes"))]
    max_output_bytes: usize,
}
#[derive(Args)]
struct Budget {
    #[arg(long,default_value_t=EvidenceReadBudget::default().page_rows,help=culprit_core::t!("evidence.cli.page_rows"))]
    page_rows: u32,
    #[arg(long,default_value_t=EvidenceReadBudget::default().page_bytes,help=culprit_core::t!("evidence.cli.page_bytes"))]
    page_bytes: u32,
    #[arg(long,default_value_t=EvidenceReadBudget::default().max_rows,help=culprit_core::t!("evidence.cli.max_rows"))]
    max_rows: u32,
    #[arg(long,default_value_t=EvidenceReadBudget::default().max_queries,help=culprit_core::t!("evidence.cli.max_queries"))]
    max_queries: u32,
    #[arg(long,default_value_t=EvidenceReadBudget::default().max_bytes,help=culprit_core::t!("evidence.cli.max_bytes"))]
    max_bytes: u64,
    #[arg(long,default_value_t=EvidenceReadBudget::default().max_proof_bytes,help=culprit_core::t!("evidence.cli.max_proof_bytes"))]
    max_proof_bytes: u64,
    #[arg(long,default_value_t=EvidenceReadBudget::default().timeout_ms,help=culprit_core::t!("evidence.cli.timeout"))]
    timeout_ms: u32,
    #[arg(long,help=culprit_core::t!("evidence.cli.with_recovery_payloads"))]
    with_recovery_payloads: bool,
    #[arg(long,requires="with_recovery_payloads",help=culprit_core::t!("evidence.cli.max_recovery_objects", default=format::count(RecoveryPayloadBudget::default().max_objects as f64)))]
    max_recovery_objects: Option<u32>,
    #[arg(long,requires="with_recovery_payloads",help=culprit_core::t!("evidence.cli.max_recovery_bytes", default=format::digits(RecoveryPayloadBudget::default().max_bytes.to_string())))]
    max_recovery_bytes: Option<u64>,
}
impl Budget {
    fn read_budget(&self) -> EvidenceReadBudget {
        EvidenceReadBudget {
            page_rows: self.page_rows,
            page_bytes: self.page_bytes,
            max_rows: self.max_rows,
            max_queries: self.max_queries,
            max_bytes: self.max_bytes,
            max_proof_bytes: self.max_proof_bytes,
            timeout_ms: self.timeout_ms,
            recovery_payloads: self.with_recovery_payloads.then(|| {
                let defaults = RecoveryPayloadBudget::default();
                RecoveryPayloadBudget {
                    max_objects: self.max_recovery_objects.unwrap_or(defaults.max_objects),
                    max_bytes: self.max_recovery_bytes.unwrap_or(defaults.max_bytes),
                }
            }),
        }
    }
}

#[derive(Clone, Copy, ValueEnum)]
enum Family {
    WaitCounterSamples,
    ResourceSamples,
    OperationalSamples,
    ActivityObservations,
    ObjectDefinitions,
    TuningObservations,
    QueryDefinitions,
    PlanDefinitions,
    QueryRuntime,
    QueryWaits,
    ReplicationSamples,
}
impl From<Family> for EvidenceFamily {
    fn from(family: Family) -> Self {
        match family {
            Family::WaitCounterSamples => Self::WaitCounterSamples,
            Family::ResourceSamples => Self::ResourceSamples,
            Family::OperationalSamples => Self::OperationalSamples,
            Family::ActivityObservations => Self::ActivityObservations,
            Family::ObjectDefinitions => Self::ObjectDefinitions,
            Family::TuningObservations => Self::TuningObservations,
            Family::QueryDefinitions => Self::QueryDefinitions,
            Family::PlanDefinitions => Self::PlanDefinitions,
            Family::QueryRuntime => Self::QueryRuntime,
            Family::QueryWaits => Self::QueryWaits,
            Family::ReplicationSamples => Self::ReplicationSamples,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
enum Analysis {
    Queries,
    SourceOverhead,
    Blocking,
    SessionCensus,
    MemoryFailures,
    NativeFailures,
    Jobs,
    RequestMemory,
    SessionMemory,
    PageWaits,
    PageWaitWriters,
    PageTaskHolders,
    TaskSpace,
    StatementWork,
    CompletedSpills,
    PageServerReads,
    StatementGrants,
    CacheEntryMemory,
    RuntimeDistributions,
    CachedPlans,
    Compilation,
    CompilerAdmission,
    Maintenance,
    MaintenanceProgress,
    MaintenanceBlocking,
    CurrentRequestWork,
    Replication,
    ConnectionOutcomes,
    ConfiguredLogRates,
    HyperscaleLogComponents,
    CounterRatios,
    PerformanceCounters,
    MemoryClerks,
    TempdbMetadataMemory,
    DatabaseProperties,
    InstanceResources,
    Storage,
    WaitCounters,
    Cleanup,
    Tuning,
}
impl Analysis {
    fn families(self) -> Vec<EvidenceFamily> {
        match self {
            Self::WaitCounters => vec![EvidenceFamily::WaitCounterSamples],
            Self::SourceOverhead => vec![], // Metadata is selected before constructing a row EvidenceRequest.
            Self::Blocking | Self::Maintenance | Self::SessionCensus => vec![EvidenceFamily::ActivityObservations],
            Self::CachedPlans => vec![EvidenceFamily::ObjectDefinitions],
            Self::Tuning => vec![EvidenceFamily::TuningObservations],
            Self::Replication => vec![EvidenceFamily::ReplicationSamples],
            Self::PageServerReads => vec![EvidenceFamily::QueryRuntime],
            Self::Queries => vec![
                EvidenceFamily::QueryDefinitions,
                EvidenceFamily::PlanDefinitions,
                EvidenceFamily::QueryRuntime,
                EvidenceFamily::QueryWaits,
            ],
            Self::Jobs
            | Self::MemoryFailures
            | Self::NativeFailures
            | Self::RequestMemory
            | Self::SessionMemory
            | Self::PageWaits
            | Self::PageWaitWriters
            | Self::PageTaskHolders
            | Self::TaskSpace
            | Self::StatementWork
            | Self::CompletedSpills
            | Self::CounterRatios
            | Self::PerformanceCounters
            | Self::MemoryClerks
            | Self::TempdbMetadataMemory
            | Self::InstanceResources
            | Self::DatabaseProperties
            | Self::Storage
            | Self::ConnectionOutcomes
            | Self::ConfiguredLogRates
            | Self::HyperscaleLogComponents
            | Self::StatementGrants
            | Self::CacheEntryMemory
            | Self::RuntimeDistributions
            | Self::Compilation
            | Self::CompilerAdmission
            | Self::MaintenanceProgress
            | Self::MaintenanceBlocking
            | Self::CurrentRequestWork
            | Self::Cleanup => {
                vec![EvidenceFamily::OperationalSamples]
            }
        }
    }
}

impl CommonArgs {
    fn service(&self) -> Result<EvidenceService> {
        CollectorRuntime::default().evidence(&EvidenceConfig::from_file(&self.config)?)
    }

    fn validate(&self, output: Format) -> Result<EvidenceReadBudget> {
        if !matches!(output, Format::Text | Format::Json) {
            return Err(culprit_core::Error::invalid(t!("evidence.cli.format")));
        }
        bounded_output::validate_json_limit(self.max_output_bytes)?;
        if !valid_digest(&self.range) {
            return Err(DeliveryProblem::InvalidContract.error());
        }
        let budget = self.budget.read_budget();
        budget.validate()?;
        Ok(budget)
    }

    fn request(&self, families: Vec<EvidenceFamily>, output: Format) -> Result<EvidenceRequest> {
        let request = EvidenceRequest { range_id: self.range.clone(), families, budget: self.validate(output)? };
        request.validate()?;
        Ok(request)
    }
}

pub(crate) async fn run(command: EvidenceCommand, out: &Out) -> Result<u8> {
    let selection::EvidenceSelection { common, plan } = selection::EvidenceSelection::from_command(command)?;
    let analysis = plan.analysis;
    if analysis == Some(Analysis::SourceOverhead) {
        return source_overhead(&common, out).await;
    }
    let request = common.request(plan.families.clone(), out.format())?;
    let service = common.service()?;
    let cancellation = CollectionCancellation::default();
    let reading = read_captures(&service, &request, &plan.companions, &cancellation);
    let captures = crate::collection_shutdown::run(&cancellation, reading).await?;
    let complete = captures.is_read_complete();
    let ReadCaptures { capture, activity, baseline, blocking, definitions } = captures;
    match analysis {
        None => emit(out, &capture, common.max_output_bytes, || {
            read_summary(capture.range_id(), capture.records().len(), capture.gaps().len())
        })?,
        Some(Analysis::SourceOverhead) => return Err(DeliveryProblem::InvalidContract.error()),
        Some(Analysis::Queries) => {
            let catalog = capture.source().query_history().and_then(|history| history.population_catalog.as_ref());
            let population = resolve_population(catalog, plan.population.as_ref())?;
            let analysis = culprit_engine::collected_evidence::analyze(&capture, population.as_ref())?;
            emit(out, &analysis, common.max_output_bytes, || observations::queries(&analysis))?;
        }
        Some(Analysis::MemoryFailures) => {
            let analysis = culprit_engine::collected_resources::memory_failures(&capture)?;
            emit(out, &analysis, common.max_output_bytes, || observations::retained(&analysis.report))?;
        }
        Some(Analysis::Blocking) => {
            let analysis = culprit_engine::collected_resources::blocking(&capture)?;
            emit(out, &analysis, common.max_output_bytes, || observations::blocking(&analysis.report))?;
        }
        Some(Analysis::NativeFailures) => {
            let analysis = culprit_engine::collected_resources::native_failures(&capture)?;
            emit(out, &analysis, common.max_output_bytes, || observations::retained(&analysis.report))?;
        }
        Some(Analysis::RequestMemory) => {
            let analysis = culprit_engine::collected_resources::request_memory(&capture)?;
            emit(out, &analysis, common.max_output_bytes, || observations::participants(&analysis.report))?;
        }
        Some(Analysis::SessionMemory) => {
            let analysis = culprit_engine::collected_resources::session_memory(&capture)?;
            emit(out, &analysis, common.max_output_bytes, || observations::participants(&analysis.report))?;
        }
        Some(Analysis::PageWaits) => {
            let analysis = culprit_engine::collected_resources::page_waits(&capture, activity.as_ref())?;
            emit(out, &analysis, common.max_output_bytes, || observations::page_waits(&analysis.report))?;
        }
        Some(Analysis::PageWaitWriters) => {
            let analysis = culprit_engine::collected_resources::page_wait_writers(&capture)?;
            emit(out, &analysis, common.max_output_bytes, || observations::page_wait_writers(&analysis.report))?;
        }
        Some(Analysis::PageTaskHolders) => {
            let analysis = culprit_engine::collected_resources::page_task_holders(&capture)?;
            emit(out, &analysis, common.max_output_bytes, || observations::page_task_holders(&analysis.report))?;
        }
        Some(Analysis::TaskSpace) => {
            let analysis = culprit_engine::collected_resources::task_space(&capture)?;
            emit(out, &analysis, common.max_output_bytes, || observations::task_space(&analysis.report))?;
        }
        Some(Analysis::StatementWork) => {
            let analysis = culprit_engine::collected_resources::statement_work(&capture)?;
            emit(out, &analysis, common.max_output_bytes, || observations::statement_work(&analysis.report))?;
        }
        Some(Analysis::CompletedSpills) => {
            let analysis = culprit_engine::collected_resources::completed_spills(&capture, definitions.as_ref())?;
            emit(out, &analysis, common.max_output_bytes, || observations::completed_spills(&analysis.report))?;
        }
        Some(Analysis::PageServerReads) => {
            let analysis = culprit_engine::collected_resources::page_server_reads(&capture, definitions.as_ref())?;
            emit(out, &analysis, common.max_output_bytes, || observations::collected_page_server_reads(&analysis))?;
        }
        Some(Analysis::StatementGrants) => {
            let analysis = culprit_engine::collected_resources::statement_grants(&capture)?;
            emit(out, &analysis, common.max_output_bytes, || observations::statement_grants(&analysis.report))?;
        }
        Some(Analysis::CacheEntryMemory) => {
            let analysis = culprit_engine::collected_resources::cache_entry_memory(&capture, baseline.as_ref())?;
            emit(out, &analysis, common.max_output_bytes, || observations::cache_entry_memory(&analysis.report))?;
        }
        Some(Analysis::RuntimeDistributions) => {
            let analysis = culprit_engine::collected_resources::runtime_distributions(&capture)?;
            emit(out, &analysis, common.max_output_bytes, || observations::runtime_distributions(&analysis.report))?;
        }
        Some(Analysis::CachedPlans) => {
            let analysis = culprit_engine::collected_resources::cached_plans(&capture)?;
            emit(out, &analysis, common.max_output_bytes, || observations::cached_plans(&analysis.report))?;
        }
        Some(Analysis::Compilation) => {
            let analysis =
                culprit_engine::collected_resources::compilation(&capture, baseline.as_ref(), definitions.as_ref())?;
            emit(out, &analysis, common.max_output_bytes, || observations::collected_compilation(&analysis))?;
        }
        Some(Analysis::CompilerAdmission) => {
            let analysis = culprit_engine::collected_resources::compiler_admission(&capture)?;
            emit(out, &analysis, common.max_output_bytes, || observations::compiler_admission(&analysis.report))?;
        }
        Some(Analysis::Maintenance) => {
            let analysis = culprit_engine::collected_resources::maintenance(&capture)?;
            emit(out, &analysis, common.max_output_bytes, || observations::maintenance(&analysis.report))?;
        }
        Some(Analysis::MaintenanceProgress) => {
            let analysis = culprit_engine::collected_resources::maintenance_progress(&capture)?;
            emit(out, &analysis, common.max_output_bytes, || observations::maintenance_progress(&analysis.report))?;
        }
        Some(Analysis::MaintenanceBlocking) => {
            let blocking = blocking.as_ref().ok_or_else(|| DeliveryProblem::InvalidContract.error())?;
            let analysis = culprit_engine::collected_resources::maintenance_blocking(&capture, blocking)?;
            emit(out, &analysis, common.max_output_bytes, || observations::maintenance_blocking(&analysis.report))?;
        }
        Some(Analysis::CurrentRequestWork) => {
            let analysis = culprit_engine::collected_resources::current_request_work(&capture)?;
            emit(out, &analysis, common.max_output_bytes, || observations::current_request_work(&analysis.report))?;
        }
        Some(Analysis::Jobs) => {
            let analysis = culprit_engine::collected_resources::jobs(&capture)?;
            emit(out, &analysis, common.max_output_bytes, || observations::jobs(&analysis.report))?;
        }
        Some(Analysis::Replication) => {
            let analysis = culprit_engine::collected_resources::replication(&capture)?;
            emit(out, &analysis, common.max_output_bytes, || observations::replication(&analysis.report))?;
        }
        Some(Analysis::WaitCounters) => {
            let analysis = culprit_engine::collected_resources::wait_counters(&capture)?;
            emit(out, &analysis, common.max_output_bytes, || observations::wait_counters(&analysis.report))?;
        }
        Some(Analysis::SessionCensus) => {
            let analysis = culprit_engine::collected_resources::session_census(&capture)?;
            emit(out, &analysis, common.max_output_bytes, || observations::session_census(&analysis.report))?;
        }
        Some(Analysis::Storage) => {
            let analysis = culprit_engine::collected_resources::storage(&capture)?;
            emit(out, &analysis, common.max_output_bytes, || observations::storage(&analysis.report))?;
        }
        Some(Analysis::InstanceResources) => {
            let analysis = culprit_engine::collected_resources::instance_resources(&capture)?;
            emit(out, &analysis, common.max_output_bytes, || observations::instance_resources(&analysis.report))?;
        }
        Some(Analysis::DatabaseProperties) => {
            let analysis = culprit_engine::collected_resources::database_properties(&capture)?;
            emit(out, &analysis, common.max_output_bytes, || observations::database_properties(&analysis.report))?;
        }
        Some(Analysis::MemoryClerks) => {
            let analysis = culprit_engine::collected_resources::memory_clerks(&capture)?;
            emit(out, &analysis, common.max_output_bytes, || observations::memory_clerks(&analysis.report))?;
        }
        Some(Analysis::TempdbMetadataMemory) => {
            let analysis = culprit_engine::collected_resources::tempdb_metadata_memory(&capture)?;
            emit(out, &analysis, common.max_output_bytes, || observations::tempdb_metadata_memory(&analysis.report))?;
        }
        Some(Analysis::PerformanceCounters) => {
            let analysis = culprit_engine::collected_resources::performance_counters(&capture)?;
            emit(out, &analysis, common.max_output_bytes, || observations::performance_counters(&analysis.report))?;
        }
        Some(Analysis::CounterRatios) => {
            let analysis = culprit_engine::collected_resources::counter_ratios(&capture)?;
            emit(out, &analysis, common.max_output_bytes, || observations::counter_ratios(&analysis.report))?;
        }
        Some(Analysis::HyperscaleLogComponents) => {
            let analysis = culprit_engine::collected_resources::hyperscale_log_components(&capture)?;
            emit(out, &analysis, common.max_output_bytes, || {
                observations::hyperscale_log_components(&analysis.report)
            })?;
        }
        Some(Analysis::ConfiguredLogRates) => {
            let analysis = culprit_engine::collected_resources::configured_log_rates(&capture)?;
            emit(out, &analysis, common.max_output_bytes, || observations::configured_log_rates(&analysis.report))?;
        }
        Some(Analysis::ConnectionOutcomes) => {
            let analysis = culprit_engine::collected_resources::connection_outcomes(&capture)?;
            emit(out, &analysis, common.max_output_bytes, || observations::connection_outcomes(&analysis.report))?;
        }
        Some(Analysis::Tuning) => {
            let analysis = culprit_engine::collected_resources::tuning(&capture)?;
            emit(out, &analysis, common.max_output_bytes, || observations::tuning(&analysis.report))?;
        }
        Some(Analysis::Cleanup) => {
            let analysis = culprit_engine::collected_resources::cleanup(&capture)?;
            emit(out, &analysis, common.max_output_bytes, || observations::cleanup(&analysis.report))?;
        }
    }
    if !complete && out.format() == Format::Text {
        println!("{}", t!("evidence.cli.read_incomplete"));
    }
    Ok(if complete { 0 } else { 2 })
}

async fn source_overhead(common: &CommonArgs, out: &Out) -> Result<u8> {
    let budget = common.validate(out.format())?;
    let service = common.service()?;
    let cancellation = CollectionCancellation::default();
    let reading = async {
        let mut session = service.session(budget, &cancellation)?;
        session.read_metadata(&common.range).await
    };
    let metadata = crate::collection_shutdown::run(&cancellation, reading).await?;
    let complete = metadata.is_read_complete();
    let analysis = culprit_engine::collected_resources::source_overhead(&metadata)?;
    emit(out, &analysis, common.max_output_bytes, || observations::collected_source_overhead(&analysis))?;
    if !complete && out.format() == Format::Text {
        println!("{}", t!("evidence.cli.read_incomplete"));
    }
    Ok(if complete { 0 } else { 2 })
}

async fn read_captures(
    service: &EvidenceService,
    request: &EvidenceRequest,
    companions: &selection::CompanionReadPlan,
    cancellation: &CollectionCancellation,
) -> Result<ReadCaptures> {
    let mut session = service.session(request.budget, cancellation)?;
    let capture = session.read(&request.range_id, &request.families).await?;
    let activity = if let Some(range) = &companions.activity {
        if culprit_core::PageWaitActivityEvidence::declared_parent(capture.source()).is_none() {
            return Err(DeliveryProblem::UnsupportedSchema.error());
        }
        Some(session.read(range, &[EvidenceFamily::ActivityObservations]).await?)
    } else {
        None
    };
    let baseline_selection = match (&companions.compilation_baseline, &companions.cache_baseline) {
        (Some(range), None) => Some((range, culprit_core::telemetry::SnapshotDataset::QueryCompilation)),
        (None, Some(range)) => {
            if !matches!(capture.source().observation, SourceObservation::CacheEntryMemory { .. }) {
                return Err(DeliveryProblem::UnsupportedSchema.error());
            }
            Some((range, culprit_core::telemetry::SnapshotDataset::CacheEntryMemory))
        }
        (None, None) => None,
        (Some(_), Some(_)) => return Err(DeliveryProblem::InvalidContract.error()),
    };
    let baseline = if let Some((range, dataset)) = baseline_selection {
        if !capture.source().observation.snapshot().is_some_and(|snapshot| snapshot.dataset == dataset) {
            return Err(DeliveryProblem::UnsupportedSchema.error());
        }
        Some(session.read(range, &[EvidenceFamily::OperationalSamples]).await?)
    } else {
        None
    };
    let blocking = if let Some(range) = &companions.blocking {
        if !matches!(capture.source().observation, SourceObservation::MaintenanceProgress { .. }) {
            return Err(DeliveryProblem::UnsupportedSchema.error());
        }
        Some(session.read(range, &[EvidenceFamily::ActivityObservations]).await?)
    } else {
        None
    };
    let definitions = if let Some(definitions) = &companions.definitions {
        if !definitions.kind.accepts_primary(capture.source()) {
            return Err(DeliveryProblem::UnsupportedSchema.error());
        }
        Some(session.read(&definitions.range, &[definitions.kind.family()]).await?)
    } else {
        None
    };
    Ok(ReadCaptures { capture, activity, baseline, blocking, definitions })
}

struct ReadCaptures {
    capture: EvidenceCapture,
    activity: Option<EvidenceCapture>,
    baseline: Option<EvidenceCapture>,
    blocking: Option<EvidenceCapture>,
    definitions: Option<EvidenceCapture>,
}

impl ReadCaptures {
    fn is_read_complete(&self) -> bool {
        std::iter::once(&self.capture)
            .chain(self.activity.iter())
            .chain(self.baseline.iter())
            .chain(self.blocking.iter())
            .chain(self.definitions.iter())
            .all(EvidenceCapture::is_read_complete)
    }
}

/// CLI IDs select an exact captured identity only when its native ID is unambiguous; labels and native properties
/// never supply missing qualifiers. The immutable archive read, including all row proofs, remains unchanged.
fn resolve_population(
    catalog: Option<&NativeRuntimePopulationCatalog>,
    requested: Option<&NativeIdentity>,
) -> Result<Option<NativeIdentity>> {
    let Some(requested) = requested else { return Ok(None) };
    let missing = || culprit_core::Error::invalid(t!("evidence.cli.population_missing", id = &requested.id));
    let catalog = catalog.filter(|catalog| catalog.is_valid()).ok_or_else(missing)?;
    let mut matches = catalog.populations.iter().filter(|population| population.identity.id == requested.id);
    let selected = matches.next().ok_or_else(missing)?;
    if matches.next().is_some() {
        return Err(culprit_core::Error::invalid(t!("evidence.cli.population_ambiguous", id = &requested.id)));
    }
    Ok(Some(selected.identity.clone()))
}

fn read_summary(range: &str, rows: usize, gaps: usize) -> String {
    t!("evidence.cli.read_summary", range = range, rows = format::count(rows as f64), gaps = format::count(gaps as f64))
}
fn emit(value_out: &Out, value: &impl Serialize, limit: usize, human: impl FnOnce() -> String) -> Result<()> {
    if value_out.format() == Format::Json {
        let bytes = bounded_output::json(value, limit)?;
        std::io::stdout().lock().write_all(&bytes).map_err(|_| output_failed())?;
    } else {
        println!("{}", human());
    }
    Ok(())
}
fn output_failed() -> culprit_core::Error {
    culprit_core::Error::internal(t!("cli.output_failed"))
}

#[cfg(test)]
mod tests;
