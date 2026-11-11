use crate::output::{Format, Out};
use clap::Subcommand;
use culprit_collector::outbox::Outbox;
use culprit_core::{Result, telemetry::OutboxLimits};
use std::path::PathBuf;

#[derive(Subcommand)]
pub(crate) enum OutboxCommand {
    #[command(about=culprit_core::t!("outbox.cli.init"))]
    Init {
        directory: PathBuf,
        #[arg(long,default_value_t=OutboxLimits::default().max_bytes,help=culprit_core::t!("outbox.cli.max_bytes"))]
        max_bytes: u64,
        #[arg(long,default_value_t=OutboxLimits::default().max_captures,help=culprit_core::t!("outbox.cli.max_captures"))]
        max_captures: u32,
    },
    #[command(about=culprit_core::t!("outbox.cli.status"))]
    Status {
        directory: PathBuf,
        #[arg(long, default_value_t = 50)]
        limit: u32,
        #[arg(long)]
        after: Option<String>,
    },
    #[command(about=culprit_core::t!("outbox.cli.recover"))]
    Recover { directory: PathBuf },
    #[command(about=culprit_core::t!("outbox.cli.verify"))]
    Verify { directory: PathBuf, observation: String },
    #[command(about=culprit_core::t!("outbox.cli.replay"))]
    Replay {
        directory: PathBuf,
        observation: String,
        #[arg(long)]
        output: PathBuf,
    },
    #[command(about=culprit_core::t!("outbox.cli.acknowledge"))]
    Acknowledge {
        directory: PathBuf,
        observation: String,
        #[arg(long,required=true,help=culprit_core::t!("outbox.cli.source_quiescent"))]
        source_quiescent: bool,
    },
}

pub(crate) async fn run(command: OutboxCommand, out: &Out) -> Result<u8> {
    use culprit_core::{format, t};
    let (value, message) = match command {
        OutboxCommand::Init { directory, max_bytes, max_captures } => {
            let outbox = Outbox::create(&directory, OutboxLimits { max_bytes, max_captures })?;
            (
                encode(&outbox.status(50, None).await?)?,
                t!("outbox.cli.initialized", directory = directory.display().to_string()),
            )
        }
        OutboxCommand::Status { directory, limit, after } => {
            let status = Outbox::inspect(&directory, limit, after.as_deref()).await?;
            let message = t!(
                "outbox.cli.status_result",
                captures = format::count(status.captures as f64),
                ready = format::count(status.ready as f64),
                debts = format::count(status.source_debts as f64)
            );
            (encode(&status)?, message)
        }
        OutboxCommand::Recover { directory } => {
            let report = Outbox::open(&directory)?.recover().await?;
            let message = t!(
                "outbox.cli.recovered",
                committed = format::count(report.committed as f64),
                interrupted = format::count(report.interrupted as f64),
                quarantined = format::count(report.quarantined as f64),
                debts = format::count(report.source_debts as f64)
            );
            (encode(&report)?, message)
        }
        OutboxCommand::Verify { directory, observation } => {
            let manifest = Outbox::open(&directory)?.verify(&observation).await?;
            (encode(&manifest)?, t!("outbox.cli.verified", observation = observation))
        }
        OutboxCommand::Replay { directory, observation, output } => {
            let replay = Outbox::replay(&directory, &observation, &output).await?;
            (encode(&replay)?, t!("outbox.cli.replayed", directory = output.display().to_string()))
        }
        OutboxCommand::Acknowledge { directory, observation, source_quiescent } => {
            if !source_quiescent {
                return Err(culprit_core::Error::invalid(t!("outbox.cli.source_quiescent")));
            }
            let mut outbox = Outbox::open(&directory)?;
            outbox.acknowledge_source_quiescence(&observation).await?;
            (encode(&outbox.status(50, None).await?)?, t!("outbox.cli.acknowledged", observation = observation))
        }
    };
    if out.format() == Format::Json {
        out.value(&value);
    } else {
        out.done(&message);
    }
    Ok(0)
}
fn encode(value: &impl serde::Serialize) -> Result<serde_json::Value> {
    serde_json::to_value(value)
        .map_err(|_| culprit_core::Error::internal(culprit_core::t!("collection.manifest_failed")))
}
