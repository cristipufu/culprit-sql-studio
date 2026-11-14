use super::*;

impl DiagnosticRevision {
    /// Native state transitions preserve the compact owner-event reference. Only with_investigation can change it.
    pub fn between(before: Option<&DiagnosticEpisode>, next: DiagnosticEpisode, at: &str) -> Result<Option<Self>> {
        if before.and_then(|episode| episode.investigation_event.as_ref()) != next.investigation_event.as_ref() {
            return Err(invalid());
        }
        Self::transition(before, next, None, at)
    }

    /// Install exact immutable owner provenance independently of native lifecycle and analysis status.
    /// An unchanged event/state emits nothing; a different attempt remains history even at the same status.
    pub fn with_investigation(
        before: &DiagnosticEpisode,
        mut next: DiagnosticEpisode,
        event: DiagnosticInvestigationEvent,
        at: &str,
    ) -> Result<Option<Self>> {
        if !event.matches_episode(before) || !event.matches_episode(&next) {
            return Err(invalid());
        }
        let key = event.key()?;
        let changed = before.investigation_event.as_ref() != Some(&key);
        next.investigation_event = Some(key);
        let investigation = changed.then(|| Box::new(event));
        Self::transition(Some(before), next, investigation, at)
    }

    fn transition(
        before: Option<&DiagnosticEpisode>,
        mut next: DiagnosticEpisode,
        investigation: Option<Box<DiagnosticInvestigationEvent>>,
        at: &str,
    ) -> Result<Option<Self>> {
        if let Some(before) = before
            && (!before.is_valid()
                || before.id != next.id
                || before.first != next.first
                || before.signal != next.signal)
        {
            return Err(invalid());
        }
        if before == Some(&next) {
            return Ok(None);
        }
        if let Some(before) = before {
            next.revision = positive(&before.revision)
                .and_then(|revision| revision.checked_add(1))
                .ok_or_else(invalid)?
                .to_string();
        }
        let mut inputs = vec![next.latest.clone()];
        if let Some(report) = &next.report
            && !inputs.contains(report.evidence())
        {
            inputs.push(report.evidence().clone());
        }
        if let Some(event) = &investigation
            && !inputs.contains(&event.intent.focus.parent)
        {
            inputs.push(event.intent.focus.parent.clone());
        }
        let mut revision = Self {
            id: String::new(),
            controller_version: DIAGNOSTIC_CONTROLLER_VERSION,
            episode: next,
            previous_revision: before.map(|episode| episode.revision.clone()),
            inputs,
            analyzed_at: at.into(),
            investigation,
        };
        revision.id = revision.expected_id()?;
        if !revision.is_valid() {
            return Err(invalid());
        }
        Ok(Some(revision))
    }
}
