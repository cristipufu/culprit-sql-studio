use culprit_core::{RetainedFailureReport, SourceState, t};

pub(crate) fn summary(report: &RetainedFailureReport) -> String {
    let mut text = format!("{}\n{}", report.title, report.summary);
    let qualification = match report.source_state {
        Some(SourceState::Collected | SourceState::Empty) => None,
        Some(SourceState::Partial) => Some(t!("evidence.cli.source_partial")),
        Some(SourceState::Unavailable | SourceState::NotApplicable) => Some(t!("evidence.cli.source_unavailable")),
        None => Some(t!("evidence.cli.source_unknown")),
    };
    for line in qualification.iter().chain(report.source_reason.iter()) {
        text.push('\n');
        text.push_str(line);
    }
    super::append_brief_titles(&mut text, report.entries.iter().map(|entry| entry.title.as_str()));
    // The pure view orders its general coverage/retention limitation first; words never drive a decision here.
    if let Some(limitation) = report.limitations.first() {
        text.push('\n');
        text.push_str(limitation);
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn concise_retained_output_preserves_native_source_qualification_and_reason() {
        let mut report = RetainedFailureReport {
            source_state: None,
            source_reason: Some("native source reason".into()),
            title: "retained diagnostics".into(),
            summary: "observations are not incident counts".into(),
            limitations: vec!["native retained-buffer limitation".into()],
            entries: vec![],
        };
        for (state, key) in [
            (None, "evidence.cli.source_unknown"),
            (Some(SourceState::Partial), "evidence.cli.source_partial"),
            (Some(SourceState::Unavailable), "evidence.cli.source_unavailable"),
            (Some(SourceState::NotApplicable), "evidence.cli.source_unavailable"),
        ] {
            report.source_state = state;
            let text = summary(&report);
            assert!(text.contains(&culprit_core::i18n::t(key, &[])));
            assert!(text.contains("native source reason"));
            assert!(text.contains("native retained-buffer limitation"));
        }
    }
}
