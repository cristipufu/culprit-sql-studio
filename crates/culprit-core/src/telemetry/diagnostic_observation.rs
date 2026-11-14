//! Positive activity facts create durable episodes; only qualified absence can resolve one.
use super::*;
use crate::WaitResourceKind;
use std::collections::{BTreeMap, BTreeSet};

pub struct DiagnosticActivityRow<'a> {
    pub reference: &'a DiagnosticEvidenceRef,
    pub observation: &'a ActivityObservation,
}

pub struct DiagnosticObservationUpdate {
    pub revisions: Vec<DiagnosticRevision>,
    pub checkpoint: DiagnosticCheckpoint,
    pub focuses: Vec<(String, ActivityWaitFocus)>,
}

/// Detection records symptoms and a bounded question. It establishes neither a latch owner nor a causal fix.
pub fn observe_diagnostic_activity(
    source: &CollectionDescription,
    input: &DiagnosticCaptureRef,
    rows: &[DiagnosticActivityRow<'_>],
    previous: Option<&DiagnosticCheckpoint>,
    policy: &ActivityPolicy,
    analyzed_at: &str,
) -> Result<DiagnosticObservationUpdate> {
    let native = source.observation.activity().ok_or_else(invalid)?;
    policy.validate()?;
    if !native.is_valid()
        || !input.is_valid()
        || rows.len() != native.observations as usize
        || source.captured_range()?.scope_id != input.source_scope_id
    {
        return Err(invalid());
    }
    let scope = DiagnosticScope::from_source(source)?;
    let scope_key = scope.key()?;
    if let Some(previous) = previous
        && (!previous.is_valid()
            || previous.controller_version != DIAGNOSTIC_CONTROLLER_VERSION
            || previous.target.telemetry_id()? != source.target.telemetry_id()?)
    {
        return Err(invalid());
    }
    let mut episodes = previous
        .into_iter()
        .flat_map(|checkpoint| &checkpoint.episodes)
        .map(|episode| (episode.id.clone(), episode.clone()))
        .collect::<BTreeMap<_, _>>();
    let mut by_signal = BTreeMap::new();
    for episode in episodes.values() {
        if episode.lifecycle != DiagnosticLifecycle::Resolved
            && episode.scope.key()? == scope_key
            && by_signal.insert(episode.signal.key()?, episode.id.clone()).is_some()
        {
            return Err(invalid());
        }
    }
    let mut observed = BTreeSet::new();
    let mut revisions = Vec::new();
    let mut focuses = Vec::new();
    let mut native_keys = BTreeSet::new();
    let observations = rows.iter().map(|row| row.observation.clone()).collect::<Vec<_>>();
    let classified = ActivityEvidence::select(&observations, policy)?;
    let uncertain_blocking = ActivityEvidence::context_for(&observations, &classified.uncertain)?;
    for row in rows {
        verify_row(source, input, row)?;
        if !native_keys.insert(row.observation.key.clone()) {
            return Err(invalid());
        }
        for (signal, resource) in signals(
            row.observation,
            classified.conditions.get(&row.observation.key),
            uncertain_blocking.contains(&row.observation.key),
            policy,
        ) {
            let before = by_signal.get(&signal.key()?).and_then(|id| episodes.get(id)).cloned();
            let mut episode = match &before {
                Some(before) => {
                    let mut next = before.clone();
                    if ObservationBounds::new(&next.last_seen_at, &native.at).is_none() {
                        return Err(invalid());
                    }
                    next.lifecycle = DiagnosticLifecycle::Active;
                    next.last_seen_at = native.at.clone();
                    next.latest = row.reference.clone();
                    next
                }
                None => DiagnosticEpisode::start(scope.clone(), signal, native.at.clone(), row.reference.clone())?,
            };
            observed.insert(episode.id.clone());
            if let Some(resource) = resource {
                if let (Some(epoch), Some(statement)) = (&native.epoch, &row.observation.statement_identity) {
                    let focus = ActivityWaitFocus {
                        parent: row.reference.clone(),
                        target: source.target.clone(),
                        source: source.source.clone(),
                        epoch: epoch.clone(),
                        original_observed_at: native.at.clone(),
                        original: row.observation.clone(),
                        statement_identity: statement.clone(),
                        resource,
                    };
                    if focus.is_valid() && episode.analysis.needs_work() {
                        focuses.push((episode.id.clone(), focus));
                    } else if !focus.is_valid() && episode.analysis.needs_work() {
                        episode.analysis = DiagnosticAnalysisStatus::Unresolved;
                        episode.issue = Some(DiagnosticAnalysisIssue::MissingNativeIdentity);
                    }
                } else if episode.analysis.needs_work() {
                    episode.analysis = DiagnosticAnalysisStatus::Unresolved;
                    episode.issue = Some(DiagnosticAnalysisIssue::MissingNativeIdentity);
                }
            }
            record_transition(&mut revisions, before.as_ref(), &mut episode, analyzed_at)?;
            episodes.insert(episode.id.clone(), episode);
        }
    }
    let can_resolve = diagnostic_baseline_can_resolve(&scope, native.coverage.state, native.coverage.complete_scope);
    for episode in episodes.values_mut() {
        if observed.contains(&episode.id) || episode.lifecycle == DiagnosticLifecycle::Resolved {
            continue;
        }
        let before = episode.clone();
        // Complete actor inventory can establish that an actor disappeared. A still-visible actor whose
        // condition is no longer measured is uncertainty, not a negative lock/resource observation.
        if episode.scope.key()? == scope_key && can_resolve && !native_keys.contains(episode.signal.activity_key()) {
            episode.lifecycle = DiagnosticLifecycle::Resolved;
            episode.recovery = Some(DiagnosticRecoveryProof::CompleteScope { capture: input.clone() });
        } else {
            episode.lifecycle = DiagnosticLifecycle::Uncertain;
            if matches!(&episode.signal, DiagnosticSignal::Activity { policy: original, .. } if original != policy) {
                episode.issue = Some(DiagnosticAnalysisIssue::PolicyChanged);
            }
        }
        record_transition(&mut revisions, Some(&before), episode, analyzed_at)?;
    }
    let checkpoint = DiagnosticCheckpoint {
        controller_version: DIAGNOSTIC_CONTROLLER_VERSION,
        target: source.target.clone(),
        input: input.clone(),
        episodes: episodes.into_values().filter(DiagnosticEpisode::retained_in_checkpoint).collect(),
    };
    if !checkpoint.is_valid() {
        return Err(invalid());
    }
    Ok(DiagnosticObservationUpdate { revisions, checkpoint, focuses })
}

fn verify_row(
    source: &CollectionDescription,
    input: &DiagnosticCaptureRef,
    row: &DiagnosticActivityRow<'_>,
) -> Result<()> {
    if row.reference.capture_ref() != *input {
        return Err(invalid());
    }
    let exported = ExportedRecord::new(source, &input.capture, TelemetryRecord::from(row.observation.clone()))?;
    let expected =
        DiagnosticEvidenceRef::from_export(source, &input.manifest_sha256, &exported, row.reference.row_ordinal)?;
    if expected != *row.reference {
        return Err(invalid());
    }
    Ok(())
}

fn signals(
    row: &ActivityObservation,
    conditions: Option<&Vec<ActivityCondition>>,
    uncertain_blocking: bool,
    policy: &ActivityPolicy,
) -> Vec<(DiagnosticSignal, Option<crate::ObservedWaitResource>)> {
    if matches!(row.scope, ActivityScopeBasis::OtherDatabase | ActivityScopeBasis::BlockingReference) {
        return Vec::new();
    }
    let mut signals = Vec::new();
    for condition in conditions.into_iter().flatten() {
        if *condition == ActivityCondition::Blocked
            && row.blocking_status == ActivityBlockingStatus::Unknown
            && row.waiting_on_lock != Some(true)
            || *condition == ActivityCondition::IdleBlocker && uncertain_blocking
        {
            continue;
        }
        signals.push((
            DiagnosticSignal::Activity { key: row.key.clone(), condition: *condition, policy: policy.clone() },
            None,
        ));
    }
    if row.key.request.is_none() {
        return signals;
    }
    if row.waiting_on_lock == Some(true) {
        signals.push((DiagnosticSignal::LockWait { key: row.key.clone() }, None));
    }
    for resource in row
        .wait
        .iter()
        .flat_map(|wait| &wait.identified_resources)
        .filter(|resource| matches!(resource.kind, WaitResourceKind::MemoryPage | WaitResourceKind::PageIo))
    {
        signals.push((
            DiagnosticSignal::PageWait {
                key: row.key.clone(),
                statement_identity: row.statement_identity.clone(),
                resource: resource.clone(),
            },
            Some(resource.clone()),
        ));
    }
    signals
}

fn record_transition(
    revisions: &mut Vec<DiagnosticRevision>,
    before: Option<&DiagnosticEpisode>,
    episode: &mut DiagnosticEpisode,
    at: &str,
) -> Result<()> {
    if let Some(revision) = DiagnosticRevision::between(before, episode.clone(), at)? {
        *episode = revision.episode.clone();
        revisions.push(revision);
    }
    Ok(())
}

fn invalid() -> Error {
    CollectionStop::InvalidPage.error()
}

#[cfg(test)]
mod tests;
