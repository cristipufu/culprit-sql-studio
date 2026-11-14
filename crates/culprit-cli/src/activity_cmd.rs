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
    let line = line(event, out.format())?;
    writeln!(std::io::stdout().lock(), "{line}")
        .map_err(|_| culprit_core::Error::internal(culprit_core::t!("activity.cli.output_failed")))
}
fn line(event: &ActivityMonitorEvent, format: Format) -> Result<String> {
    Ok(if format == Format::Json {
        serde_json::to_string(event)
            .map_err(|_| culprit_core::Error::internal(culprit_core::t!("collection.manifest_failed")))?
    } else if event.status == CollectionStatus::Complete {
        match (event.active_episodes, event.uncertain_episodes, &event.native_coverage) {
            (Some(active), Some(uncertain), Some(coverage)) if coverage.complete_scope => {
                culprit_core::t!("activity.cli.complete", active = active, uncertain = uncertain)
            }
            (Some(active), Some(uncertain), Some(_)) => {
                culprit_core::t!("activity.cli.partial", active = active, uncertain = uncertain)
            }
            _ => culprit_core::t!("activity.cli.analysis_pending"),
        }
    } else {
        culprit_core::t!("activity.cli.unknown")
    })
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
        assert!(
            crate::Cli::try_parse_from([
                "culprit",
                "collect",
                "activity",
                "pilot",
                "--outbox",
                "journal",
                "--update-seconds",
                "300",
            ])
            .is_err()
        );
    }
    fn event() -> ActivityMonitorEvent {
        ActivityMonitorEvent {
            attempt: 1,
            status: CollectionStatus::Complete,
            error_code: None,
            capture: None,
            observed_at: None,
            active_episodes: Some(2),
            uncertain_episodes: Some(1),
            native_coverage: Some(StateSourceCoverage::collected("native-activity".into(), 3)),
            source_queries: Some(3),
        }
    }
    #[test]
    fn saved_baseline_without_matching_analysis_never_prints_a_fabricated_zero() {
        let mut event = event();
        event.active_episodes = None;
        event.uncertain_episodes = None;
        assert_eq!(line(&event, Format::Text).unwrap(), culprit_core::t!("activity.cli.analysis_pending"));
        let value: serde_json::Value = serde_json::from_str(&line(&event, Format::Json).unwrap()).unwrap();
        assert!(value["active_episodes"].is_null() && value["uncertain_episodes"].is_null());
        assert!(value.get("active_incidents").is_none() && value.get("changes").is_none());
    }
    #[test]
    fn retained_uncertainty_and_incomplete_native_coverage_are_visible_independently_of_capture_completion() {
        let mut event = event();
        assert_eq!(
            line(&event, Format::Text).unwrap(),
            culprit_core::t!("activity.cli.complete", active = 2, uncertain = 1)
        );
        event.native_coverage = Some(StateSourceCoverage::observed("native-activity".into(), 3, false));
        assert_eq!(
            line(&event, Format::Text).unwrap(),
            culprit_core::t!("activity.cli.partial", active = 2, uncertain = 1)
        );
        event.status = CollectionStatus::Incomplete(CollectionStop::Cancelled);
        assert_eq!(line(&event, Format::Text).unwrap(), culprit_core::t!("activity.cli.unknown"));
    }
}
