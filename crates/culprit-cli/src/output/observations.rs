use culprit_core::{
    CachedPlanEntry, CachedPlanReport, CompilationReport, MemoryParticipantsReport, PageWaitReport,
    RetainedFailureReport, RuntimeDistributionReport, SourceOutcome, SourceState, StatementWorkReport, TaskSpaceReport,
    collected_analysis::{CollectedQueryAnalysis, CollectedQueryAnalysisResult, CollectedQueryPopulation},
    format, t,
};

const OBSERVATION_DETAIL_CHARS: usize = 1024;

struct Summary<'a> {
    title: &'a str,
    summary: &'a str,
    source: SourceQualification<'a>,
    limitations: &'a [String],
}

enum SourceQualification<'a> {
    Single { state: Option<SourceState>, reason: Option<&'a str> },
    Multiple(&'a [SourceOutcome]),
}

pub(crate) fn queries(analysis: &CollectedQueryAnalysis) -> String {
    let mut text = t!(
        "evidence.cli.query_summary",
        range = &analysis.context.range_id,
        findings = format::count(analysis.analyses.iter().map(|result| result.findings.len()).sum::<usize>() as f64),
        limitations =
            format::count(analysis.analyses.iter().map(|result| result.limitations.len()).sum::<usize>() as f64),
        gaps = format::count(analysis.context.read_gaps.len() as f64)
    );
    text.push('\n');
    text.push_str(&t!("evidence.cli.query_source", source = &analysis.context.source.source));
    for result in &analysis.analyses {
        text.push_str("\n\n");
        text.push_str(&query_result(result));
    }
    text
}

fn query_result(result: &CollectedQueryAnalysisResult) -> String {
    let mut text = match &result.population {
        CollectedQueryPopulation::Unknown => t!("evidence.cli.query_population_unknown"),
        CollectedQueryPopulation::Unpartitioned => t!("evidence.cli.query_population_unpartitioned"),
        CollectedQueryPopulation::Population { identity } => {
            let mut title = t!("evidence.cli.query_population", id = &identity.id);
            if !identity.qualifiers.is_empty() {
                let qualifiers =
                    identity.qualifiers.iter().map(|(name, value)| format!("{name}={value:?}")).collect::<Vec<_>>();
                title.push('\n');
                title.push_str(&t!("evidence.cli.query_population_qualifiers", qualifiers = format::list(&qualifiers)));
            }
            title
        }
    };
    text.push('\n');
    text.push_str(&t!(
        "evidence.cli.query_population_summary",
        findings = format::count(result.findings.len() as f64),
        limitations = format::count(result.limitations.len() as f64)
    ));
    if result.workload.is_none() {
        text.push('\n');
        text.push_str(&t!("evidence.cli.query_population_unavailable"));
    }
    super::append_brief_titles(&mut text, result.findings.iter().map(|finding| finding.title.as_str()));
    text
}

pub(crate) fn cleanup(report: &culprit_core::CleanupReport) -> String {
    let entries = report
        .entries
        .iter()
        .take(super::BRIEF_FINDING_LIMIT)
        .map(|entry| t!("evidence.cli.cleanup_entry", title = &entry.title, explanation = &entry.explanation))
        .collect::<Vec<_>>();
    render_summary(
        Summary {
            title: &report.title,
            summary: &report.summary,
            source: SourceQualification::Multiple(&report.sources),
            limitations: &report.limitations,
        },
        |text| super::append_brief_details(text, entries.iter().map(String::as_str), OBSERVATION_DETAIL_CHARS),
    )
}

pub(crate) fn tuning(report: &culprit_core::TuningReport) -> String {
    let title = report.title.render();
    let summary = report.summary.render();
    let limitations = report.limitations.iter().map(|message| message.render()).collect::<Vec<_>>();
    let titles =
        report.entries.iter().take(super::BRIEF_FINDING_LIMIT).map(|entry| entry.title.render()).collect::<Vec<_>>();
    render(
        Summary {
            title: &title,
            summary: &summary,
            source: SourceQualification::Multiple(&report.sources),
            limitations: &limitations,
        },
        titles.iter().map(String::as_str),
    )
}

pub(crate) fn statement_grants(report: &culprit_core::StatementGrantReport) -> String {
    let title = report.title.render();
    let summary = report.summary.render();
    let limitations = report.limitations.iter().map(|message| message.render()).collect::<Vec<_>>();
    let entries = report
        .entries
        .iter()
        .take(super::BRIEF_FINDING_LIMIT)
        .map(|entry| {
            t!(
                "evidence.cli.statement_grant_entry",
                title = entry.title.render(),
                explanation = entry.explanation.render()
            )
        })
        .collect::<Vec<_>>();
    render_summary(
        Summary {
            title: &title,
            summary: &summary,
            source: SourceQualification::Multiple(&report.sources),
            limitations: &limitations,
        },
        |text| super::append_brief_details(text, entries.iter().map(String::as_str), OBSERVATION_DETAIL_CHARS),
    )
}

pub(crate) fn compilation(report: &CompilationReport) -> String {
    render_explanations(
        Summary {
            title: &report.title,
            summary: &report.summary,
            source: SourceQualification::Single { state: report.source_state, reason: report.source_reason.as_deref() },
            limitations: &report.limitations,
        },
        report.entries.iter().map(|entry| (entry.title.as_str(), entry.explanation.as_str())),
    )
}

pub(crate) fn cache_entry_memory(report: &culprit_core::CacheEntryMemoryReport) -> String {
    render(
        Summary {
            title: &report.title,
            summary: &report.summary,
            source: SourceQualification::Single { state: report.source_state, reason: report.source_reason.as_deref() },
            limitations: &report.limitations,
        },
        report.entries.iter().map(|entry| entry.title.as_str()),
    )
}

pub(crate) fn cached_plans(report: &CachedPlanReport) -> String {
    let titles = report
        .entries
        .iter()
        .take(super::BRIEF_FINDING_LIMIT)
        .map(|entry| {
            t!(
                "evidence.cli.cached_plan_entry",
                definition = entry.definition_key.as_str(),
                observations = cached_plan_observations(entry)
            )
        })
        .collect::<Vec<_>>();
    render(
        Summary {
            title: &report.title,
            summary: &report.summary,
            source: SourceQualification::Multiple(&report.sources),
            limitations: &report.limitations,
        },
        titles.iter().map(String::as_str),
    )
}

fn cached_plan_observations(entry: &CachedPlanEntry) -> String {
    let Some(plan) = &entry.plan else {
        return entry
            .limitations
            .first()
            .map(|limitation| limitation.explanation.clone())
            .unwrap_or_else(|| t!("evidence.cli.cached_plan_unavailable"));
    };
    let titles = plan
        .insights
        .as_ref()
        .map(|insights| insights.items.iter().map(|item| item.title.clone()).collect::<Vec<_>>())
        .unwrap_or_default();
    if titles.is_empty() { t!("evidence.cli.cached_plan_unremarkable") } else { format::list(&titles) }
}

pub(crate) fn retained(report: &RetainedFailureReport) -> String {
    render(
        Summary {
            title: &report.title,
            summary: &report.summary,
            source: SourceQualification::Single { state: report.source_state, reason: report.source_reason.as_deref() },
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
            source: SourceQualification::Single { state: report.source_state, reason: report.source_reason.as_deref() },
            limitations: &report.limitations,
        },
        report.entries.iter().map(|entry| entry.title.as_str()),
    )
}

pub(crate) fn task_space(report: &TaskSpaceReport) -> String {
    render(
        Summary {
            title: &report.title,
            summary: &report.summary,
            source: SourceQualification::Single { state: report.source_state, reason: report.source_reason.as_deref() },
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
            source: SourceQualification::Single { state: report.source_state, reason: report.source_reason.as_deref() },
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
            source: SourceQualification::Single { state: report.source_state, reason: report.source_reason.as_deref() },
            limitations: &report.limitations,
        },
        titles.iter().map(String::as_str),
    )
}

fn render<'a>(report: Summary<'_>, titles: impl Iterator<Item = &'a str>) -> String {
    render_summary(report, |text| super::append_brief_titles(text, titles))
}

fn render_explanations<'a>(report: Summary<'_>, entries: impl Iterator<Item = (&'a str, &'a str)>) -> String {
    let entries = entries
        .take(super::BRIEF_FINDING_LIMIT)
        .map(|(title, explanation)| t!("evidence.cli.observation_entry", title = title, explanation = explanation))
        .collect::<Vec<_>>();
    render_summary(report, |text| {
        super::append_brief_details(text, entries.iter().map(String::as_str), OBSERVATION_DETAIL_CHARS);
    })
}

fn render_summary(report: Summary<'_>, append_entries: impl FnOnce(&mut String)) -> String {
    let mut text = format!("{}\n{}", report.title, report.summary);
    match report.source {
        SourceQualification::Single { state, reason } => append_single_source(&mut text, state, reason),
        SourceQualification::Multiple(sources) => append_native_sources(&mut text, sources),
    }
    append_entries(&mut text);
    // Pure views put their general observation qualification first; words never drive a decision here.
    if let Some(limitation) = report.limitations.first() {
        text.push('\n');
        text.push_str(limitation);
    }
    text
}

pub(crate) fn runtime_distributions(report: &RuntimeDistributionReport) -> String {
    render(
        Summary {
            title: &report.title,
            summary: &report.summary,
            source: SourceQualification::Multiple(&report.sources),
            limitations: &report.limitations,
        },
        report.entries.iter().map(|entry| entry.title.as_str()),
    )
}

fn append_single_source(text: &mut String, state: Option<SourceState>, reason: Option<&str>) {
    let qualification = match state {
        Some(SourceState::Collected | SourceState::Empty) => None,
        Some(SourceState::Partial) => Some(t!("evidence.cli.source_partial")),
        Some(SourceState::Unavailable | SourceState::NotApplicable) => Some(t!("evidence.cli.source_unavailable")),
        None => Some(t!("evidence.cli.source_unknown")),
    };
    for line in qualification.iter().map(String::as_str).chain(reason) {
        text.push('\n');
        text.push_str(line);
    }
}

fn append_native_sources(text: &mut String, sources: &[SourceOutcome]) {
    if sources.is_empty() {
        append_single_source(text, None, None);
    }
    for source in sources {
        let message = match source.state {
            SourceState::Collected => culprit_core::key!("evidence.cli.native_source.collected"),
            SourceState::Empty => culprit_core::key!("evidence.cli.native_source.empty"),
            SourceState::Partial => culprit_core::key!("evidence.cli.native_source.partial"),
            SourceState::Unavailable => culprit_core::key!("evidence.cli.native_source.unavailable"),
            SourceState::NotApplicable => culprit_core::key!("evidence.cli.native_source.not_applicable"),
        };
        text.push('\n');
        text.push_str(&culprit_core::i18n::t(message, &[("source", source.source.as_str().into())]));
        if let Some(reason) = &source.reason {
            text.push('\n');
            text.push_str(reason);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_sections_keep_exact_native_context_and_distinguish_unknown_from_unpartitioned_history() {
        let mut result = CollectedQueryAnalysisResult {
            population: CollectedQueryPopulation::Population {
                identity: culprit_core::telemetry::NativeIdentity::new("17")
                    .qualified("native-context", "readonly, original generation"),
            },
            query_inputs: vec![],
            workload: None,
            findings: vec![],
            assessments: vec![],
            locked: vec![],
            limitations: vec![culprit_core::collected_analysis::CollectedAnalysisLimitation {
                record_id: None,
                reason: culprit_core::collected_analysis::CollectedAnalysisLimit::RuntimeDenominatorUnavailable,
            }],
            provenance: None,
        };
        let text = query_result(&result);
        assert!(text.starts_with(&t!("evidence.cli.query_population", id = "17")));
        assert!(text.contains("native-context=\"readonly, original generation\""));
        assert!(text.contains(&t!("evidence.cli.query_population_unavailable")));
        assert!(text.contains(&t!("evidence.cli.query_population_summary", findings = "0", limitations = "1")));
        assert!(!text.contains(&t!("evidence.cli.query_population_unknown")));
        assert!(!text.contains(&t!("evidence.cli.query_population_unpartitioned")));

        result.population = CollectedQueryPopulation::Unknown;
        let unknown = query_result(&result);
        assert!(unknown.starts_with(&t!("evidence.cli.query_population_unknown")));
        assert!(
            !unknown.contains("native-context")
                && !unknown.contains(&t!("evidence.cli.query_population_unpartitioned"))
        );
        result.population = CollectedQueryPopulation::Unpartitioned;
        assert!(query_result(&result).starts_with(&t!("evidence.cli.query_population_unpartitioned")));
    }

    #[test]
    fn cleanup_text_keeps_native_mechanisms_and_source_failures_with_a_bounded_entry_list() {
        let mut report = culprit_core::CleanupReport {
            title: "Cleanup evidence".into(),
            summary: "Eight native observations.".into(),
            sources: vec![SourceOutcome {
                source: "cleanup-source".into(),
                state: SourceState::Partial,
                reason: Some("Original native visibility was incomplete.".into()),
                reason_message: None,
            }],
            boundaries: vec![],
            limitations: vec!["This observation has no qualified rate window.".into()],
            entries: (0..8)
                .map(|id| culprit_core::CleanupExplanation {
                    reference: culprit_core::CleanupReference {
                        source: "cleanup-source".into(),
                        observation_key: format!("row-{id}"),
                        identity: None,
                    },
                    mechanisms: vec![culprit_core::CleanupMechanism::LongSnapshotScan],
                    title: format!("Native cleanup record {id}"),
                    explanation: "Recorded snapshot-scan pages: 9007199254740993.".into(),
                    metrics: vec![],
                    investigation: String::new(),
                    verify: String::new(),
                })
                .collect(),
        };
        let text = cleanup(&report);
        assert_eq!(text.matches('•').count(), super::super::BRIEF_FINDING_LIMIT);
        assert!(text.contains("Recorded snapshot-scan pages: 9007199254740993."));
        assert!(!text.contains("9007199254740992"));
        assert!(text.contains("Original native visibility was incomplete."));
        assert!(text.contains("This observation has no qualified rate window."));
        assert!(!text.contains("Native cleanup record 7"));
        // A title-sized cutoff would hide the mechanism behind the leading native qualification.
        report.entries[0].explanation = format!("{} Recorded pages: 9007199254740993.", "qualification ".repeat(20));
        let detailed = cleanup(&report);
        assert!(detailed.contains("Recorded pages: 9007199254740993."));
        report.entries[0].explanation = "é".repeat(OBSERVATION_DETAIL_CHARS * 2);
        let bounded = cleanup(&report);
        let first = bounded.lines().find(|line| line.starts_with('•')).unwrap();
        assert!(first.ends_with('…'));
        assert!(first.chars().count() <= OBSERVATION_DETAIL_CHARS + 3);
    }

    #[test]
    fn cached_plan_output_bounds_entries_and_keeps_unavailable_plan_and_source_reasons() {
        let report = CachedPlanReport {
            title: "Captured plan evidence".into(),
            summary: "Retained definitions inspected: 8.".into(),
            sources: vec![SourceOutcome {
                source: "native-definitions".into(),
                state: SourceState::Partial,
                reason: Some("Original native visibility was incomplete.".into()),
                reason_message: None,
            }],
            limitations: vec!["Retained evidence has its own lifetime.".into()],
            entries: (0..8)
                .map(|id| CachedPlanEntry {
                    source: "native-definitions".into(),
                    definition_key: format!("definition-{id}"),
                    definition_version: "version".into(),
                    identity: None,
                    plan: None,
                    limitations: vec![culprit_core::CachedPlanLimitation {
                        reason: culprit_core::CachedPlanLimit::UnusablePlan,
                        explanation: "No unambiguous native statement was retained.".into(),
                    }],
                })
                .collect(),
        };
        let text = cached_plans(&report);
        assert_eq!(text.matches('•').count(), super::super::BRIEF_FINDING_LIMIT);
        assert!(text.contains("Original native visibility was incomplete."));
        assert!(text.contains("No unambiguous native statement was retained."));
        assert!(text.contains("Retained evidence has its own lifetime."));
        assert!(!text.contains("definition-7"));
    }

    #[test]
    fn distribution_output_keeps_independent_source_coverage_and_bounds_details() {
        let mut report = RuntimeDistributionReport {
            sources: vec![
                SourceOutcome {
                    source: "native-summary".into(),
                    state: SourceState::Collected,
                    reason: None,
                    reason_message: None,
                },
                SourceOutcome {
                    source: "native-histogram".into(),
                    state: SourceState::Unavailable,
                    reason: Some("Histogram visibility was not established.".into()),
                    reason_message: None,
                },
            ],
            title: "Recorded distributions".into(),
            summary: "The summary and histogram have separate populations.".into(),
            limitations: vec!["These points establish neither a complete history nor a cause.".into()],
            entries: (0..8)
                .map(|index| culprit_core::RuntimeDistributionExplanation {
                    observation_key: format!("private-record-{index}"),
                    identity: culprit_core::telemetry::NativeIdentity::new("private-native-identity"),
                    observed_at: "2026-11-14T00:00:00Z".into(),
                    reset: culprit_core::ResetMarker::Unknown,
                    title: "x".repeat(1000),
                    explanation: "private detailed evidence".into(),
                    metrics: vec![],
                    investigation: "private investigation details".into(),
                    verify: "private verification details".into(),
                })
                .collect(),
        };
        let text = runtime_distributions(&report);
        assert!(text.contains(&culprit_core::i18n::t(
            "evidence.cli.native_source.collected",
            &[("source", "native-summary".into())]
        )));
        assert!(text.contains(&culprit_core::i18n::t(
            "evidence.cli.native_source.unavailable",
            &[("source", "native-histogram".into())]
        )));
        assert!(text.contains(report.sources[1].reason.as_deref().unwrap()));
        assert!(text.contains(&report.limitations[0]));
        assert_eq!(text.matches('•').count(), super::super::BRIEF_FINDING_LIMIT);
        assert!(!text.contains("private-"));
        assert!(!text.contains("private detailed"));

        report.sources.clear();
        assert!(runtime_distributions(&report).contains(&t!("evidence.cli.source_unknown")));
    }

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
    fn task_space_summaries_preserve_source_limits_and_bound_titles_without_native_payload_copies() {
        let report = TaskSpaceReport {
            source_state: Some(SourceState::Partial),
            source_reason: Some("request context was incomplete".into()),
            title: "native task space".into(),
            summary: "allocation counters are not current footprint".into(),
            limitations: vec!["task incarnations and allocating statements remain unresolved".into()],
            entries: (0..8)
                .map(|index| culprit_core::TaskSpaceExplanation {
                    activity: culprit_core::TaskAllocationActivity::Unknown,
                    observation_key: format!("private-proof-{index}"),
                    task: culprit_core::telemetry::TaskSpaceIdentity {
                        session_id: "42".into(),
                        request_id: "0".into(),
                        execution_context_id: index.to_string(),
                    },
                    title: "x".repeat(1000),
                    explanation: "private detailed native context".into(),
                    investigation: "bounded native investigation".into(),
                    verify: "equivalent useful work".into(),
                })
                .collect(),
        };
        let text = task_space(&report);
        assert!(text.contains(&t!("evidence.cli.source_partial")));
        assert!(text.contains(report.source_reason.as_deref().unwrap()));
        assert!(text.contains(&report.limitations[0]));
        assert_eq!(text.matches('•').count(), super::super::BRIEF_FINDING_LIMIT);
        assert_eq!(
            text.lines()
                .filter(|line| line.starts_with('•'))
                .flat_map(str::chars)
                .filter(|character| *character == 'x')
                .count(),
            super::super::BRIEF_FINDING_LIMIT * super::super::BRIEF_TITLE_CHARS
        );
        assert!(!text.contains("private-proof") && !text.contains("private detailed native context"));
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
                task: None,
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
