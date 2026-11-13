use culprit_core::{
    MemoryParticipantsReport, PageWaitReport, RetainedFailureReport, SourceState, StatementWorkReport, format, t,
};

struct Summary<'a> {
    title: &'a str,
    summary: &'a str,
    source_state: Option<SourceState>,
    source_reason: Option<&'a str>,
    limitations: &'a [String],
}

pub(crate) fn retained(report: &RetainedFailureReport) -> String {
    render(
        Summary {
            title: &report.title,
            summary: &report.summary,
            source_state: report.source_state,
            source_reason: report.source_reason.as_deref(),
            limitations: &report.limitations,
        },
        report.entries.iter().map(|entry| entry.title.as_str()),
    )
}

pub(crate) fn participants(report: &MemoryParticipantsReport) -> String {
    render(
        Summary {
            title: &report.title,
            summary: &report.summary,
            source_state: report.source_state,
            source_reason: report.source_reason.as_deref(),
            limitations: &report.limitations,
        },
        report.entries.iter().map(|entry| entry.title.as_str()),
    )
}

pub(crate) fn page_waits(report: &PageWaitReport) -> String {
    let titles = report
        .entries
        .iter()
        .take(super::BRIEF_FINDING_LIMIT)
        .map(|entry| {
            t!(
                "evidence.cli.page_wait_entry",
                title = entry.title.as_str(),
                session = entry.request_key.session.id.as_str(),
                resource = entry.resource.native.id.as_str()
            )
        })
        .collect::<Vec<_>>();
    render(
        Summary {
            title: &report.title,
            summary: &report.summary,
            source_state: report.source_state,
            source_reason: report.source_reason.as_deref(),
            limitations: &report.limitations,
        },
        titles.iter().map(String::as_str),
    )
}

pub(crate) fn statement_work(report: &StatementWorkReport) -> String {
    let titles = report
        .entries
        .iter()
        .take(super::BRIEF_FINDING_LIMIT)
        .map(|entry| {
            let work = if entry.work.is_empty() {
                t!("evidence.cli.statement_work_unmeasured")
            } else {
                format::list(&entry.work.iter().map(|work| work.title.clone()).collect::<Vec<_>>())
            };
            t!(
                "evidence.cli.statement_work_entry",
                query = entry.identity.query_id.as_str(),
                observed = entry.observed_at.as_str(),
                work = work
            )
        })
        .collect::<Vec<_>>();
    render(
        Summary {
            title: &report.title,
            summary: &report.summary,
            source_state: report.source_state,
            source_reason: report.source_reason.as_deref(),
            limitations: &report.limitations,
        },
        titles.iter().map(String::as_str),
    )
}

fn render<'a>(report: Summary<'_>, titles: impl Iterator<Item = &'a str>) -> String {
    let mut text = format!("{}\n{}", report.title, report.summary);
    let qualification = match report.source_state {
        Some(SourceState::Collected | SourceState::Empty) => None,
        Some(SourceState::Partial) => Some(t!("evidence.cli.source_partial")),
        Some(SourceState::Unavailable | SourceState::NotApplicable) => Some(t!("evidence.cli.source_unavailable")),
        None => Some(t!("evidence.cli.source_unknown")),
    };
    for line in qualification.iter().map(String::as_str).chain(report.source_reason) {
        text.push('\n');
        text.push_str(line);
    }
    super::append_brief_titles(&mut text, titles);
    // Pure views put their general observation qualification first; words never drive a decision here.
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
            let text = retained(&report);
            assert!(text.contains(&culprit_core::i18n::t(key, &[])));
            assert!(text.contains("native source reason"));
            assert!(text.contains("native retained-buffer limitation"));
        }
    }

    #[test]
    fn participant_summaries_keep_partial_source_qualification_and_bound_native_entry_titles() {
        let report = MemoryParticipantsReport {
            source_state: Some(SourceState::Partial),
            source_reason: Some("native ownership remains unresolved".into()),
            title: "observed request-memory participants".into(),
            summary: "one waiter and one reservation".into(),
            limitations: vec!["only the recorded observation is covered".into()],
            entries: (0..8)
                .map(|index| culprit_core::MemoryParticipantExplanation {
                    observation_key: format!("native-{index}"),
                    observation_identity: culprit_core::telemetry::NativeIdentity::new(format!("native-{index}")),
                    title: "x".repeat(1000),
                    explanation: "not included in concise output".into(),
                    investigation: "resource-first investigation".into(),
                    verify: "equivalent work".into(),
                })
                .collect(),
        };
        let text = participants(&report);
        assert!(text.contains(&t!("evidence.cli.source_partial")));
        assert!(text.contains(report.source_reason.as_deref().unwrap()));
        assert!(text.contains(&report.limitations[0]));
        assert_eq!(text.matches('•').count(), super::super::BRIEF_FINDING_LIMIT);
        assert_eq!(
            text.chars().filter(|character| *character == 'x').count(),
            super::super::BRIEF_FINDING_LIMIT * super::super::BRIEF_TITLE_CHARS
        );
        assert!(!text.contains("native-"));
        assert!(!text.contains("not included in concise output"));
    }

    #[test]
    fn page_wait_summary_names_observed_session_and_resource_without_repeating_native_payloads() {
        let report = PageWaitReport {
            source_state: Some(SourceState::Partial),
            source_reason: Some("page metadata visibility was partial".into()),
            title: "observed page waits".into(),
            summary: "one original waiter".into(),
            limitations: vec!["an observed waiter is not a transient latch owner".into()],
            entries: vec![culprit_core::PageWaitExplanation {
                observation_key: "native-key".into(),
                request_key: culprit_core::telemetry::ActivityKey {
                    session: culprit_core::telemetry::NativeActivityIdentity {
                        id: "61".into(),
                        generation: "native-generation".into(),
                    },
                    request: Some(culprit_core::telemetry::NativeActivityIdentity {
                        id: "0".into(),
                        generation: "request-generation".into(),
                    }),
                    transaction: None,
                },
                resource: culprit_core::ObservedWaitResource {
                    kind: culprit_core::WaitResourceKind::MemoryPage,
                    native: culprit_core::telemetry::NativeIdentity::new("5:1:42"),
                },
                title: "memory-page wait".into(),
                explanation: "private detailed explanation".into(),
                investigation: "resource investigation".into(),
                verify: "equivalent load".into(),
            }],
        };
        let text = page_waits(&report);
        assert!(text.contains("61") && text.contains("5:1:42"));
        assert!(text.contains(&t!("evidence.cli.source_partial")));
        assert!(text.contains(&report.limitations[0]));
        assert!(!text.contains("native-generation") && !text.contains("private detailed explanation"));
    }

    #[test]
    fn statement_work_summary_retains_point_identity_and_source_qualification_without_repeating_native_rows() {
        let report = StatementWorkReport {
            source_state: Some(SourceState::Partial),
            source_reason: Some("native source is incomplete".into()),
            title: "native statement work".into(),
            summary: "counter points, not a measured interval".into(),
            limitations: vec!["native reset remains unknown".into()],
            entries: (0..8)
                .map(|index| culprit_core::StatementWorkEntry {
                    observation_key: "proof-reference".into(),
                    identity: culprit_core::telemetry::NativeQueryIdentity {
                        query_id: format!("query-{index}"),
                        qualifiers: Default::default(),
                    },
                    observed_at: "2020-01-01T12:00:00.123456789Z".into(),
                    reset: culprit_core::ResetMarker::Unknown,
                    work: vec![],
                })
                .collect(),
        };
        let text = statement_work(&report);
        assert!(text.contains("query-0") && text.contains("2020-01-01T12:00:00.123456789Z"));
        assert!(text.contains(&t!("evidence.cli.statement_work_unmeasured")));
        assert!(text.contains(&t!("evidence.cli.source_partial")));
        assert!(text.contains("native reset remains unknown"));
        assert_eq!(text.matches('•').count(), super::super::BRIEF_FINDING_LIMIT);
        assert!(!text.contains("proof-reference") && !text.contains("query-7"));
    }
}
