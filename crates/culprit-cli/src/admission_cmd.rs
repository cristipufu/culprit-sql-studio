use crate::output::{Format, Out};
use clap::Subcommand;
use culprit_core::Result;
use culprit_engine::Workspace;

#[derive(Subcommand)]
pub(crate) enum AdmissionCommand {
    #[command(about=culprit_core::t!("collection.admission.status"))]
    Status,
    #[command(about=culprit_core::t!("collection.admission.acknowledge"))]
    Acknowledge {
        observation: String,
        #[arg(long,required=true,help=culprit_core::t!("outbox.cli.source_quiescent"))]
        source_quiescent: bool,
    },
}

pub(crate) async fn run(workspace: &Workspace, command: AdmissionCommand, out: &Out) -> Result<u8> {
    let status = match command {
        AdmissionCommand::Status => workspace.collection_admission_status().await?,
        AdmissionCommand::Acknowledge { observation, source_quiescent } => {
            if !source_quiescent {
                return Err(culprit_core::Error::invalid(culprit_core::t!("outbox.cli.source_quiescent")));
            }
            workspace.acknowledge_collection_quiescence(&observation).await?
        }
    };
    if out.format() == Format::Json {
        out.value(
            &serde_json::to_value(&status)
                .map_err(|_| culprit_core::Error::internal(culprit_core::t!("collection.manifest_failed")))?,
        );
    } else if let (Some(observation), Some(connection)) = (&status.observation_id, &status.connection) {
        out.done(&culprit_core::t!("collection.admission.pending", observation = observation, connection = connection));
    } else {
        out.done(&culprit_core::t!("collection.admission.quiet"));
    }
    Ok(0)
}
