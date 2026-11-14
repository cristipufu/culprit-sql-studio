use super::*;

impl DiagnosticRevision {
    /// A state transition owns its next revision and evidence inputs. Replaying an unchanged state emits nothing.
    pub fn between(before: Option<&DiagnosticEpisode>, mut next: DiagnosticEpisode, at: &str) -> Result<Option<Self>> {
        if before == Some(&next) {
            return Ok(None);
        }
        if let Some(before) = before {
            if !before.is_valid() || before.id != next.id || before.first != next.first || before.signal != next.signal
            {
                return Err(invalid());
            }
            next.revision = before
                .revision
                .parse::<u64>()
                .ok()
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
        let mut revision = Self {
            id: String::new(),
            controller_version: DIAGNOSTIC_CONTROLLER_VERSION,
            episode: next,
            previous_revision: before.map(|episode| episode.revision.clone()),
            inputs,
            analyzed_at: at.into(),
        };
        revision.id = revision.expected_id()?;
        if !revision.is_valid() {
            return Err(invalid());
        }
        Ok(Some(revision))
    }
}
