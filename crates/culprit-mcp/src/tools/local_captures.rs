//! An explicitly installed local reader supplies fresh canonical originals; live Snapshot statistics stay separate.
use super::*;
use culprit_core::{
    limits::OperationHandle,
    serialization::{JsonSizeError, json_size},
    telemetry::{CollectionStop, DeliveryProblem},
};
use culprit_engine::{LocalCaptureAnalysisKind, LocalCaptureRequest, LocalCapturedAnalysis};

mod hyperscale_log_components;
mod memory_clerks;

const MAX_OUTPUT_BYTES: usize = 64 * 1024;
pub(crate) const LOCAL_CAPTURE_TOOL: &str = "analyze_capture";

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct LocalCaptureArgs {
    connection: String,
    observation_id: String,
    #[schemars(with = "String")]
    analysis: LocalCaptureAnalysisKind,
}

pub(super) fn definition() -> Tool {
    culprit_core::i18n::scope("en", || {
        let mut schema = schema_for_type::<LocalCaptureArgs>().as_ref().clone();
        if let Some(Value::Object(properties)) = schema.get_mut("properties") {
            properties.insert("analysis".into(), json!({"type": "string", "enum": LocalCaptureAnalysisKind::ALL}));
        }
        Tool::new(
            LOCAL_CAPTURE_TOOL,
            format!("{}{DATA_NOTE}", culprit_core::t!("view.hyperscale_log_components.intake.mcp_description")),
            schema,
        )
        .with_title(culprit_core::t!("mcp.local_capture.analyze.title"))
        .with_annotations(ToolAnnotations::new().read_only(true).destructive(false).open_world(false))
    })
}

impl CulpritMcp {
    pub(super) async fn analyze_capture(&self, value: Value, operation: Option<OperationHandle>) -> Result<Value> {
        let reader = self.local_captures.as_ref().ok_or_else(|| unknown_tool(LOCAL_CAPTURE_TOOL))?;
        let args: LocalCaptureArgs = args(value)?;
        self.allowed(&args.connection)?;
        let operation = operation.ok_or_else(|| DeliveryProblem::InvalidContract.error())?;
        check_operation(&operation)?;
        let analysis = self
            .ws
            .analyze_local_capture(
                reader.as_ref(),
                LocalCaptureRequest {
                    connection: &args.connection,
                    observation_id: &args.observation_id,
                    operation: operation.clone(),
                    analysis: args.analysis,
                },
            )
            .await?;
        check_operation(&operation)?;
        // A delegation revoked while a queued read ran cannot publish its result.
        self.allowed(&args.connection)?;
        let value = match analysis {
            LocalCapturedAnalysis::PageServerReads(analysis) => {
                crate::waits_view::page_server_reads::captured(&analysis)?
            }
            LocalCapturedAnalysis::MemoryClerks(analysis) => memory_clerks::captured(&analysis)?,
            LocalCapturedAnalysis::HyperscaleLogComponents(analysis) => hyperscale_log_components::captured(&analysis)?,
        };
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
