//! Explanations of retained native memory failures. A retained observation is not a current database opportunity
//! or proof that a contemporaneous query owned the exhausted memory.

use culprit_core::operations::MemoryFailureSnapshot;
use culprit_core::telemetry::MssqlOutOfMemoryObservation;
use culprit_core::{
    Error, RetainedFailureExplanation, RetainedFailureReport, SourceOutcome, SourceState, format, key, t,
};

/// Scan and authenticated resource readback use the same native facts and source qualification. Capture bounds
/// describe the read; this view never compares the native offset-free event time with those UTC bounds.
pub fn explain(
    evidence: &MemoryFailureSnapshot,
    source: Option<&SourceOutcome>,
) -> culprit_core::Result<RetainedFailureReport> {
    validate_input(evidence, source)?;
    let cases = evidence
        .observations
        .iter()
        .map(|observation| MemoryFailureCase { observation, cause: NativeMemoryCause::of(observation.oom_cause) });
    let mut limitations = vec![
        t!("view.memory_failures.limit.retention"),
        t!("view.memory_failures.limit.clock"),
        t!("view.memory_failures.limit.attribution"),
    ];
    if evidence.resource.is_none() {
        limitations.push(t!("view.memory_failures.limit.unowned"));
    }
    match source.map(|source| source.state) {
        None => limitations.push(t!("view.memory_failures.limit.source_unknown")),
        Some(SourceState::Partial) => limitations.push(t!("view.memory_failures.limit.source_partial")),
        Some(SourceState::Unavailable | SourceState::NotApplicable) => {
            limitations.push(t!("view.memory_failures.limit.source_unavailable"));
        }
        Some(SourceState::Collected | SourceState::Empty) => {}
    }
    Ok(RetainedFailureReport {
        source_state: source.map(|source| source.state),
        source_reason: source.and_then(|source| source.reason.clone()),
        title: t!("view.memory_failures.title"),
        summary: if evidence.observations.is_empty() {
            t!("view.memory_failures.summary.empty")
        } else {
            t!(
                "view.memory_failures.summary.retained",
                n = evidence.observations.len(),
                count = format::count(evidence.observations.len() as f64)
            )
        },
        limitations,
        entries: cases.map(render).collect(),
    })
}

fn validate_input(evidence: &MemoryFailureSnapshot, source: Option<&SourceOutcome>) -> culprit_core::Result<()> {
    let inconsistent_source = source.is_some_and(|source| {
        source.source != evidence.source
            || (!evidence.observations.is_empty()
                && matches!(source.state, SourceState::Empty | SourceState::Unavailable | SourceState::NotApplicable))
    });
    if !evidence.is_valid() || inconsistent_source {
        return Err(Error::invalid(t!("view.memory_failures.invalid_capture")));
    }
    Ok(())
}

struct MemoryFailureCase<'a> {
    observation: &'a MssqlOutOfMemoryObservation,
    cause: NativeMemoryCause,
}

/// These are the source's heuristic classifications. They identify an investigation, never a query owner.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum NativeMemoryCause {
    Unknown,
    InMemoryOltp,
    EngineMemory,
    OperatingSystem,
    OtherInstances,
    NonSosLeak,
    ServerlessReclamation,
    SosLeak,
    BufferPoolShrink,
    InternalPool,
    SystemPool,
    QueryGrants,
    ReplicaWorkload,
    Unrecognized,
}

impl NativeMemoryCause {
    fn of(code: u8) -> Self {
        match code {
            0 => Self::Unknown,
            1 => Self::InMemoryOltp,
            2 => Self::EngineMemory,
            3 => Self::OperatingSystem,
            4 => Self::OtherInstances,
            5 => Self::NonSosLeak,
            6 => Self::ServerlessReclamation,
            7 => Self::SosLeak,
            8 => Self::BufferPoolShrink,
            9 => Self::InternalPool,
            10 => Self::SystemPool,
            11 => Self::QueryGrants,
            12 => Self::ReplicaWorkload,
            _ => Self::Unrecognized,
        }
    }

    fn messages(self) -> (&'static str, &'static str, &'static str) {
        match self {
            Self::Unknown => (
                key!("view.memory_failures.cause.unknown.title"),
                key!("view.memory_failures.cause.unknown.explanation"),
                key!("view.memory_failures.cause.unknown.investigation"),
            ),
            Self::InMemoryOltp => (
                key!("view.memory_failures.cause.in_memory.title"),
                key!("view.memory_failures.cause.in_memory.explanation"),
                key!("view.memory_failures.cause.in_memory.investigation"),
            ),
            Self::EngineMemory => (
                key!("view.memory_failures.cause.engine.title"),
                key!("view.memory_failures.cause.engine.explanation"),
                key!("view.memory_failures.cause.engine.investigation"),
            ),
            Self::OperatingSystem => (
                key!("view.memory_failures.cause.os.title"),
                key!("view.memory_failures.cause.os.explanation"),
                key!("view.memory_failures.cause.os.investigation"),
            ),
            Self::OtherInstances => (
                key!("view.memory_failures.cause.other_instances.title"),
                key!("view.memory_failures.cause.other_instances.explanation"),
                key!("view.memory_failures.cause.other_instances.investigation"),
            ),
            Self::NonSosLeak => (
                key!("view.memory_failures.cause.non_sos.title"),
                key!("view.memory_failures.cause.non_sos.explanation"),
                key!("view.memory_failures.cause.non_sos.investigation"),
            ),
            Self::ServerlessReclamation => (
                key!("view.memory_failures.cause.serverless.title"),
                key!("view.memory_failures.cause.serverless.explanation"),
                key!("view.memory_failures.cause.serverless.investigation"),
            ),
            Self::SosLeak => (
                key!("view.memory_failures.cause.sos.title"),
                key!("view.memory_failures.cause.sos.explanation"),
                key!("view.memory_failures.cause.sos.investigation"),
            ),
            Self::BufferPoolShrink => (
                key!("view.memory_failures.cause.shrink.title"),
                key!("view.memory_failures.cause.shrink.explanation"),
                key!("view.memory_failures.cause.shrink.investigation"),
            ),
            Self::InternalPool => (
                key!("view.memory_failures.cause.internal.title"),
                key!("view.memory_failures.cause.internal.explanation"),
                key!("view.memory_failures.cause.internal.investigation"),
            ),
            Self::SystemPool => (
                key!("view.memory_failures.cause.system.title"),
                key!("view.memory_failures.cause.system.explanation"),
                key!("view.memory_failures.cause.system.investigation"),
            ),
            Self::QueryGrants => (
                key!("view.memory_failures.cause.grants.title"),
                key!("view.memory_failures.cause.grants.explanation"),
                key!("view.memory_failures.cause.grants.investigation"),
            ),
            Self::ReplicaWorkload => (
                key!("view.memory_failures.cause.replicas.title"),
                key!("view.memory_failures.cause.replicas.explanation"),
                key!("view.memory_failures.cause.replicas.investigation"),
            ),
            Self::Unrecognized => (
                key!("view.memory_failures.cause.unrecognized.title"),
                key!("view.memory_failures.cause.unrecognized.explanation"),
                key!("view.memory_failures.cause.unknown.investigation"),
            ),
        }
    }
}

fn render(case: MemoryFailureCase<'_>) -> RetainedFailureExplanation {
    let (title, explanation, investigation) = case.cause.messages();
    RetainedFailureExplanation {
        position: case.observation.position,
        title: culprit_core::i18n::t(title, &[]),
        explanation: culprit_core::i18n::t(explanation, &[]),
        investigation: culprit_core::i18n::t(investigation, &[]),
        verify: t!("view.memory_failures.verify"),
    }
}

#[cfg(test)]
mod tests;
