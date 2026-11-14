//! Canonical row and current-head policies are identical across warehouse query dialects.
use culprit_core::{
    Result,
    telemetry::{DeliveryProblem, TelemetryStream},
};
#[derive(Clone, Copy)]
pub enum CurrentScope {
    Activity,
    NativeSource,
}
pub struct StreamSemantics {
    pub latest_revision: bool,
    pub current: Option<CurrentScope>,
}
pub fn stream_semantics(stream: TelemetryStream) -> Result<StreamSemantics> {
    use TelemetryStream::*;
    let (latest_revision, current) = match stream {
        ActivityObservations => (false, Some(CurrentScope::Activity)),
        OperationalSamples | WaitCounterSamples | ReplicationSamples | SettingObservations | TuningObservations
        | ObjectDefinitions => (false, Some(CurrentScope::NativeSource)),
        QueryRuntime | QueryWaits | QueryDefinitions | PlanDefinitions | ResourceSamples | DeadlockEvents => {
            (true, None)
        }
        DiagnosticCheckpoints => return Err(DeliveryProblem::UnsupportedSchema.error()),
    };
    Ok(StreamSemantics { latest_revision, current })
}
