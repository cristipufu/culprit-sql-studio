//! Page scenarios adapt their admitted fixture to the shared native capture owner.
use super::fixture::{Fixture, invalid};
pub(super) use crate::test_support::owned_captures::{Captured, OwnedCaptures};
use culprit_core::{Result, SourceState, telemetry::*};
mod dispatch_gate;
pub(super) use dispatch_gate::capture_at_gate_release;

impl OwnedCaptures {
    pub(super) fn new(fixture: &Fixture) -> Result<Self> {
        Self::from_pool(
            fixture.pool.as_ref().ok_or_else(|| invalid("owned fixture pool absent"))?.clone(),
            fixture.port,
        )
    }
}

pub(super) fn require_readable_capture(capture: &Captured) -> Result<()> {
    let metrics = capture.manifest.read_metrics.as_ref().ok_or_else(|| invalid("native source accounting absent"))?;
    // OwnedCaptures already authenticates canonical persisted rows, completion and confirmed shutdown. This
    // scenario additionally refuses swallowed source errors; a truthful limitation is not compatibility proof.
    require_successful_reads(metrics)?;
    require_readable_source(capture.source()?)
}

pub(super) fn require_successful_reads(metrics: &ReadWorkMetrics) -> Result<()> {
    if metrics.templates.iter().any(|template| template.failures != 0) {
        return Err(invalid("owned native compatibility capture included a failed source read"));
    }
    Ok(())
}

pub(super) fn require_readable_source(source: &CollectionDescription) -> Result<()> {
    let coverage = match &source.observation {
        SourceObservation::Activity(activity) => std::slice::from_ref(&activity.coverage),
        observation => observation
            .snapshot()
            .map(|snapshot| snapshot.sources.as_slice())
            .ok_or_else(|| invalid("owned native compatibility capture omitted its declared source coverage"))?,
    };
    if coverage.is_empty()
        || coverage.iter().any(|source| {
            matches!(source.state, SourceState::Unavailable | SourceState::NotApplicable)
                || source.error_code.is_some()
                || source.stopped.is_some()
        })
    {
        return Err(invalid("owned native compatibility capture did not read its declared source successfully"));
    }
    Ok(())
}
