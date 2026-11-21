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
    } else {
        out.done(&status_message(&status)?.render());
    }
    Ok(0)
}

fn status_message(
    status: &culprit_core::telemetry::CollectionAdmissionStatus,
) -> culprit_core::Result<culprit_core::i18n::Message> {
    use culprit_core::telemetry::SourceAdmissionRoute;
    if !status.source_quiescence_required {
        if status.observation_id.is_some() || status.route.is_some() {
            return Err(status_error());
        }
        return Ok(culprit_core::message!("collection.admission.quiet"));
    }
    let observation = status.observation_id.as_ref().ok_or_else(status_error)?;
    let route = status.route.as_ref().filter(|route| route.is_valid()).ok_or_else(status_error)?;
    match route {
        SourceAdmissionRoute::SavedConnection { name } | SourceAdmissionRoute::SavedConnectionTest { name, .. } => {
            Ok(culprit_core::message!("collection.admission.pending", observation = observation, connection = name))
        }
        SourceAdmissionRoute::UnsavedConnectionTest { invocation_id } if invocation_id == observation => {
            Ok(culprit_core::message!("collection.admission.pending_test", observation = observation))
        }
        _ => Err(status_error()),
    }
}
fn status_error() -> culprit_core::Error {
    culprit_core::Error::new(culprit_core::ErrorCode::QueryFailed, culprit_core::t!("collection.admission.storage"))
}
#[cfg(test)]
mod tests {
    use super::*;
    use culprit_core::telemetry::{CollectionAdmissionStatus, SourceAdmissionRoute};
    const ID: &str = "0123456789abcdef0123456789abcdef";
    #[test]
    fn unsaved_pending_work_has_its_own_route_message_without_a_fabricated_saved_name() {
        let status = CollectionAdmissionStatus {
            observation_id: Some(ID.into()),
            route: Some(SourceAdmissionRoute::UnsavedConnectionTest { invocation_id: ID.into() }),
            source_quiescence_required: true,
        };
        assert_eq!(
            status_message(&status).unwrap().render(),
            culprit_core::t!("collection.admission.pending_test", observation = ID)
        );
        let saved = CollectionAdmissionStatus {
            route: Some(SourceAdmissionRoute::SavedConnection { name: "production".into() }),
            ..status
        };
        assert_eq!(
            status_message(&saved).unwrap().render(),
            culprit_core::t!("collection.admission.pending", observation = ID, connection = "production")
        );
    }
    #[test]
    fn missing_or_contradictory_route_never_renders_pending_work_as_quiet() {
        let pending = CollectionAdmissionStatus {
            observation_id: Some(ID.into()),
            route: None,
            source_quiescence_required: true,
        };
        assert!(status_message(&pending).is_err());
        let changed = CollectionAdmissionStatus {
            route: Some(SourceAdmissionRoute::UnsavedConnectionTest {
                invocation_id: "1123456789abcdef0123456789abcdef".into(),
            }),
            ..pending.clone()
        };
        assert!(status_message(&changed).is_err());
        let inconsistent = CollectionAdmissionStatus { source_quiescence_required: false, ..pending };
        assert!(status_message(&inconsistent).is_err());
        let quiet = CollectionAdmissionStatus { observation_id: None, route: None, source_quiescence_required: false };
        assert_eq!(status_message(&quiet).unwrap().render(), culprit_core::t!("collection.admission.quiet"));
    }
}
