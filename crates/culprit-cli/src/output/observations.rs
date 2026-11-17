use culprit_core::{
    CachedPlanEntry, CachedPlanReport, CompilationReport, MemoryParticipantsReport, PageWaitReport,
    RetainedFailureReport, RuntimeDistributionReport, SourceOutcome, SourceState, StatementWorkReport, TaskSpaceReport,
    collected_analysis::{CollectedQueryAnalysis, CollectedQueryAnalysisResult, CollectedQueryPopulation},
    format, t,
};

const OBSERVATION_DETAIL_CHARS: usize = 1024;
mod blocking;
pub(crate) use blocking::blocking;
mod connection_outcomes;
pub(crate) use connection_outcomes::connection_outcomes;
mod maintenance_blocking;
pub(crate) use maintenance_blocking::maintenance_blocking;
mod page_task_holders;
pub(crate) use page_task_holders::page_task_holders;

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

pub(crate) fn replication(report: &culprit_core::ReplicationReport) -> String {
    let (from, until) = report.context.bounds();
    render_summary(
        Summary {
            title: &report.title,
            summary: &report.summary,
            source: SourceQualification::Multiple(&report.sources),
            limitations: &report.limitations,
        },
        |text| {
            text.push('\n');
            text.push_str(&t!("evidence.cli.replication_bounds", from = from, until = until));
            text.push('\n');
            text.push_str(&t!(
                "evidence.cli.replication_counts",
                native = format::digits(report.native_rows.to_string()),
                observed = format::digits(report.observed_rows.to_string()),
                states = format::digits(report.state_rows.to_string()),
                presented = format::digits(report.presented_rows.to_string()),
                printed = format::digits(report.entries.len().min(super::BRIEF_FINDING_LIMIT).to_string())
            ));
            for entry in report.entries.iter().take(super::BRIEF_FINDING_LIMIT) {
                append_replication_entry(text, entry);
            }
            for limitation in report.limitations.iter().skip(1) {
                text.push('\n');
                text.push_str(limitation);
            }
        },
    )
}

fn append_replication_entry(text: &mut String, entry: &culprit_core::ReplicationEntry) {
    text.push_str("\n• ");
    text.push_str(&t!("evidence.cli.observation_entry", title = &entry.title, explanation = &entry.explanation));
    text.push('\n');
    text.push_str(&t!(
        "evidence.cli.replication_reference",
        source = &entry.reference.source,
        scope = culprit_core::wire_name(&entry.scope),
        key = &entry.reference.observation_key
    ));
    if !entry.conditions.is_empty() {
        text.push('\n');
        text.push_str(&format::list(&entry.conditions.iter().map(|condition| condition.label()).collect::<Vec<_>>()));
    }
    append_replication_fields(text, &entry.fields);
    for group in &entry.groups {
        text.push('\n');
        text.push_str(&group.title);
        append_replication_fields(text, &group.fields);
    }
}

fn append_replication_fields(text: &mut String, fields: &[culprit_core::ReplicationField]) {
    for field in fields {
        let value = match field.value.as_deref() {
            None => t!("evidence.cli.replication_unknown"),
            Some("") => t!("evidence.cli.replication_empty"),
            Some(value) => value.into(),
        };
        text.push('\n');
        text.push_str(&t!("evidence.cli.replication_field", label = &field.label, value = value));
    }
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
    let mut text = render_explanations(
        Summary {
            title: &report.title,
            summary: &report.summary,
            source: SourceQualification::Single { state: report.source_state, reason: report.source_reason.as_deref() },
            limitations: &report.limitations,
        },
        report.entries.iter().map(|entry| (entry.title.as_str(), entry.explanation.as_str())),
    );
    if let Some(comparison) = &report.comparison {
        text.push('\n');
        let summary = match (&comparison.observed_from, &comparison.observed_until) {
            (Some(from), Some(until)) => format!(
                "{}\n{}",
                comparison.summary,
                t!("view.compilation.comparison.bounds", from = from, until = until)
            ),
            _ => comparison.summary.clone(),
        };
        let entries =
            comparison.entries.iter().map(|entry| (entry.identity.query_id.as_str(), entry.explanation.as_str()));
        text.push_str(&render_explanations(
            Summary {
                title: &comparison.title,
                summary: &summary,
                source: SourceQualification::Single { state: report.source_state, reason: None },
                limitations: &comparison.limitations,
            },
            entries,
        ));
        for limitation in comparison.limitations.iter().skip(1) {
            text.push('\n');
            text.push_str(limitation);
        }
    }
    text
}

pub(crate) fn compiler_admission(report: &culprit_core::CompilerAdmissionReport) -> String {
    render_explanations(
        Summary {
            title: &report.title,
            summary: &report.summary,
            source: SourceQualification::Single { state: report.source_state, reason: report.source_reason.as_deref() },
            limitations: &report.limitations,
        },
        report
            .waiters
            .iter()
            .map(|entry| (entry.title.as_str(), entry.explanation.as_str()))
            .chain(report.gateways.iter().map(|entry| (entry.title.as_str(), entry.explanation.as_str()))),
    )
}

pub(crate) fn page_wait_writers(report: &culprit_core::PageWaitWriterReport) -> String {
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

pub(crate) fn maintenance(report: &culprit_core::MaintenanceReport) -> String {
    render_explanations(
        Summary {
            title: &report.title,
            summary: &report.summary,
            source: SourceQualification::Single {
                state: Some(report.source_state),
                reason: report.source_reason.as_deref(),
            },
            limitations: &report.limitations,
        },
        report.entries.iter().map(|entry| (entry.title.as_str(), entry.explanation.as_str())),
    )
}

pub(crate) fn jobs(report: &culprit_core::JobsReport) -> String {
    native_work_summary(
        Summary {
            title: &report.title,
            summary: &report.summary,
            source: SourceQualification::Multiple(&report.sources),
            limitations: &report.limitations,
        },
        report.entries.iter().map(|entry| {
            let mut details = vec![entry.explanation.clone()];
            details.extend(native_metrics(&entry.metrics));
            (entry.title.as_str(), details)
        }),
    )
}

pub(crate) fn maintenance_progress(report: &culprit_core::MaintenanceProgressReport) -> String {
    native_work_summary(
        Summary {
            title: &report.title,
            summary: &report.summary,
            source: SourceQualification::Single {
                state: Some(report.source_state),
                reason: report.source_reason.as_deref(),
            },
            limitations: &report.limitations,
        },
        report.entries.iter().map(|entry| {
            let mut details = vec![entry.phase.clone()];
            details.extend(native_metrics(&entry.metrics));
            (entry.title.as_str(), details)
        }),
    )
}

pub(crate) fn current_request_work(report: &culprit_core::CurrentRequestWorkReport) -> String {
    native_work_summary(
        Summary {
            title: &report.title,
            summary: &report.summary,
            source: SourceQualification::Single {
                state: Some(report.source_state),
                reason: report.source_reason.as_deref(),
            },
            limitations: &report.limitations,
        },
        report.entries.iter().map(|entry| {
            let mut details = vec![entry.explanation.clone()];
            details.extend(native_metrics(&entry.metrics));
            (entry.title.as_str(), details)
        }),
    )
}

fn native_metrics(metrics: &[culprit_core::Metric]) -> impl Iterator<Item = String> + '_ {
    metrics.iter().filter_map(|metric| metric.text.as_ref().map(|value| format!("{}: {value}", metric.label)))
}

fn native_work_summary<'a>(summary: Summary<'a>, entries: impl Iterator<Item = (&'a str, Vec<String>)>) -> String {
    let details = entries
        .take(super::BRIEF_FINDING_LIMIT)
        .map(|(title, details)| t!("evidence.cli.observation_entry", title = title, explanation = details.join("\n")))
        .collect::<Vec<_>>();
    render_summary(summary, |text| {
        super::append_brief_details(text, details.iter().map(String::as_str), OBSERVATION_DETAIL_CHARS)
    })
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
            let mut details = if entry.work.is_empty() {
                t!("evidence.cli.statement_work_unmeasured")
            } else {
                format::list(&entry.work.iter().map(|work| work.title.clone()).collect::<Vec<_>>())
            };
            for metric in &entry.outcome_metrics {
                if let Some(value) = &metric.text {
                    details.push_str(&format!("\n{}: {value}", metric.label));
                }
            }
            details.push('\n');
            details.push_str(&t!(
                "evidence.cli.statement_work_context",
                source = &entry.source,
                scope = culprit_core::wire_name(&entry.scope),
                reset = serde_json::to_string(&entry.reset).expect("native reset markers serialize")
            ));
            if !entry.identity.qualifiers.is_empty() {
                let qualifiers = entry
                    .identity
                    .qualifiers
                    .iter()
                    .map(|(name, value)| format!("{name}={value:?}"))
                    .collect::<Vec<_>>();
                details.push('\n');
                details
                    .push_str(&t!("evidence.cli.query_population_qualifiers", qualifiers = format::list(&qualifiers)));
            }
            t!(
                "evidence.cli.statement_work_entry",
                query = entry.identity.query_id.as_str(),
                observed = entry.observed_at.as_str(),
                work = details
            )
        })
        .collect::<Vec<_>>();
    let mut text = render_summary(
        Summary {
            title: &report.title,
            summary: &report.summary,
            source: SourceQualification::Single { state: report.source_state, reason: report.source_reason.as_deref() },
            limitations: &report.limitations,
        },
        |text| super::append_brief_details(text, titles.iter().map(String::as_str), OBSERVATION_DETAIL_CHARS),
    );
    if let Some(context) = &report.context {
        text.push('\n');
        text.push_str(&t!(
            "evidence.cli.statement_work_bounds",
            from = &context.observed_from,
            until = &context.observed_until
        ));
    }
    text
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
    fn compilation_changes_show_exact_native_bounds_and_all_coverage_qualifications_without_implied_cost() {
        use culprit_core::{
            CompilationChange, CompilationComparisonIssue, CompilationComparisonReport, telemetry::NativeQueryIdentity,
        };
        let identity = NativeQueryIdentity {
            query_id: "42".into(),
            qualifiers: [
                ("context_settings_id".into(), "5".into()),
                ("object_id".into(), "1999".into()),
                ("query_hash".into(), "0123456789ABCDEF".into()),
                ("initial_compile_at".into(), "2026-11-13T06:00:00Z".into()),
            ]
            .into(),
        };
        let key = culprit_core::telemetry::MssqlQueryCompilation::identity_key(&identity).unwrap();
        let report = CompilationReport {
            source_state: Some(SourceState::Collected),
            source_reason: None,
            title: "Retained compilations".into(),
            summary: "Native lifetime entries.".into(),
            limitations: vec![],
            entries: vec![],
            comparison: Some(CompilationComparisonReport {
                baseline: None,
                observed_from: Some("2026-11-13T09:59:00.000000001Z".into()),
                observed_until: Some("2026-11-13T10:00:01.000000002Z".into()),
                missing_before: 0,
                missing_after: 1,
                issues: vec![CompilationComparisonIssue::PartialRead, CompilationComparisonIssue::UnmatchedEntries],
                title: t!("view.compilation.comparison.title"),
                summary: t!("view.compilation.comparison.summary", n = 1, count = "1"),
                limitations: vec![
                    t!("view.compilation.comparison.limit.interval"),
                    t!("view.compilation.comparison.limit.cost"),
                    t!("view.compilation.comparison.issue.partial"),
                    t!("view.compilation.comparison.issue.entries"),
                ],
                entries: vec![CompilationChange {
                    observation_key: key.clone(),
                    baseline_observation_key: key,
                    identity,
                    compilations: "18446744073709551615".into(),
                    last_compile_at: "2026-11-13T10:00:00Z".into(),
                    explanation: t!("view.compilation.comparison.entry.changed", count = "18446744073709551615"),
                    investigation: String::new(),
                    verify: String::new(),
                }],
            }),
        };
        let text = compilation(&report);
        assert!(text.contains("18446744073709551615"));
        assert!(text.contains("2026-11-13T09:59:00.000000001Z"));
        assert!(text.contains("2026-11-13T10:00:01.000000002Z"));
        for limitation in &report.comparison.as_ref().unwrap().limitations {
            assert!(text.contains(limitation), "{limitation}");
        }
        assert!(!text.contains("duration_delta") && !text.contains("memory_total"));
    }

    fn replication_report() -> culprit_core::ReplicationReport {
        use culprit_core::{
            ReplicationEntry, ReplicationField, ReplicationFieldGroup, ReplicationReference, telemetry::*,
        };
        culprit_core::ReplicationReport {
            context: ObservationReadContext::Native {
                context: Box::new(NativeReadContext {
                    target: culprit_core::DatabaseIdentity::server(
                        culprit_core::Engine::Mysql,
                        "app",
                        "private-cli-replication-member.invalid",
                        3306,
                        None,
                    )
                    .into(),
                    partition: SourcePartition::Target,
                    observed_from: "2026-11-16T10:00:00Z".into(),
                    observed_until: "2026-11-16T10:00:01Z".into(),
                    database_writable: None,
                }),
            },
            sources: vec![SourceOutcome {
                source: "replication-channels".into(),
                state: SourceState::Partial,
                reason: Some("Native visibility is incomplete.".into()),
                reason_message: None,
            }],
            read_complete: false,
            native_rows: 7,
            observed_rows: 5,
            state_rows: 1,
            presented_rows: 5,
            entries_limited: false,
            title: "Native replication".into(),
            summary: "Five original points.".into(),
            limitations: vec![
                "A recorded error is not an application owner.".into(),
                "The archive read is incomplete.".into(),
            ],
            entries: (0..5)
                .map(|i| ReplicationEntry {
                    reference: ReplicationReference {
                        source: "replication-channels".into(),
                        observation_key: format!("point-{i}"),
                    },
                    scope: culprit_core::MeasurementScope::Server,
                    conditions: vec![culprit_core::ReplicationCondition::ReceiverStopped],
                    title: format!("Channel-{i}"),
                    explanation: "Original native state.".into(),
                    fields: vec![ReplicationField {
                        key: "received-source-log-position".into(),
                        label: "Received position".into(),
                        value: Some(format::digits("9007199254740993".into())),
                    }],
                    groups: vec![ReplicationFieldGroup {
                        title: "Recorded coordinates".into(),
                        fields: vec![
                            ReplicationField {
                                key: "source_log_file".into(),
                                label: "Source file".into(),
                                value: Some("bin.000007".into()),
                            },
                            ReplicationField {
                                key: "io_error_at_without_offset".into(),
                                label: "Native error time".into(),
                                value: Some("261115 01:02:03".into()),
                            },
                            ReplicationField {
                                key: "configured-delay".into(),
                                label: "Configured delay".into(),
                                value: Some("0".into()),
                            },
                            ReplicationField {
                                key: "source_uuid".into(),
                                label: "Source identifier".into(),
                                value: None,
                            },
                            ReplicationField {
                                key: "channel".into(),
                                label: "Native channel name".into(),
                                value: Some("".into()),
                            },
                        ],
                    }],
                    session: None,
                    unresolved_session_id: None,
                    investigation: String::new(),
                    verify: String::new(),
                })
                .collect(),
        }
    }

    #[test]
    fn replication_text_keeps_exact_fields_refs_read_gaps_and_an_explicit_printed_entry_bound() {
        culprit_core::i18n::scope("en", || {
            let report = replication_report();
            let text = replication(&report);
            assert!(text.contains(&format::digits("9007199254740993".into())));
            assert!(text.contains("bin.000007") && text.contains("261115 01:02:03"));
            assert!(text.contains("Recorded coordinates") && text.contains("2026-11-16T10:00:00Z"));
            assert!(text.contains(&t!("evidence.cli.replication_field", label = "Configured delay", value = "0")));
            assert!(
                text.contains(&t!("evidence.cli.replication_unknown"))
                    && text.contains(&t!("evidence.cli.replication_empty"))
            );
            assert!(text.contains(&t!(
                "evidence.cli.replication_counts",
                native = "7",
                observed = "5",
                states = "1",
                presented = "5",
                printed = "3"
            )));
            assert!(text.contains("point-0") && text.contains("point-2"));
            assert!(!text.contains("point-3") && !text.contains("Channel-3"));
            assert!(
                text.contains("Native visibility is incomplete.") && text.contains("The archive read is incomplete.")
            );
            assert!(!text.contains("private-cli-replication-member") && !text.contains("9007199254740992"));
        });
    }

    #[test]
    fn replication_empty_partial_and_unavailable_text_keep_independent_source_qualification() {
        for (state, key) in [
            (SourceState::Empty, "evidence.cli.native_source.empty"),
            (SourceState::Partial, "evidence.cli.native_source.partial"),
            (SourceState::Unavailable, "evidence.cli.native_source.unavailable"),
        ] {
            let mut report = replication_report();
            report.sources[0].state = state;
            report.entries.clear();
            report.presented_rows = 0;
            let text = replication(&report);
            assert!(text.contains(&culprit_core::i18n::t(key, &[("source", "replication-channels".into())])));
            assert!(text.contains("Native visibility is incomplete."));
            assert!(!text.contains("point-0"));
        }
    }

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
            admission_resources: vec![],
            entries: (0..8)
                .map(|index| culprit_core::MemoryParticipantExplanation {
                    observation_key: format!("native-{index}"),
                    observation_identity: culprit_core::telemetry::NativeIdentity::new(format!("native-{index}")),
                    title: "x".repeat(1000),
                    explanation: "not included in concise output".into(),
                    investigation: "resource-first investigation".into(),
                    verify: "equivalent work".into(),
                    grant_admission: None,
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
                blocking: culprit_core::PageWaitBlockingParticipation::unavailable(),
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
        let mut report = StatementWorkReport {
            source: "native-counters".into(),
            context: None,
            source_state: Some(SourceState::Partial),
            source_reason: Some("native source is incomplete".into()),
            title: "native statement work".into(),
            summary: "counter points, not a measured interval".into(),
            limitations: vec!["native reset remains unknown".into()],
            entries: (0..8)
                .map(|index| culprit_core::StatementWorkEntry {
                    source: "native-counters".into(),
                    outcomes: Default::default(),
                    outcome_metrics: vec![],
                    observation_key: "proof-reference".into(),
                    identity: culprit_core::telemetry::NativeQueryIdentity {
                        query_id: format!("query-{index}"),
                        qualifiers: Default::default(),
                    },
                    scope: culprit_core::MeasurementScope::Database,
                    observed_at: "2020-01-01T12:00:00.123456789Z".into(),
                    reset: culprit_core::ResetMarker::Unknown,
                    work: vec![],
                })
                .collect(),
        };
        report.context = Some(Box::new(culprit_core::telemetry::NativeReadContext {
            target: culprit_core::DatabaseIdentity::server(
                culprit_core::Engine::Mysql,
                "app",
                "fixture.invalid",
                3306,
                None,
            )
            .into(),
            partition: culprit_core::telemetry::SourcePartition::Target,
            observed_from: "2020-01-01T12:00:00Z".into(),
            observed_until: "2020-01-01T12:00:01Z".into(),
            database_writable: None,
        }));
        report.entries[0].identity.qualifiers.insert("schema".into(), "app".into());
        report.entries[1].identity.query_id = report.entries[0].identity.query_id.clone();
        report.entries[1].identity.qualifiers.insert("schema".into(), "another_schema".into());
        report.entries[0].outcomes = culprit_core::StatementOutcomeCounts {
            errors: Some("9007199254740993".into()),
            warnings: Some("0".into()),
        };
        report.entries[0].outcome_metrics = vec![
            culprit_core::Metric::said_text("view.statement_work.outcome.errors", "9007199254740993"),
            culprit_core::Metric::said_text("view.statement_work.outcome.warnings", "0"),
        ];
        let text = statement_work(&report);
        assert!(text.contains("query-0") && text.contains("2020-01-01T12:00:00.123456789Z"));
        assert!(text.contains(&t!("evidence.cli.statement_work_unmeasured")));
        assert!(text.contains(&t!("evidence.cli.source_partial")));
        assert!(text.contains("native reset remains unknown"));
        assert!(text.contains("native-counters") && text.contains("another_schema"));
        assert!(text.contains("9007199254740993") && !text.contains("9,007,199,254,740,993"));
        assert!(text.contains(&format!("{}: 0", t!("view.statement_work.outcome.warnings"))));
        assert!(text.contains("2020-01-01T12:00:00Z") && text.contains("2020-01-01T12:00:01Z"));
        assert_eq!(text.matches('•').count(), super::super::BRIEF_FINDING_LIMIT);
        assert!(!text.contains("proof-reference") && !text.contains("query-7"));
        let long_context = "x".repeat(OBSERVATION_DETAIL_CHARS * 2);
        report.entries[0].identity.qualifiers.insert("long-native-context".into(), long_context.clone());
        let bounded = statement_work(&report);
        assert!(bounded.contains('…') && !bounded.contains(&long_context));
        assert!(bounded.contains("9007199254740993"), "exact outcomes precede bounded context details");
    }
}
