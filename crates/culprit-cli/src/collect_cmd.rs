use crate::output::{Format, Out};
use clap::{Args, Subcommand, ValueEnum};
use culprit_collector::capture::CollectionCancellation;
use culprit_core::{
    Result,
    telemetry::{CollectionBudget, CollectionRequest, CollectionStatus},
};
use culprit_engine::Workspace;
use std::path::PathBuf;

#[derive(Subcommand)]
pub(crate) enum CollectCommand {
    #[command(about=culprit_core::t!("delivery.cli.schema"))]
    Schema(SchemaArgs),
    #[command(subcommand,about=culprit_core::t!("service.cli.about"))]
    Service(crate::service_cmd::ServiceCommand),
    #[command(about=culprit_core::t!("collection.cli.once"))]
    Once(OnceArgs),
    #[command(subcommand,about=culprit_core::t!("outbox.cli.about"))]
    Outbox(crate::outbox_cmd::OutboxCommand),
    #[command(subcommand,about=culprit_core::t!("collection.admission.about"))]
    Admission(crate::admission_cmd::AdmissionCommand),
    #[command(about=culprit_core::t!("activity.cli.about"))]
    Activity(crate::activity_cmd::ActivityArgs),
    #[command(about=culprit_core::t!("fleet.cli.about"))]
    Fleet(crate::fleet_cmd::FleetArgs),
}

#[derive(Args)]
pub(crate) struct OnceArgs {
    connection: String,
    #[arg(long, value_enum, default_value_t=CollectionChoice::QueryHistory, help=culprit_core::t!("collection.cli.collection"))]
    collection: CollectionChoice,
    #[arg(long,requires="until",required_unless_present="collection",required_if_eq_any=[("collection","query-history"),("collection","resource-history"),("collection","deadlock-history")],help=culprit_core::t!("collection.cli.from"))]
    from: Option<String>,
    #[arg(long,requires="from",required_unless_present="collection",required_if_eq_any=[("collection","query-history"),("collection","resource-history"),("collection","deadlock-history")],help=culprit_core::t!("collection.cli.until"))]
    until: Option<String>,
    #[arg(long,required_unless_present="outbox",conflicts_with="outbox",help=culprit_core::t!("collection.cli.output"))]
    output: Option<PathBuf>,
    #[arg(long,help=culprit_core::t!("outbox.cli.destination"))]
    outbox: Option<PathBuf>,
    #[command(flatten)]
    budget: BudgetArgs,
    #[command(flatten)]
    tuning: ActivityTuning,
    #[command(flatten)]
    identity: AzureIdentityArgs,
}

#[derive(Args)]
pub(crate) struct AzureIdentityArgs {
    #[arg(long,value_name="FILE",help=culprit_core::t!("collection.cli.azure_identity"))]
    azure_identity: Option<PathBuf>,
}
impl AzureIdentityArgs {
    pub(crate) fn configure(&self, workspace: Workspace) -> Result<Workspace> {
        match &self.azure_identity {
            Some(path) => Ok(workspace
                .with_database_tokens(std::sync::Arc::new(culprit_auth_azure::AzureCredential::from_file(path)?))),
            None => Ok(workspace),
        }
    }
}

#[derive(Args)]
pub(crate) struct BudgetArgs {
    #[arg(long,default_value_t=CollectionBudget::default().page_rows,help=culprit_core::t!("collection.cli.page_rows"))]
    page_rows: u32,
    #[arg(long,default_value_t=CollectionBudget::default().page_bytes,help=culprit_core::t!("collection.cli.page_bytes"))]
    page_bytes: u32,
    #[arg(long,default_value_t=CollectionBudget::default().total_bytes,help=culprit_core::t!("collection.cli.max_bytes"))]
    max_bytes: u32,
    #[arg(long,default_value_t=CollectionBudget::default().max_sql_bytes,help=culprit_core::t!("collection.cli.max_sql_bytes"))]
    max_sql_bytes: u32,
    #[arg(long,default_value_t=CollectionBudget::default().max_plan_bytes,help=culprit_core::t!("collection.cli.max_plan_bytes"))]
    max_plan_bytes: u32,
    #[arg(long,default_value_t=CollectionBudget::default().max_pages,help=culprit_core::t!("collection.cli.max_pages"))]
    max_pages: u32,
    #[arg(long,default_value_t=CollectionBudget::default().max_queries,help=culprit_core::t!("collection.cli.max_queries"))]
    max_queries: u32,
    #[arg(long,default_value_t=CollectionBudget::default().statement_timeout_ms,help=culprit_core::t!("collection.cli.statement_timeout"))]
    statement_timeout_ms: u32,
    #[arg(long,default_value_t=CollectionBudget::default().total_timeout_ms,help=culprit_core::t!("collection.cli.timeout"))]
    timeout_ms: u32,
    #[arg(long,default_value_t=CollectionBudget::default().max_snapshot_rows,help=culprit_core::t!("activity.cli.max_rows"))]
    max_snapshot_rows: u32,
}
impl BudgetArgs {
    pub(crate) fn budget(&self) -> CollectionBudget {
        CollectionBudget {
            page_rows: self.page_rows,
            max_snapshot_rows: self.max_snapshot_rows,
            page_bytes: self.page_bytes,
            total_bytes: self.max_bytes,
            max_sql_bytes: self.max_sql_bytes,
            max_plan_bytes: self.max_plan_bytes,
            max_pages: self.max_pages,
            max_queries: self.max_queries,
            statement_timeout_ms: self.statement_timeout_ms,
            total_timeout_ms: self.timeout_ms,
        }
    }
}
#[derive(Args)]
pub(crate) struct ActivityTuning {
    #[arg(long,default_value_t=culprit_core::telemetry::ActivityPolicy::default().blocked_after_ms/1000,help=culprit_core::t!("activity.cli.blocked"))]
    blocked_seconds: u32,
    #[arg(long,default_value_t=culprit_core::telemetry::ActivityPolicy::default().running_after_ms/1000,help=culprit_core::t!("activity.cli.running"))]
    running_seconds: u32,
    #[arg(long,default_value_t=culprit_core::telemetry::ActivityPolicy::default().update_every_ms/1000,help=culprit_core::t!("activity.cli.update"))]
    update_seconds: u32,
}
impl ActivityTuning {
    pub(crate) fn policy(&self) -> Result<culprit_core::telemetry::ActivityPolicy> {
        let milliseconds = |seconds: u32| {
            seconds
                .checked_mul(1000)
                .ok_or_else(|| culprit_core::Error::invalid(culprit_core::t!("collection.invalid_budget")))
        };
        let policy = culprit_core::telemetry::ActivityPolicy {
            blocked_after_ms: milliseconds(self.blocked_seconds)?,
            running_after_ms: milliseconds(self.running_seconds)?,
            update_every_ms: milliseconds(self.update_seconds)?,
        };
        policy.validate()?;
        Ok(policy)
    }
}

#[derive(Clone, Copy, ValueEnum)]
enum CollectionChoice {
    StatementCounters,
    StatementDefinitions,
    Sequences,
    Cleanup,
    QueryHistory,
    ResourceHistory,
    DeadlockHistory,
    Activity,
    Operational,
    WaitCounters,
    Replication,
    Settings,
    PlanCacheSummary,
    StatementGrants,
    IndexOperations,
    Tables,
    ForeignKeys,
    Columns,
    Statistics,
    Indexes,
    CheckConstraints,
    MissingIndexes,
    QueryCompilation,
    CachedRuntime,
    Objects,
    Triggers,
    Partitions,
    CachedDefinitions,
    RetainedPlans,
    Tuning,
}

impl CollectionChoice {
    fn snapshot_dataset(self) -> Option<culprit_core::telemetry::SnapshotDataset> {
        use culprit_core::telemetry::SnapshotDataset;
        Some(match self {
            Self::StatementCounters => SnapshotDataset::StatementCounters,
            Self::StatementDefinitions => SnapshotDataset::StatementDefinitions,
            Self::Sequences => SnapshotDataset::Sequences,
            Self::Cleanup => SnapshotDataset::Cleanup,
            Self::Operational => SnapshotDataset::Operational,
            Self::WaitCounters => SnapshotDataset::WaitCounters,
            Self::Replication => SnapshotDataset::Replication,
            Self::Settings => SnapshotDataset::Settings,
            Self::PlanCacheSummary => SnapshotDataset::PlanCacheSummary,
            Self::StatementGrants => SnapshotDataset::StatementGrants,
            Self::IndexOperations => SnapshotDataset::IndexOperations,
            Self::Tables => SnapshotDataset::Tables,
            Self::ForeignKeys => SnapshotDataset::ForeignKeys,
            Self::Columns => SnapshotDataset::Columns,
            Self::Statistics => SnapshotDataset::Statistics,
            Self::Indexes => SnapshotDataset::Indexes,
            Self::CheckConstraints => SnapshotDataset::CheckConstraints,
            Self::MissingIndexes => SnapshotDataset::MissingIndexes,
            Self::QueryCompilation => SnapshotDataset::QueryCompilation,
            Self::CachedRuntime => SnapshotDataset::CachedRuntime,
            Self::Objects => SnapshotDataset::Objects,
            Self::Triggers => SnapshotDataset::Triggers,
            Self::Partitions => SnapshotDataset::Partitions,
            Self::CachedDefinitions => SnapshotDataset::CachedDefinitions,
            Self::RetainedPlans => SnapshotDataset::RetainedPlans,
            Self::Tuning => SnapshotDataset::Tuning,
            Self::QueryHistory | Self::ResourceHistory | Self::DeadlockHistory | Self::Activity => return None,
        })
    }
}

impl OnceArgs {
    fn request(&self) -> Result<CollectionRequest> {
        use culprit_core::telemetry::CollectionSelection;
        let selection =
            if let Some(dataset) = self.collection.snapshot_dataset() {
                if self.from.is_some() || self.until.is_some() {
                    return Err(culprit_core::Error::invalid(culprit_core::t!("collection.snapshot_no_window")));
                }
                CollectionSelection::Snapshot { dataset }
            } else {
                match self.collection {
                    CollectionChoice::Activity => {
                        if self.from.is_some() || self.until.is_some() {
                            return Err(culprit_core::Error::invalid(culprit_core::t!("activity.cli.no_window")));
                        }
                        CollectionSelection::Activity { policy: self.tuning.policy()? }
                    }
                    CollectionChoice::QueryHistory
                    | CollectionChoice::ResourceHistory
                    | CollectionChoice::DeadlockHistory => {
                        let from = self.from.clone().ok_or_else(|| {
                            culprit_core::Error::invalid(culprit_core::t!("collection.invalid_interval"))
                        })?;
                        let until = self.until.clone().ok_or_else(|| {
                            culprit_core::Error::invalid(culprit_core::t!("collection.invalid_interval"))
                        })?;
                        if matches!(self.collection, CollectionChoice::QueryHistory) {
                            CollectionSelection::QueryHistory { from, until }
                        } else if matches!(self.collection, CollectionChoice::DeadlockHistory) {
                            CollectionSelection::DeadlockHistory { from, until }
                        } else {
                            CollectionSelection::ResourceHistory { from, until }
                        }
                    }
                    _ => unreachable!("snapshot datasets were handled above"),
                }
            };
        let request = CollectionRequest { selection, budget: self.budget.budget() };
        request.validate()?;
        Ok(request)
    }
}

pub(crate) fn run(command: CollectCommand, out: &Out) -> crate::CommandTask<'_> {
    match command {
        CollectCommand::Schema(args) => crate::command_task(|| async { schema(args, out) }),
        CollectCommand::Service(command) => crate::command_task(|| crate::service_cmd::run(command, out)),
        CollectCommand::Outbox(command) => crate::command_task(|| crate::outbox_cmd::run(command, out)),
        CollectCommand::Once(args) => crate::command_task(|| once(args, out)),
        CollectCommand::Activity(args) => crate::command_task(|| async move {
            let ws = args.identity.configure(Workspace::open()?)?;
            crate::activity_cmd::run(&ws, args, out).await
        }),
        CollectCommand::Fleet(args) => crate::command_task(|| async move {
            let ws = args.identity.configure(Workspace::open()?)?;
            crate::fleet_cmd::run(&ws, args, out).await
        }),
        CollectCommand::Admission(command) => {
            crate::command_task(|| async move { crate::admission_cmd::run(&Workspace::open()?, command, out).await })
        }
    }
}
async fn once(args: OnceArgs, out: &Out) -> Result<u8> {
    let ws = args.identity.configure(Workspace::open()?)?;
    let request = args.request()?;
    let cancellation = CollectionCancellation::default();
    let directory = args
        .output
        .as_deref()
        .or(args.outbox.as_deref())
        .ok_or_else(|| culprit_core::Error::invalid(culprit_core::t!("outbox.cli.destination")))?;
    let mut outbox = args.outbox.as_deref().map(culprit_collector::outbox::Outbox::open).transpose()?;
    let collecting = async {
        match &mut outbox {
            Some(outbox) => ws.collect_into_outbox(&args.connection, request, outbox, &cancellation).await,
            None => ws.collect_once(&args.connection, request, directory, &cancellation).await,
        }
    };
    let result = crate::collection_shutdown::run(&cancellation, collecting).await?;
    let complete = matches!(result.status, CollectionStatus::Complete);
    if out.format() == Format::Json {
        out.value(
            &serde_json::to_value(&result)
                .map_err(|_| culprit_core::Error::internal(culprit_core::t!("collection.manifest_failed")))?,
        );
    } else if complete {
        out.done(&culprit_core::t!("collection.cli.complete", directory = directory.display().to_string()));
    } else {
        println!(
            "{}",
            culprit_core::t!(
                "collection.cli.incomplete",
                manifest = if args.outbox.is_some() {
                    directory.join("captures").join(&result.capture.observation_id).join("manifest.json")
                } else {
                    directory.join("manifest.json")
                }
                .display()
                .to_string()
            )
        );
    }
    Ok(if complete { 0 } else { 2 })
}
#[derive(Args)]
pub(crate) struct SchemaArgs {
    #[arg(long, value_enum, default_value_t=StoreChoice::Adx, help=culprit_core::t!("delivery.cli.store"))]
    store: StoreChoice,
    #[arg(long, requires="store", help=culprit_core::t!("delivery.cli.replicated"))]
    replicated: bool,
}
#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
enum StoreChoice {
    Adx,
    Clickhouse,
}
pub(crate) fn schema(args: SchemaArgs, out: &Out) -> Result<u8> {
    use std::io::Write;
    if args.replicated && args.store != StoreChoice::Clickhouse {
        return Err(culprit_core::Error::invalid(culprit_core::t!("delivery.cli.replicated_requires_clickhouse")));
    }
    let (value, script) = match args.store {
        StoreChoice::Adx => {
            let assets = culprit_store_adx::deployment::DeploymentAssets::new()?;
            (serde_json::to_value(&assets), assets.script())
        }
        StoreChoice::Clickhouse => {
            use culprit_store_clickhouse::deployment::{DeploymentAssets, TableLayout};
            let layout = if args.replicated { TableLayout::Replicated } else { TableLayout::SingleNode };
            let assets = DeploymentAssets::with_layout(layout)?;
            (serde_json::to_value(&assets), assets.script())
        }
    };
    if out.format() == Format::Json {
        out.value(&value.map_err(|_| culprit_core::Error::internal(culprit_core::t!("collection.manifest_failed")))?);
    } else {
        std::io::stdout()
            .lock()
            .write_all(script.as_bytes())
            .map_err(|_| culprit_core::Error::internal(culprit_core::t!("activity.cli.output_failed")))?;
    }
    Ok(0)
}

#[cfg(test)]
mod tests {
    #[test]
    fn point_collections_choose_their_dataset_and_reject_historical_windows() {
        use clap::Parser;
        use culprit_core::telemetry::{SnapshotDataset, TelemetryCollection};
        for (name, dataset) in [
            ("statement-counters", SnapshotDataset::StatementCounters),
            ("statement-definitions", SnapshotDataset::StatementDefinitions),
            ("sequences", SnapshotDataset::Sequences),
            ("cleanup", SnapshotDataset::Cleanup),
            ("operational", SnapshotDataset::Operational),
            ("wait-counters", SnapshotDataset::WaitCounters),
            ("replication", SnapshotDataset::Replication),
            ("settings", SnapshotDataset::Settings),
            ("plan-cache-summary", SnapshotDataset::PlanCacheSummary),
            ("tuning", SnapshotDataset::Tuning),
            ("statement-grants", SnapshotDataset::StatementGrants),
            ("index-operations", SnapshotDataset::IndexOperations),
            ("tables", SnapshotDataset::Tables),
            ("foreign-keys", SnapshotDataset::ForeignKeys),
            ("columns", SnapshotDataset::Columns),
            ("statistics", SnapshotDataset::Statistics),
            ("indexes", SnapshotDataset::Indexes),
            ("check-constraints", SnapshotDataset::CheckConstraints),
            ("missing-indexes", SnapshotDataset::MissingIndexes),
            ("query-compilation", SnapshotDataset::QueryCompilation),
            ("cached-runtime", SnapshotDataset::CachedRuntime),
            ("objects", SnapshotDataset::Objects),
            ("triggers", SnapshotDataset::Triggers),
            ("partitions", SnapshotDataset::Partitions),
            ("cached-definitions", SnapshotDataset::CachedDefinitions),
            ("retained-plans", SnapshotDataset::RetainedPlans),
        ] {
            let base = ["culprit", "collect", "once", "pilot", "--collection", name, "--output", "capture"];
            let cli = crate::Cli::try_parse_from(base).unwrap();
            let crate::Command::Collect(super::CollectCommand::Once(args)) = cli.command else { panic!("once") };
            assert_eq!(args.request().unwrap().collection(), TelemetryCollection::Snapshot(dataset));
            let cli = crate::Cli::try_parse_from(base.into_iter().chain([
                "--from",
                "2020-01-01T00:00:00Z",
                "--until",
                "2020-01-01T01:00:00Z",
            ]))
            .unwrap();
            let crate::Command::Collect(super::CollectCommand::Once(args)) = cli.command else { panic!("once") };
            assert!(args.request().is_err());
        }
    }
    #[test]
    fn unattended_identity_configuration_applies_to_both_collection_entry_points() {
        use clap::Parser;
        for arguments in [
            vec!["once", "pilot", "--collection", "activity", "--output", "capture"],
            vec!["activity", "pilot", "--outbox", "outbox"],
        ] {
            let parsed = crate::Cli::try_parse_from(
                ["culprit", "collect"].into_iter().chain(arguments).chain(["--azure-identity", "identity.json"]),
            )
            .unwrap();
            let identity = match parsed.command {
                crate::Command::Collect(super::CollectCommand::Once(args)) => args.identity,
                crate::Command::Collect(super::CollectCommand::Activity(args)) => args.identity,
                _ => panic!("collection entry point"),
            };
            assert_eq!(identity.azure_identity.as_deref(), Some(std::path::Path::new("identity.json")));
        }
    }
    use super::*;
    use clap::Parser;

    #[test]
    fn resource_history_uses_the_shared_collector_with_its_own_stream_set() {
        let cli = crate::Cli::try_parse_from([
            "culprit",
            "collect",
            "once",
            "pilot",
            "--collection",
            "resource-history",
            "--from",
            "2020-01-01T06:00:00Z",
            "--until",
            "2020-01-01T06:01:00Z",
            "--outbox",
            "outbox",
        ])
        .unwrap();
        let crate::Command::Collect(CollectCommand::Once(args)) = cli.command else { panic!("wrong command") };
        assert_eq!(args.request().unwrap().collection(), culprit_core::telemetry::TelemetryCollection::ResourceHistory);
        assert_eq!(
            args.request().unwrap().collection().streams(),
            &[culprit_core::telemetry::TelemetryStream::ResourceSamples]
        );
    }

    #[test]
    fn collection_requires_an_explicit_interval_and_destination() {
        assert!(crate::Cli::try_parse_from(["culprit", "collect", "once", "pilot"]).is_err());
        let cli = crate::Cli::try_parse_from([
            "culprit",
            "collect",
            "once",
            "pilot",
            "--from",
            "2020-01-01T06:00:00Z",
            "--until",
            "2020-01-01T07:00:00Z",
            "--output",
            "observation",
        ])
        .unwrap();
        let crate::Command::Collect(CollectCommand::Once(args)) = cli.command else { panic!("wrong command") };
        assert!(args.request().unwrap().validate().is_ok());
        assert_eq!(args.request().unwrap().budget, CollectionBudget::default());
    }

    #[test]
    fn safety_acknowledgement_requires_an_explicit_quiescence_assertion() {
        let arguments = ["culprit", "collect", "admission", "acknowledge", "0123456789abcdef0123456789abcdef"];
        assert!(crate::Cli::try_parse_from(arguments).is_err());
        assert!(crate::Cli::try_parse_from(arguments.into_iter().chain(["--source-quiescent"])).is_ok());
        assert!(crate::Cli::try_parse_from(["culprit", "collect", "admission", "status", "--json"]).is_ok());
    }

    #[test]
    fn a_capture_chooses_exactly_one_directory_or_outbox() {
        let base = [
            "culprit",
            "collect",
            "once",
            "pilot",
            "--from",
            "2020-01-01T06:00:00Z",
            "--until",
            "2020-01-01T07:00:00Z",
        ];
        assert!(crate::Cli::try_parse_from(base).is_err());
        assert!(crate::Cli::try_parse_from(base.into_iter().chain(["--outbox", "journal"])).is_ok());
        assert!(
            crate::Cli::try_parse_from(base.into_iter().chain(["--outbox", "journal", "--output", "export"])).is_err()
        );
        assert!(
            crate::Cli::try_parse_from(["culprit", "collect", "outbox", "acknowledge", "journal", "capture"]).is_err()
        );
        assert!(
            crate::Cli::try_parse_from([
                "culprit",
                "collect",
                "outbox",
                "acknowledge",
                "journal",
                "capture",
                "--source-quiescent"
            ])
            .is_ok()
        );
    }
    #[test]
    fn activity_is_an_instantaneous_read_and_rejects_historical_windows() {
        let base = ["culprit", "collect", "once", "pilot", "--collection", "activity", "--outbox", "journal"];
        let cli = crate::Cli::try_parse_from(base).unwrap();
        let crate::Command::Collect(CollectCommand::Once(args)) = cli.command else { panic!("wrong command") };
        assert_eq!(args.request().unwrap().collection(), culprit_core::telemetry::TelemetryCollection::Activity);
        let cli = crate::Cli::try_parse_from(base.into_iter().chain([
            "--from",
            "2020-01-01T00:00:00Z",
            "--until",
            "2020-01-02T00:00:00Z",
        ]))
        .unwrap();
        let crate::Command::Collect(CollectCommand::Once(args)) = cli.command else { panic!("wrong command") };
        assert!(args.request().is_err());
        assert!(crate::Cli::try_parse_from(["culprit", "collect", "once", "pilot", "--outbox", "journal"]).is_err());
    }
}
