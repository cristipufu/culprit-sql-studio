//! An explicitly installed local reader supplies fresh canonical originals; live Snapshot statistics stay separate.
use super::*;
use culprit_core::{
    limits::OperationHandle,
    serialization::{JsonSizeError, json_size},
    telemetry::{CollectionStop, DeliveryProblem},
};
use culprit_engine::LocalCaptureRequest;

const MAX_OUTPUT_BYTES: usize = 64 * 1024;
pub(crate) const LOCAL_CAPTURE_TOOL: &str = "captured_page_server_reads";

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct LocalCaptureArgs {
    connection: String,
    observation_id: String,
}

pub(super) fn definition() -> Tool {
    culprit_core::i18n::scope("en", || {
        Tool::new(
            LOCAL_CAPTURE_TOOL,
            format!("{}{DATA_NOTE}", culprit_core::t!("mcp.local_capture.description")),
            schema_for_type::<LocalCaptureArgs>(),
        )
        .with_title(culprit_core::t!("mcp.local_capture.title"))
        .with_annotations(ToolAnnotations::new().read_only(true).destructive(false).open_world(false))
    })
}

impl CulpritMcp {
    pub(super) async fn captured_page_server_reads(
        &self,
        value: Value,
        operation: Option<OperationHandle>,
    ) -> Result<Value> {
        let reader = self.local_captures.as_ref().ok_or_else(|| unknown_tool(LOCAL_CAPTURE_TOOL))?;
        let args: LocalCaptureArgs = args(value)?;
        self.allowed(&args.connection)?;
        let operation = operation.ok_or_else(|| DeliveryProblem::InvalidContract.error())?;
        check_operation(&operation)?;
        let analysis = reader
            .page_server_reads(LocalCaptureRequest {
                connection: &args.connection,
                observation_id: &args.observation_id,
                operation: operation.clone(),
            })
            .await?;
        check_operation(&operation)?;
        // A delegation revoked while a queued read ran cannot publish its result.
        self.allowed(&args.connection)?;
        let value = crate::waits_view::page_server_reads::captured(&analysis)?;
        json_size(&value, MAX_OUTPUT_BYTES).map_err(|error| match error {
            JsonSizeError::LimitExceeded => DeliveryProblem::Capacity.error(),
            JsonSizeError::Serialization => DeliveryProblem::Integrity.error(),
        })?;
        check_operation(&operation)?;
        Ok(value)
    }
}

pub(crate) fn check_operation(operation: &OperationHandle) -> Result<()> {
    if operation.is_cancelled() {
        return Err(CollectionStop::Cancelled.error());
    }
    if operation.remaining().is_zero() {
        return Err(CollectionStop::TimedOut.error());
    }
    Ok(())
}

#[cfg(test)]
mod tests;
