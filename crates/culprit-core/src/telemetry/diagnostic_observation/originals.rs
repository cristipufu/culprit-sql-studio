//! Closed positive source conditions feed the existing journal. They never authorize a new native question.
use super::*;

pub struct DiagnosticOriginalRow<'a> {
    pub reference: &'a DiagnosticEvidenceRef,
    pub record: &'a TelemetryRecord,
}
impl DiagnosticOriginalRow<'_> {
    fn verify(&self, source: &CollectionDescription, input: &DiagnosticCaptureRef) -> Result<()> {
        if self.reference.capture_ref() != *input {
            return Err(invalid());
        }
        self.reference.validate_canonical_record(source, self.record)
    }
}

/// A complete immutable input roster qualifies request generations once before any point is selected.
/// The caller still authenticates the original manifest; this owner never opens a source or grants an action.
pub struct DiagnosticOriginalEvidence {
    source: CollectionDescription,
    cleared_boundary_source: Option<String>,
    groups: BTreeMap<String, OriginalGroup>,
    conditions: BTreeMap<String, DiagnosticOriginalCondition>,
}
struct OriginalGroup {
    signal: DiagnosticSignal,
    references: Vec<DiagnosticEvidenceRef>,
}
impl DiagnosticOriginalEvidence {
    pub fn new(
        source: &CollectionDescription,
        input: &DiagnosticCaptureRef,
        rows: &[DiagnosticOriginalRow<'_>],
    ) -> Result<Self> {
        diagnostic_original_source_kind(source).ok_or_else(invalid)?;
        if source.observation.snapshot().is_some_and(|snapshot| rows.len() != snapshot.records as usize) {
            return Err(invalid());
        }
        let refused = ActivityEvidence::observed_session_generation_refusals(rows.iter().filter_map(|row| {
            let TelemetryRecord::Operational(OperationalObservation { fact: OperationalFact::TaskSpace(task), .. }) =
                row.record
            else {
                return None;
            };
            task.request.as_ref()
        }))?;
        let mut references = BTreeSet::new();
        let mut conditions = BTreeMap::new();
        let mut groups = BTreeMap::<String, OriginalGroup>::new();
        for row in rows {
            row.verify(source, input)?;
            let key = row.reference.key()?;
            if !references.insert(key.clone()) {
                return Err(invalid());
            }
            let Some(condition) = DiagnosticOriginalCondition::from_record(source, row.record) else {
                continue;
            };
            if matches!(&condition, DiagnosticOriginalCondition::MssqlTemporarySpace { request, .. }
                if refused.contains(&request.session))
            {
                continue;
            }
            let signal = DiagnosticSignal::OriginalObservation {
                condition: condition.clone(),
                native_key: row.record.native_identity(),
            };
            let group = groups.entry(signal.key()?).or_insert_with(|| OriginalGroup { signal, references: vec![] });
            if group.references.len() >= MAX_DIAGNOSTIC_INPUTS {
                return Err(DeliveryProblem::Capacity.error());
            }
            group.references.push(row.reference.clone());
            conditions.insert(key, condition);
        }
        Ok(Self {
            source: source.clone(),
            cleared_boundary_source: cleared_boundary_source(source, rows),
            groups,
            conditions,
        })
    }
    pub fn condition(&self, reference: &DiagnosticEvidenceRef) -> Result<Option<&DiagnosticOriginalCondition>> {
        Ok(self.conditions.get(&reference.key()?))
    }
    pub fn matches_episode(&self, episode: &DiagnosticEpisode) -> bool {
        let Some(group) = episode.signal.key().ok().and_then(|key| self.groups.get(&key)) else {
            return false;
        };
        let Some(DiagnosticReport::OriginalObservation { observation, peers }) = &episode.report else {
            return false;
        };
        episode.is_valid()
            && DiagnosticScope::from_source(&self.source).is_ok_and(|scope| scope == episode.scope)
            && group.references.first() == Some(&episode.latest)
            && observation == &episode.latest
            && group.references.get(1..).is_some_and(|expected| expected == peers)
    }
    pub fn can_resolve(&self, episode: &DiagnosticEpisode) -> bool {
        let DiagnosticSignal::OriginalObservation {
            native_key: NativeRecordIdentity::Operational { source, .. }, ..
        } = &episode.signal
        else {
            return false;
        };
        episode.original_scope_can_resolve(&self.source) && self.cleared_boundary_source.as_ref() == Some(source)
    }
}

/// Reconcile original observation groups inside the existing bounded checkpoint. Repeated slots or native
/// aggregate fragments revise one group; none of these counts is a proven distinct-event or execution count.
pub fn observe_diagnostic_original(
    source: &CollectionDescription,
    input: &DiagnosticCaptureRef,
    rows: &[DiagnosticOriginalRow<'_>],
    previous: Option<&DiagnosticCheckpoint>,
    analyzed_at: &str,
) -> Result<DiagnosticObservationUpdate> {
    let kind = diagnostic_original_source_kind(source).ok_or_else(invalid)?;
    let evidence = DiagnosticOriginalEvidence::new(source, input, rows)?;
    let scope = DiagnosticScope::from_source(source)?;
    if !input.is_valid()
        || source.captured_range()?.scope_id != input.source_scope_id
        || previous.is_some_and(|checkpoint| {
            !checkpoint.is_valid()
                || checkpoint.controller_version != DIAGNOSTIC_CONTROLLER_VERSION
                || checkpoint.target != source.target
        })
    {
        return Err(invalid());
    }
    let at = diagnostic_original_observed_until(source).ok_or_else(invalid)?;
    let mut episodes = previous
        .into_iter()
        .flat_map(|checkpoint| &checkpoint.episodes)
        .map(|episode| (episode.id.clone(), episode.clone()))
        .collect::<BTreeMap<_, _>>();
    let mut by_condition = BTreeMap::new();
    for episode in episodes.values() {
        if episode.lifecycle != DiagnosticLifecycle::Resolved
            && episode.scope.key()? == scope.key()?
            && by_condition.insert(episode.signal.key()?, episode.id.clone()).is_some()
        {
            return Err(invalid());
        }
    }
    let mut revisions = Vec::new();
    let mut observed = BTreeSet::new();
    for (key, group) in &evidence.groups {
        let reference = group.references.first().ok_or_else(invalid)?;
        let before = by_condition.get(key).and_then(|id| episodes.get(id)).cloned();
        let mut episode = match &before {
            Some(before) => {
                if ObservationBounds::new(&before.last_seen_at, at).is_none() {
                    return Err(invalid());
                }
                let mut next = before.clone();
                next.lifecycle = if next.signal.is_capture_point() {
                    DiagnosticLifecycle::Uncertain
                } else {
                    DiagnosticLifecycle::Active
                };
                next.latest = reference.clone();
                next.last_seen_at = at.to_owned();
                next
            }
            None => DiagnosticEpisode::start(scope.clone(), group.signal.clone(), at.to_owned(), reference.clone())?,
        };
        observed.insert(episode.id.clone());
        episode.report = Some(DiagnosticReport::OriginalObservation {
            observation: reference.clone(),
            peers: group.references[1..].to_vec(),
        });
        // Known original conditions do not establish a cause or authorize a new native question.
        episode.analysis = DiagnosticAnalysisStatus::Unresolved;
        episode.issue = Some(DiagnosticAnalysisIssue::Unsupported);
        record_transition(&mut revisions, before.as_ref(), &mut episode, analyzed_at)?;
        episodes.insert(episode.id.clone(), episode);
    }
    for episode in episodes.values_mut() {
        if !matches!(episode.signal, DiagnosticSignal::OriginalObservation { .. })
            || episode.lifecycle == DiagnosticLifecycle::Resolved
            || observed.contains(&episode.id)
            || episode.scope.key()? != scope.key()?
        {
            continue;
        }
        let before = episode.clone();
        if kind == DiagnosticSourceKind::Cleanup && evidence.can_resolve(episode) {
            episode.lifecycle = DiagnosticLifecycle::Resolved;
            episode.recovery = Some(DiagnosticRecoveryProof::CompleteScope { capture: input.clone() });
        } else {
            // Empty retained buffers, rollover, partial history and vanished task rows prove no recovery.
            episode.lifecycle = DiagnosticLifecycle::Uncertain;
        }
        record_transition(&mut revisions, Some(&before), episode, analyzed_at)?;
    }
    if episodes.len() > MAX_DIAGNOSTIC_EPISODES {
        return Err(DeliveryProblem::Capacity.error());
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
    Ok(DiagnosticObservationUpdate { revisions, checkpoint, focuses: vec![] })
}

/// Complete native negative of this one current boundary; zero bytes, another oldest transaction, missing bodies
/// and partial/denied sources establish neither disappearance nor global cleanup health.
fn cleared_boundary_source(source: &CollectionDescription, rows: &[DiagnosticOriginalRow<'_>]) -> Option<String> {
    if diagnostic_original_source_kind(source) != Some(DiagnosticSourceKind::Cleanup)
        || source.target.engine() != crate::Engine::Mssql
    {
        return None;
    }
    let snapshot = source.observation.snapshot()?;
    let mut horizon = None;
    for row in rows {
        let TelemetryRecord::Operational(row) = row.record else {
            return None;
        };
        if let OperationalFact::MssqlCleanupHorizon(boundary) = &row.fact {
            if horizon.replace((row.source.as_str(), boundary)).is_some() {
                return None;
            }
        }
    }
    let (native_source, boundary) = horizon?;
    (boundary.oldest_confirmed.is_none()
        && snapshot.sources.iter().any(|coverage| {
            coverage.source == native_source
                && coverage.records == 1
                && coverage.complete_scope
                && coverage.state == crate::SourceState::Collected
                && coverage.error_code.is_none()
                && coverage.stopped.is_none()
        }))
    .then(|| native_source.to_owned())
}
