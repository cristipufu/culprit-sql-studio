//! Tracked session allocations and independently observed statement context never establish an allocating query.
use super::memory_participants;
use culprit_core::{
    Error, MemoryParticipantExplanation, MemoryParticipantsReport, Result, SourceOutcome, format, key, t,
    telemetry::{MAX_SESSION_MEMORY_OBSERVATIONS, SessionMemoryObservation},
};
use std::collections::BTreeSet;

pub fn explain(
    observations: &[&SessionMemoryObservation],
    source: Option<&SourceOutcome>,
) -> Result<MemoryParticipantsReport> {
    let mut cases = validate(observations, source)?;
    cases.sort_by(|left, right| left.key.cmp(&right.key));
    let mut limitations = vec![
        t!("view.session_memory.limit.observation"),
        t!("view.session_memory.limit.tracked_subset"),
        t!("view.session_memory.limit.high_water"),
        t!("view.session_memory.limit.context"),
        t!("view.session_memory.limitation.monitoring"),
    ];
    if observations.iter().any(|row| !row.tracking_enabled) {
        limitations.push(t!("view.session_memory.limit.disabled"));
    }
    limitations.extend(memory_participants::source_limitation(source));
    Ok(MemoryParticipantsReport {
        source_state: source.map(|source| source.state),
        source_reason: source.and_then(|source| source.reason.clone()),
        title: t!("view.session_memory.title"),
        summary: if observations.is_empty() {
            t!("view.session_memory.summary.empty")
        } else {
            t!(
                "view.session_memory.summary.participants",
                n = observations.len(),
                count = format::count(observations.len() as f64)
            )
        },
        limitations,
        entries: cases.into_iter().map(render).collect(),
    })
}

struct Participant<'a> {
    observation: &'a SessionMemoryObservation,
    key: String,
}

fn validate<'a>(
    observations: &[&'a SessionMemoryObservation],
    source: Option<&SourceOutcome>,
) -> Result<Vec<Participant<'a>>> {
    if observations.len() > MAX_SESSION_MEMORY_OBSERVATIONS
        || !memory_participants::consistent_source(source, observations.iter().map(|row| row.source.as_str()))
    {
        return Err(invalid());
    }
    let mut keys = BTreeSet::new();
    observations
        .iter()
        .map(|observation| {
            if !observation.is_valid() {
                return Err(invalid());
            }
            let key = observation.key()?;
            if !keys.insert(key.clone()) {
                return Err(invalid());
            }
            Ok(Participant { observation, key })
        })
        .collect()
}

fn render(participant: Participant<'_>) -> MemoryParticipantExplanation {
    let row = participant.observation;
    let (title, explanation) = if !row.tracking_enabled {
        (t!("view.session_memory.disabled.title"), t!("view.session_memory.disabled.explanation"))
    } else if let Some(bytes) = &row.current_bytes {
        (
            t!("view.session_memory.tracked.title", bytes = format::digits(bytes.clone())),
            t!("view.session_memory.tracked.explanation"),
        )
    } else {
        (t!("view.session_memory.unknown.title"), t!("view.session_memory.unknown.explanation"))
    };
    let mut details = vec![explanation];
    details.push(if row.current_context.is_some() {
        t!("view.session_memory.context.observed")
    } else {
        t!("view.session_memory.context.unavailable")
    });
    for (value, message) in [
        (&row.current_bytes, key!("view.session_memory.metric.current")),
        (&row.controlled_current_bytes, key!("view.session_memory.metric.controlled_current")),
        (&row.high_water_bytes, key!("view.session_memory.metric.high_water")),
        (&row.controlled_high_water_bytes, key!("view.session_memory.metric.controlled_high_water")),
    ] {
        if let Some(value) = value {
            details.push(culprit_core::i18n::t(message, &[("n", format::digits(value.clone()).into())]));
        }
    }
    MemoryParticipantExplanation {
        observation_key: participant.key,
        observation_identity: row.identity.clone(),
        title,
        explanation: details.join(" "),
        investigation: t!("view.session_memory.investigation"),
        verify: t!("view.session_memory.verify"),
    }
}

fn invalid() -> Error {
    Error::invalid(t!("view.session_memory.invalid_capture"))
}

#[cfg(test)]
mod tests;
