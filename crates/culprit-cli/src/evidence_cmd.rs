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
    ResourceSamples,
    OperationalSamples,
    ActivityObservations,
    ObjectDefinitions,
    TuningObservations,
    QueryDefinitions,
    PlanDefinitions,
    QueryRuntime,
    QueryWaits,
}
impl From<Family> for EvidenceFamily {
    fn from(family: Family) -> Self {
        match family {
            Family::ResourceSamples => Self::ResourceSamples,
            Family::OperationalSamples => Self::OperationalSamples,
            Family::ActivityObservations => Self::ActivityObservations,
            Family::ObjectDefinitions => Self::ObjectDefinitions,
            Family::TuningObservations => Self::TuningObservations,
            Family::QueryDefinitions => Self::QueryDefinitions,
            Family::PlanDefinitions => Self::PlanDefinitions,
            Family::QueryRuntime => Self::QueryRuntime,
            Family::QueryWaits => Self::QueryWaits,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
enum Analysis {
    Queries,
    MemoryFailures,
    NativeFailures,
    RequestMemory,
    SessionMemory,
    PageWaits,
    PageWaitWriters,
    TaskSpace,
    StatementWork,
    StatementGrants,
    CacheEntryMemory,
    RuntimeDistributions,
    CachedPlans,
    Compilation,
    CompilerAdmission,
    Maintenance,
    MaintenanceProgress,
    Cleanup,
    Tuning,
}
impl Analysis {
    fn families(self) -> Vec<EvidenceFamily> {
        match self {
            Self::Maintenance => vec![EvidenceFamily::ActivityObservations],
            Self::CachedPlans => vec![EvidenceFamily::ObjectDefinitions],
            Self::Tuning => vec![EvidenceFamily::TuningObservations],
            Self::Queries => vec![
                EvidenceFamily::QueryDefinitions,
                EvidenceFamily::PlanDefinitions,
                EvidenceFamily::QueryRuntime,
                EvidenceFamily::QueryWaits,
            ],
            Self::MemoryFailures
            | Self::NativeFailures
            | Self::RequestMemory
            | Self::SessionMemory
            | Self::PageWaits
            | Self::PageWaitWriters
            | Self::TaskSpace
            | Self::StatementWork
            | Self::StatementGrants
            | Self::CacheEntryMemory
            | Self::RuntimeDistributions
            | Self::Compilation
            | Self::CompilerAdmission
            | Self::MaintenanceProgress
            | Self::Cleanup => {
                vec![EvidenceFamily::OperationalSamples]
            }
        }
    }
}

impl CommonArgs {
    fn request(&self, families: Vec<EvidenceFamily>, output: Format) -> Result<EvidenceRequest> {
        if !matches!(output, Format::Text | Format::Json) {
            return Err(culprit_core::Error::invalid(t!("evidence.cli.format")));
        }
        bounded_output::validate_json_limit(self.max_output_bytes)?;
        let request = EvidenceRequest { range_id: self.range.clone(), families, budget: self.budget.read_budget() };
        request.validate()?;
        Ok(request)
    }
}

pub(crate) async fn run(command: EvidenceCommand, out: &Out) -> Result<u8> {
    let (common, families, analysis, requested_population, activity_range) = match command {
        EvidenceCommand::Read(args) => {
            (args.common, args.families.into_iter().map(Into::into).collect(), None, None, None)
        }
        EvidenceCommand::Analyze(args) => {
            args.validate_selection()?;
            (args.common, args.analysis.families(), Some(args.analysis), args.workload_population, args.activity_range)
        }
    };
    let request = common.request(families, out.format())?;
    let config = EvidenceConfig::from_file(&common.config)?;
    let service = CollectorRuntime::default().evidence(&config)?;
    let cancellation = CollectionCancellation::default();
    let reading = read_captures(&service, &request, activity_range.as_deref(), &cancellation);
    let (capture, activity) = crate::collection_shutdown::run(&cancellation, reading).await?;
    let complete = capture.is_read_complete() && activity.as_ref().is_none_or(|capture| capture.is_read_complete());
    match analysis {
        None => emit(out, &capture, common.max_output_bytes, || {
            read_summary(capture.range_id(), capture.records().len(), capture.gaps().len())
        })?,
        Some(Analysis::Queries) => {
            let catalog = capture.source().query_history().and_then(|history| history.population_catalog.as_ref());
            let population = resolve_population(catalog, requested_population.as_ref())?;
            let analysis = culprit_engine::collected_evidence::analyze(&capture, population.as_ref())?;
            emit(out, &analysis, common.max_output_bytes, || observations::queries(&analysis))?;
        }
        Some(Analysis::MemoryFailures) => {
            let analysis = culprit_engine::collected_resources::memory_failures(&capture)?;
            emit(out, &analysis, common.max_output_bytes, || observations::retained(&analysis.report))?;
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
        Some(Analysis::TaskSpace) => {
            let analysis = culprit_engine::collected_resources::task_space(&capture)?;
            emit(out, &analysis, common.max_output_bytes, || observations::task_space(&analysis.report))?;
        }
        Some(Analysis::StatementWork) => {
            let analysis = culprit_engine::collected_resources::statement_work(&capture)?;
            emit(out, &analysis, common.max_output_bytes, || observations::statement_work(&analysis.report))?;
        }
        Some(Analysis::StatementGrants) => {
            let analysis = culprit_engine::collected_resources::statement_grants(&capture)?;
            emit(out, &analysis, common.max_output_bytes, || observations::statement_grants(&analysis.report))?;
        }
        Some(Analysis::CacheEntryMemory) => {
            let analysis = culprit_engine::collected_resources::cache_entry_memory(&capture)?;
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
            let analysis = culprit_engine::collected_resources::compilation(&capture)?;
            emit(out, &analysis, common.max_output_bytes, || observations::compilation(&analysis.report))?;
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

async fn read_captures(
    service: &EvidenceService,
    request: &EvidenceRequest,
    activity_range: Option<&str>,
    cancellation: &CollectionCancellation,
) -> Result<(EvidenceCapture, Option<EvidenceCapture>)> {
    let mut session = service.session(request.budget, cancellation)?;
    let capture = session.read(&request.range_id, &request.families).await?;
    let activity = if let Some(range) = activity_range {
        if !matches!(capture.source().observation, SourceObservation::FocusedPageWait { .. }) {
            return Err(DeliveryProblem::UnsupportedSchema.error());
        }
        Some(session.read(range, &[EvidenceFamily::ActivityObservations]).await?)
    } else {
        None
    };
    Ok((capture, activity))
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
