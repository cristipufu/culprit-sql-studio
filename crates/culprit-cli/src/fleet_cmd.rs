use crate::{
    collect_cmd::AzureIdentityArgs,
    output::{Format, Out},
};
use clap::Args;
use culprit_collector::{
    capture::CollectionCancellation,
    fleet::{FleetJournal, FleetRunner},
    outbox::Outbox,
};
use culprit_core::{Error, Result, telemetry::*};
use culprit_engine::Workspace;
use std::{
    io::Write,
    path::{Path, PathBuf},
};
use tokio::io::AsyncReadExt;

const MAX_INVENTORY_BYTES: u64 = 16 * 1024 * 1024;
#[derive(Args)]
pub(crate) struct FleetArgs {
    #[arg(long,help=culprit_core::t!("fleet.cli.inventory"))]
    inventory: PathBuf,
    #[arg(long,help=culprit_core::t!("fleet.cli.shard"))]
    shard: String,
    #[arg(long,help=culprit_core::t!("fleet.cli.state"))]
    state: PathBuf,
    #[arg(long,help=culprit_core::t!("outbox.cli.destination"))]
    outbox: PathBuf,
    #[arg(long,value_parser=clap::value_parser!(u32).range(1..),help=culprit_core::t!("fleet.cli.limit"))]
    max_work: Option<u32>,
    #[command(flatten)]
    pub(crate) identity: AzureIdentityArgs,
}
pub(crate) async fn run(ws: &Workspace, args: FleetArgs, out: &Out) -> Result<u8> {
    let inventory = read_inventory(&args.inventory).await?;
    inventory.validate(&args.shard)?;
    let journal = FleetJournal::open(&args.state, &inventory, &args.shard).await?;
    let mut outbox = Outbox::open(&args.outbox)?;
    let recovery = outbox.recover().await?;
    if recovery.source_debts > 0 {
        return Err(CollectionStop::CancellationUnconfirmed.error());
    }
    let mut runner = FleetRunner::new(ws, &inventory, &args.shard, &journal, &mut outbox)?;
    let cancellation = CollectionCancellation::default();
    let summary = crate::collection_shutdown::run(
        &cancellation,
        runner.run(&cancellation, args.max_work, |event| print(event, out)),
    )
    .await?;
    Ok(if summary.incomplete > 0 { 2 } else { 0 })
}
async fn read_inventory(path: &Path) -> Result<FleetInventory> {
    let file = tokio::fs::File::open(path).await.map_err(|_| invalid_inventory())?;
    let mut bytes = Vec::new();
    file.take(MAX_INVENTORY_BYTES + 1).read_to_end(&mut bytes).await.map_err(|_| invalid_inventory())?;
    if bytes.len() as u64 > MAX_INVENTORY_BYTES {
        return Err(invalid_inventory());
    }
    serde_json::from_slice(&bytes).map_err(|_| invalid_inventory())
}
fn invalid_inventory() -> Error {
    Error::invalid(culprit_core::t!("fleet.invalid_inventory"))
}
pub(crate) fn print(event: &FleetCollectionEvent, out: &Out) -> Result<()> {
    let line = if out.format() == Format::Json {
        serde_json::to_string(event).map_err(|_| Error::internal(culprit_core::t!("collection.manifest_failed")))?
    } else {
        description(event)
    };
    writeln!(std::io::stdout().lock(), "{line}")
        .map_err(|_| Error::internal(culprit_core::t!("activity.cli.output_failed")))
}

fn description(event: &FleetCollectionEvent) -> String {
    if event.status == CollectionStatus::Complete {
        match event.work {
            FleetWork::Capture => {
                let state = match event.native_coverage {
                    Some(culprit_core::SourceState::Partial) => Some("partial"),
                    Some(culprit_core::SourceState::Unavailable) => Some("unavailable"),
                    Some(culprit_core::SourceState::NotApplicable) => Some("not-applicable"),
                    Some(culprit_core::SourceState::Collected | culprit_core::SourceState::Empty) | None => None,
                };
                if let Some(state) = state {
                    return culprit_core::t!(
                        "fleet.cli.native_coverage",
                        connection = &event.connection,
                        schedule = &event.schedule,
                        state = state
                    );
                }
                culprit_core::t!("fleet.cli.complete", connection = &event.connection, schedule = &event.schedule)
            }
            FleetWork::DiscoverIntervals => {
                culprit_core::t!("fleet.cli.discovered", connection = &event.connection, schedule = &event.schedule)
            }
            FleetWork::SkipCommitted => {
                culprit_core::t!("fleet.cli.skipped", connection = &event.connection, schedule = &event.schedule)
            }
        }
    } else {
        culprit_core::t!("fleet.cli.incomplete", connection = &event.connection, schedule = &event.schedule)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;
    #[test]
    fn a_fleet_requires_explicit_inventory_owner_outbox_and_state() {
        let args = [
            "culprit",
            "collect",
            "fleet",
            "--inventory",
            "inventory.json",
            "--shard",
            "alpha",
            "--state",
            "state",
            "--outbox",
            "outbox",
            "--azure-identity",
            "identity.json",
        ];
        let cli = crate::Cli::try_parse_from(args).unwrap();
        let crate::Command::Collect(crate::collect_cmd::CollectCommand::Fleet(args)) = cli.command else {
            panic!("fleet command")
        };
        assert_eq!(args.shard, "alpha");
        assert!(crate::Cli::try_parse_from(["culprit", "collect", "fleet", "--inventory", "inventory.json"]).is_err());
    }

    #[test]
    fn a_completed_operation_reports_native_coverage_gaps_without_claiming_a_failed_transfer() {
        let mut event = FleetCollectionEvent {
            target: "database".into(),
            connection: "shop".into(),
            schedule: "health".into(),
            work: FleetWork::Capture,
            status: CollectionStatus::Complete,
            native_coverage: None,
            capture: None,
            error_code: None,
            gap: None,
        };
        let complete = description(&event);
        for state in [
            culprit_core::SourceState::Partial,
            culprit_core::SourceState::Unavailable,
            culprit_core::SourceState::NotApplicable,
        ] {
            event.native_coverage = Some(state);
            let text = description(&event);
            assert_ne!(text, complete);
            assert!(text.contains("shop") && text.contains("health"));
            let json = serde_json::to_value(&event).unwrap();
            assert_eq!(json["native_coverage"], serde_json::to_value(state).unwrap());
            assert_eq!(event.status, CollectionStatus::Complete);
        }
        event.native_coverage = Some(culprit_core::SourceState::Empty);
        assert_eq!(description(&event), complete);
    }
}
