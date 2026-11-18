//! Explicit native experiments and local approval administration; input JSON is a request, never authority.
use super::*;
use culprit_collector::outbox::Outbox;

#[derive(Subcommand)]
pub(crate) enum QualificationCommand {
    #[command(about=culprit_core::t!("recipe_qualification.cli.run"))]
    Run(RunArgs),
    #[command(about=culprit_core::t!("recipe_qualification.cli.inspect"))]
    Inspect(StoredArgs),
    #[command(about=culprit_core::t!("recipe_qualification.cli.revoke"))]
    Revoke(RevokeArgs),
}
#[derive(Args)]
pub(crate) struct RunArgs {
    connection: String,
    #[arg(long)]
    recipe: String,
    #[arg(long, value_name = "DIRECTORY")]
    outbox: PathBuf,
    #[arg(long, value_name = "FILE")]
    selection: PathBuf,
    #[arg(long, value_name = "FILE")]
    resource: Option<PathBuf>,
    #[command(flatten)]
    budget: BudgetArgs,
    #[command(flatten)]
    identity: AzureIdentityArgs,
}
#[derive(Args)]
pub(crate) struct StoredArgs {
    connection: String,
    #[arg(long, value_name = "DIRECTORY")]
    outbox: PathBuf,
}
#[derive(Args)]
pub(crate) struct RevokeArgs {
    connection: String,
    #[arg(long)]
    recipe: String,
    #[arg(long, value_name = "DIRECTORY")]
    outbox: PathBuf,
}

pub(crate) async fn run(command: QualificationCommand, out: &Out) -> Result<u8> {
    match command {
        QualificationCommand::Run(args) => {
            let request = CollectionRequest {
                subject: requested_subject(args.resource.as_deref())?,
                selection: requested_selection(&args.selection)?,
                budget: args.budget.budget(),
            };
            request.validate()?;
            let workspace = args.identity.configure(Workspace::open()?)?;
            let mut outbox = if args.outbox.exists() {
                Outbox::open(&args.outbox)?
            } else {
                Outbox::create(&args.outbox, Default::default())?
            };
            let cancellation = CollectionCancellation::default();
            let result = crate::collection_shutdown::run(
                &cancellation,
                workspace.qualify_recipe(&args.connection, &args.recipe, request, &mut outbox, &cancellation),
            )
            .await?;
            value(out, &result)?;
            Ok(if result.state == culprit_core::RecipeQualificationState::Installed { 0 } else { 2 })
        }
        QualificationCommand::Inspect(args) => {
            let outbox = Outbox::open(&args.outbox)?;
            let workspace = Workspace::open()?;
            let cancellation = CollectionCancellation::default();
            let delivery = outbox.delivery();
            let status = crate::collection_shutdown::run(
                &cancellation,
                workspace.recipe_qualification_status(&args.connection, &delivery, &cancellation),
            )
            .await?;
            value(out, &status)?;
            Ok(0)
        }
        QualificationCommand::Revoke(args) => {
            let outbox = Outbox::open(&args.outbox)?;
            let workspace = Workspace::open()?;
            workspace.revoke_recipe_qualification(&args.connection, &args.recipe, &outbox.delivery()).await?;
            let cancellation = CollectionCancellation::default();
            value(
                out,
                &workspace.recipe_qualification_status(&args.connection, &outbox.delivery(), &cancellation).await?,
            )?;
            Ok(0)
        }
    }
}
fn value(out: &Out, value: &impl serde::Serialize) -> Result<()> {
    out.value(
        &serde_json::to_value(value)
            .map_err(|_| culprit_core::Error::internal(culprit_core::t!("collection.manifest_failed")))?,
    );
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[test]
    fn measurement_requires_explicit_connection_recipe_request_and_outbox_not_an_approval_file() {
        let args = [
            "culprit",
            "collect",
            "qualification",
            "run",
            "selected",
            "--recipe",
            "mssql.query-compilation",
            "--outbox",
            "owned-outbox",
            "--selection",
            "request.json",
        ];
        let parsed = crate::Cli::try_parse_from(args).unwrap();
        let crate::Command::Collect(CollectCommand::Qualification(QualificationCommand::Run(request))) = parsed.command
        else {
            panic!()
        };
        assert_eq!(request.connection, "selected");
        assert_eq!(request.recipe, "mssql.query-compilation");
        assert_eq!(request.selection, PathBuf::from("request.json"));
        assert!(
            crate::Cli::try_parse_from([
                "culprit",
                "collect",
                "qualification",
                "run",
                "selected",
                "--recipe",
                "mssql.query-compilation",
                "--outbox",
                "owned-outbox"
            ])
            .is_err()
        );
        let mut imported = args.to_vec();
        imported.extend(["--receipt", "caller-approval.json"]);
        assert!(crate::Cli::try_parse_from(imported).is_err());
    }
    #[test]
    fn inspect_and_revoke_require_only_the_original_saved_name_and_explicit_outbox() {
        for action in ["inspect", "revoke"] {
            let mut args =
                vec!["culprit", "collect", "qualification", action, "removed-name", "--outbox", "owned-outbox"];
            if action == "revoke" {
                args.extend(["--recipe", "mssql.query-compilation"]);
            }
            assert!(crate::Cli::try_parse_from(args).is_ok());
        }
        assert!(
            crate::Cli::try_parse_from([
                "culprit",
                "collect",
                "qualification",
                "revoke",
                "selected",
                "--outbox",
                "owned-outbox"
            ])
            .is_err()
        );
    }
}
