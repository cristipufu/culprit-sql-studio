use crate::output::{Format, Out};
use clap::Subcommand;
use culprit_collector::{capture::CollectionCancellation, service::ServiceReporter};
use culprit_core::{Result, telemetry::*};
use culprit_engine::collector_service::{self, SourceMode, config::CollectorConfig};
use std::{io::Write, path::PathBuf};

#[derive(Subcommand)]
pub(crate) enum ServiceCommand {
    #[command(about=culprit_core::t!("service.cli.validate"))]
    Validate {
        #[arg(long,help=culprit_core::t!("service.cli.config"))]
        config: PathBuf,
    },
    #[command(about=culprit_core::t!("service.cli.init"))]
    Init {
        #[arg(long,help=culprit_core::t!("service.cli.config"))]
        config: PathBuf,
    },
    #[command(about=culprit_core::t!("service.cli.run"))]
    Run {
        #[arg(long,help=culprit_core::t!("service.cli.config"))]
        config: PathBuf,
        #[arg(long,required_unless_present="delivery_only",conflicts_with="delivery_only",help=culprit_core::t!("service.cli.quiescent"))]
        source_quiescent: bool,
        #[arg(long,conflicts_with="max_work",help=culprit_core::t!("service.cli.delivery_only"))]
        delivery_only: bool,
        #[arg(long,value_parser=clap::value_parser!(u32).range(1..),help=culprit_core::t!("fleet.cli.limit"))]
        max_work: Option<u32>,
    },
}
pub(crate) async fn run(command: ServiceCommand, out: &Out) -> Result<u8> {
    let path = match &command {
        ServiceCommand::Validate { config } | ServiceCommand::Init { config } | ServiceCommand::Run { config, .. } => {
            config
        }
    };
    let config = CollectorConfig::from_file(path)?;
    match command {
        ServiceCommand::Validate { .. } => {
            collector_service::validate(&config)?;
            out.done(&culprit_core::t!("service.cli.valid"));
            Ok(0)
        }
        ServiceCommand::Init { .. } => {
            collector_service::initialize(&config).await?;
            out.done(&culprit_core::t!("service.cli.initialized"));
            Ok(0)
        }
        ServiceCommand::Run { max_work, delivery_only, .. } => {
            let mode = if delivery_only { SourceMode::DeliveryOnly } else { SourceMode::Collect };
            let cancellation = CollectionCancellation::default();
            let summary = crate::collection_shutdown::run(
                &cancellation,
                collector_service::run(&config, mode, &cancellation, max_work, &Reporter(out)),
            )
            .await?;
            Ok(if summary.incomplete > 0 { 2 } else { 0 })
        }
    }
}
struct Reporter<'a>(&'a Out);
impl ServiceReporter for Reporter<'_> {
    fn source_paused(&self, error: &culprit_core::Error) -> Result<()> {
        if self.0.format() == Format::Json {
            write(&serde_json::json!({"kind":"source-paused","code":error.code,"detail":error.detail}))
        } else {
            self.0.error(error);
            Ok(())
        }
    }
    fn collection(&self, event: &FleetCollectionEvent) -> Result<()> {
        if self.0.format() == Format::Json {
            write(&serde_json::json!({"kind":"collection","event":event}))
        } else {
            crate::fleet_cmd::print(event, self.0)
        }
    }
    fn delivery(&self, previous: Option<&DeliveryEntry>, result: Result<DeliveryEntry>) -> Result<()> {
        if !delivery_changed(previous, &result) {
            return Ok(());
        }
        let range_id = previous.map(|entry| entry.range_id.as_str());
        match result {
            Ok(entry) => {
                if self.0.format() == Format::Json {
                    write(&serde_json::json!({"kind":"delivery","event":entry}))
                } else {
                    Ok(())
                }
            }
            Err(error) => {
                if self.0.format() == Format::Json {
                    write(
                        &serde_json::json!({"kind":"delivery-error","range_id":range_id,"code":error.code,"detail":error.detail}),
                    )
                } else {
                    if let Some(range) = range_id {
                        eprintln!("{}", culprit_core::t!("service.cli.failed_range", range = range));
                    }
                    self.0.error(&error);
                    Ok(())
                }
            }
        }
    }
}
fn delivery_changed(previous: Option<&DeliveryEntry>, result: &Result<DeliveryEntry>) -> bool {
    let Some(before) = previous else {
        return true;
    };
    match result {
        Ok(now) => before.phase != now.phase || before.attempt != now.attempt || before.problem != now.problem,
        Err(error) => {
            !matches!(error.detail,Some(culprit_core::ErrorDetail::Delivery(problem)) if before.problem==Some(problem))
        }
    }
}
fn write(value: &impl serde::Serialize) -> Result<()> {
    let mut stdout = std::io::stdout().lock();
    serde_json::to_writer(&mut stdout, value).map_err(|_| output_failed())?;
    writeln!(stdout).map_err(|_| output_failed())
}
fn output_failed() -> culprit_core::Error {
    culprit_core::Error::internal(culprit_core::t!("activity.cli.output_failed"))
}

#[cfg(test)]
mod tests {
    use clap::Parser;
    #[test]
    fn normal_startup_requires_an_independent_source_quiescence_assertion() {
        let args = ["culprit", "collect", "service", "run", "--config", "collector.json"];
        assert!(crate::Cli::try_parse_from(args).is_err());
        assert!(crate::Cli::try_parse_from(args.into_iter().chain(["--source-quiescent"])).is_ok());
        assert!(crate::Cli::try_parse_from(args.into_iter().chain(["--delivery-only"])).is_ok());
        assert!(crate::Cli::try_parse_from(args.into_iter().chain(["--source-quiescent", "--delivery-only"])).is_err());
        assert!(crate::Cli::try_parse_from(args.into_iter().chain(["--delivery-only", "--max-work", "1"])).is_err());
        assert!(
            crate::Cli::try_parse_from(["culprit", "collect", "service", "validate", "--config", "collector.json"])
                .is_ok()
        );
    }
    #[test]
    fn daemon_logs_keep_phase_changes_errors_and_recovery_without_repeating_deferred_or_identical_retry_events() {
        use super::*;
        let mut previous = DeliveryEntry {
            range_id: "range".into(),
            phase: DeliveryPhase::AwaitingData,
            attempt: 1,
            retry_at_ms: "0".into(),
            problem: None,
        };
        let mut deferred = previous.clone();
        deferred.retry_at_ms = "5000".into();
        assert!(!delivery_changed(Some(&previous), &Ok(deferred)));
        assert!(delivery_changed(Some(&previous), &Err(DeliveryProblem::Transport.error())));
        previous.problem = Some(DeliveryProblem::Transport);
        assert!(!delivery_changed(Some(&previous), &Err(DeliveryProblem::Transport.error())));
        assert!(delivery_changed(Some(&previous), &Err(DeliveryProblem::Integrity.error())));
        let mut recovered = previous.clone();
        recovered.problem = None;
        assert!(delivery_changed(Some(&previous), &Ok(recovered)));
        let mut redriven = previous.clone();
        redriven.attempt += 1;
        assert!(delivery_changed(Some(&previous), &Ok(redriven)));
        assert!(delivery_changed(None, &Err(DeliveryProblem::Transport.error())));
    }
}
