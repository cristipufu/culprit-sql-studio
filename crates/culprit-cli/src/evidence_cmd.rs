//! Archive investigation resolves only query and recovery profiles; source databases and delivery stay out of scope.
use crate::{
    bounded_output,
    output::{Format, Out, observations},
};
use clap::{Args, Subcommand, ValueEnum};
use culprit_collector::{
    analytical::evidence::{EvidenceFamily, EvidenceReadBudget},
    capture::CollectionCancellation,
};
use culprit_core::{Result, format, t};
use culprit_engine::collector_service::{
    CollectorRuntime,
    evidence::{EvidenceConfig, EvidenceRequest},
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
        }
    }
}

#[derive(Clone, Copy, ValueEnum)]
enum Family {
    ResourceSamples,
    OperationalSamples,
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
            Family::QueryDefinitions => Self::QueryDefinitions,
            Family::PlanDefinitions => Self::PlanDefinitions,
            Family::QueryRuntime => Self::QueryRuntime,
            Family::QueryWaits => Self::QueryWaits,
        }
    }
}

#[derive(Clone, Copy, ValueEnum)]
enum Analysis {
    Queries,
    MemoryFailures,
    NativeFailures,
    RequestMemory,
    SessionMemory,
    PageWaits,
    TaskSpace,
    StatementWork,
    RuntimeDistributions,
}
impl Analysis {
    fn families(self) -> Vec<EvidenceFamily> {
        match self {
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
            | Self::TaskSpace
            | Self::StatementWork
            | Self::RuntimeDistributions => {
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
    let (common, families, analysis) = match command {
        EvidenceCommand::Read(args) => (args.common, args.families.into_iter().map(Into::into).collect(), None),
        EvidenceCommand::Analyze(args) => (args.common, args.analysis.families(), Some(args.analysis)),
    };
    let request = common.request(families, out.format())?;
    let config = EvidenceConfig::from_file(&common.config)?;
    let service = CollectorRuntime::default().evidence(&config)?;
    let cancellation = CollectionCancellation::default();
    let capture = crate::collection_shutdown::run(&cancellation, service.read(&request, &cancellation)).await?;
    let complete = capture.is_read_complete();
    match analysis {
        None => emit(out, &capture, common.max_output_bytes, || {
            read_summary(capture.range_id(), capture.records().len(), capture.gaps().len())
        })?,
        Some(Analysis::Queries) => {
            let analysis = culprit_engine::collected_evidence::analyze(&capture)?;
            emit(out, &analysis, common.max_output_bytes, || {
                let mut text = t!(
                    "evidence.cli.query_summary",
                    range = capture.range_id(),
                    findings = format::count(analysis.findings.len() as f64),
                    limitations = format::count(analysis.limitations.len() as f64),
                    gaps = format::count(capture.gaps().len() as f64)
                );
                crate::output::append_brief_titles(
                    &mut text,
                    analysis.findings.iter().map(|finding| finding.title.as_str()),
                );
                text
            })?;
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
            let analysis = culprit_engine::collected_resources::page_waits(&capture)?;
            emit(out, &analysis, common.max_output_bytes, || observations::page_waits(&analysis.report))?;
        }
        Some(Analysis::TaskSpace) => {
            let analysis = culprit_engine::collected_resources::task_space(&capture)?;
            emit(out, &analysis, common.max_output_bytes, || observations::task_space(&analysis.report))?;
        }
        Some(Analysis::StatementWork) => {
            let analysis = culprit_engine::collected_resources::statement_work(&capture)?;
            emit(out, &analysis, common.max_output_bytes, || observations::statement_work(&analysis.report))?;
        }
        Some(Analysis::RuntimeDistributions) => {
            let analysis = culprit_engine::collected_resources::runtime_distributions(&capture)?;
            emit(out, &analysis, common.max_output_bytes, || observations::runtime_distributions(&analysis.report))?;
        }
    }
    if !complete && out.format() == Format::Text {
        println!("{}", t!("evidence.cli.read_incomplete"));
    }
    Ok(if complete { 0 } else { 2 })
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
