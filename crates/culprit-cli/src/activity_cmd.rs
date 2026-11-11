use crate::{
    collect_cmd::{ActivityTuning, BudgetArgs},
    output::{Format, Out},
};
use clap::Args;
use culprit_collector::{capture::CollectionCancellation, outbox::Outbox};
use culprit_core::{Result, telemetry::*};
use culprit_engine::Workspace;
use std::{io::Write, path::PathBuf};

#[derive(Args)]
pub(crate) struct ActivityArgs {
    connection: String,
    #[arg(long,help=culprit_core::t!("outbox.cli.destination"))]
    outbox: PathBuf,
    #[arg(long,default_value_t=MIN_ACTIVITY_POLL_SECONDS,help=culprit_core::t!("activity.cli.interval"))]
    poll_seconds: u32,
    #[arg(long,help=culprit_core::t!("activity.cli.max_polls"))]
    max_polls: Option<u32>,
    #[command(flatten)]
    tuning: ActivityTuning,
    #[command(flatten)]
    budget: BudgetArgs,
    #[command(flatten)]
    pub(crate) identity: crate::collect_cmd::AzureIdentityArgs,
}
impl ActivityArgs {
    fn options(&self) -> Result<ActivityMonitorOptions> {
        let options = ActivityMonitorOptions {
            poll_seconds: self.poll_seconds,
            max_polls: self.max_polls,
            policy: self.tuning.policy()?,
            budget: self.budget.budget(),
        };
        options.validate()?;
        Ok(options)
    }
}
pub(crate) async fn run(ws: &Workspace, args: ActivityArgs, out: &Out) -> Result<u8> {
    let options = args.options()?;
    let mut outbox = Outbox::open(&args.outbox)?;
    let cancellation = CollectionCancellation::default();
    let monitor =
        ws.monitor_activity(&args.connection, &options, &mut outbox, &cancellation, |event| print(event, out));
    let summary = crate::collection_shutdown::run(&cancellation, monitor).await?;
    Ok(if summary.unknown > 0 && summary.complete == 0 { 2 } else { 0 })
}
fn print(event: &ActivityMonitorEvent, out: &Out) -> Result<()> {
    let line = if out.format() == Format::Json {
        serde_json::to_string(event)
            .map_err(|_| culprit_core::Error::internal(culprit_core::t!("collection.manifest_failed")))?
    } else if event.status == CollectionStatus::Complete {
        culprit_core::t!(
            "activity.cli.complete",
            active = event.active_incidents.unwrap_or(0),
            changes = event.changes.unwrap_or(0)
        )
    } else {
        culprit_core::t!("activity.cli.unknown")
    };
    writeln!(std::io::stdout().lock(), "{line}")
        .map_err(|_| culprit_core::Error::internal(culprit_core::t!("activity.cli.output_failed")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    fn parse(arguments: &[&str]) -> ActivityArgs {
        let cli = crate::Cli::try_parse_from(
            ["culprit", "collect", "activity", "pilot", "--outbox", "journal"]
                .into_iter()
                .chain(arguments.iter().copied()),
        )
        .unwrap();
        let crate::Command::Collect(crate::collect_cmd::CollectCommand::Activity(args)) = cli.command else {
            panic!("wrong command")
        };
        args
    }

    #[test]
    fn polling_defaults_to_one_minute_independently_of_incident_thresholds() {
        let options = parse(&[]).options().unwrap();
        assert_eq!(options.poll_seconds, 60);
        assert_eq!(options.policy.blocked_after_ms, 30_000);
        assert_eq!(options.policy.running_after_ms, 60_000);
        let options = parse(&["--poll-seconds", "300", "--blocked-seconds", "120"]).options().unwrap();
        assert_eq!(options.poll_seconds, 300);
        assert_eq!(options.policy.blocked_after_ms, 120_000);
    }
    #[test]
    fn invalid_monitor_limits_are_rejected_before_opening_the_outbox_or_provider() {
        assert!(parse(&["--poll-seconds", "10"]).options().is_err());
        assert!(parse(&["--max-polls", "0"]).options().is_err());
        assert!(parse(&["--running-seconds", "0"]).options().is_err());
        assert!(parse(&["--blocked-seconds", "4294967295"]).options().is_err());
    }
}
