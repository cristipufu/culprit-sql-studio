//! An explicitly installed local reader supplies fresh canonical originals; live Snapshot statistics stay separate.
use super::*;
use culprit_core::{
    limits::OperationHandle,
    serialization::{JsonSizeError, json_size},
    telemetry::{CollectionStop, DeliveryProblem},
};
use culprit_engine::{LocalCaptureAnalysisKind, LocalCaptureRequest, LocalCapturedAnalysis};

mod blocking;
mod context;
mod hyperscale_log_components;
mod memory_clerks;
mod pool_settings;
mod replication;
mod session_census;
mod snapshots;

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
            format!("{}{DATA_NOTE}", culprit_core::t!("mcp.local_capture.analyze.snapshot_description")),
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
            LocalCapturedAnalysis::ManagedInstanceResources(analysis) => {
                managed_instance_resources::captured(&analysis)
            }
            LocalCapturedAnalysis::PageServerReads(analysis) => {
                crate::waits_view::page_server_reads::captured(&analysis)?
            }
            LocalCapturedAnalysis::RuntimeFragments(analysis) => {
                json!({"runtime_fragments":crate::waits_view::runtime_fragments::value(analysis.report())?,"local_verification_bytes":analysis.bytes_read()})
            }
            LocalCapturedAnalysis::QueryWaitFragments(analysis) => {
                json!({"query_wait_fragments": crate::waits_view::query_wait_fragments::value(analysis.report())?, "local_verification_bytes": analysis.bytes_read()})
            }
            LocalCapturedAnalysis::MemoryClerks(analysis) => memory_clerks::captured(&analysis)?,
            LocalCapturedAnalysis::HyperscaleLogComponents(analysis) => hyperscale_log_components::captured(&analysis)?,
            LocalCapturedAnalysis::Snapshot(analysis) => snapshots::captured(&analysis)?,
            LocalCapturedAnalysis::Blocking(analysis) => blocking::captured(&analysis)?,
            LocalCapturedAnalysis::SessionCensus(analysis) => session_census::captured(&analysis)?,
            LocalCapturedAnalysis::Replication(analysis) => replication::captured(&analysis)?,
            LocalCapturedAnalysis::PoolSettings(analysis) => pool_settings::captured(&analysis)?,
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

mod managed_instance_resources;
