//! Compare authenticated native compiler counters, independently of runtimes, pools and retained memory peaks.
use super::*;
use culprit_core::telemetry::{CompilationBaseline, NativeReadContext, compilation_namespace};
use culprit_core::{CompilationChange, CompilationComparisonIssue as Issue, CompilationComparisonReport};
use std::collections::BTreeMap;

pub fn compare(
    context: Option<&NativeReadContext>,
    observations: &[&OperationalObservation],
    source: Option<&SourceOutcome>,
    previous: &CompilationBaseline,
) -> Result<CompilationComparisonReport> {
    if !previous.is_valid()
        || context.is_some_and(|context| {
            !context.is_valid()
                || context.target.database().is_none_or(|database| database.engine != culprit_core::Engine::Mssql)
        })
    {
        return Err(invalid());
    }
    let cases = cases(observations, source)?;
    let mut report = empty_report(previous);
    let Some(source) = source else {
        return Ok(unavailable(report, Issue::SourceUnavailable));
    };
    if source.source != previous.source {
        return Ok(unavailable(report, Issue::DifferentSource));
    }
    let (Some(before), Some(after)) = (&previous.context, context) else {
        return Ok(unavailable(report, Issue::NativeContextUnavailable));
    };
    let (Some(before_namespace), Some(after_namespace)) = (compilation_namespace(before), compilation_namespace(after))
    else {
        return Ok(unavailable(report, Issue::NativeNamespaceUnavailable));
    };
    if before_namespace != after_namespace {
        return Ok(unavailable(report, Issue::DifferentSource));
    }
    if instant(&before.observed_until)? > instant(&after.observed_from)?
        || instant(&before.observed_until)? >= instant(&after.observed_until)?
    {
        return Ok(unavailable(report, Issue::InvalidObservationOrder));
    }
    if !readable(previous.source_state) || !readable(Some(source.state)) {
        return Ok(unavailable(report, Issue::SourceUnavailable));
    }
    report.observed_from = Some(before.observed_from.clone());
    report.observed_until = Some(after.observed_until.clone());
    if previous.source_state == Some(SourceState::Partial) || source.state == SourceState::Partial {
        add_issue(&mut report, Issue::PartialRead);
    }
    let previous: BTreeMap<_, _> =
        previous.points.iter().map(|point| (point.observation_key.as_str(), point)).collect();
    let current_keys: BTreeSet<_> = cases.iter().map(|case| case.observation.key.as_str()).collect();
    report.missing_after = previous.keys().filter(|key| !current_keys.contains(**key)).count() as u32;
    for case in cases {
        let Some(before_point) = previous.get(case.observation.key.as_str()) else {
            report.missing_before += 1;
            continue;
        };
        let before_count = before_point.compilations.parse::<u64>().map_err(|_| invalid())?;
        let Some(delta) = case.count.checked_sub(before_count).filter(|_| before_point.reset == case.native.reset)
        else {
            add_issue(&mut report, Issue::CounterChanged);
            continue;
        };
        let before_compile = instant(&before_point.last_compile_at)?;
        let last_compile = instant(&case.native.last_compile_at)?;
        if before_compile > instant(&before.observed_until)?
            || before_compile < case.first_compile_at
            || last_compile < before_compile
            || last_compile > instant(&after.observed_until)?
            || delta == 0 && last_compile != before_compile
            || delta > 0 && last_compile < instant(&before.observed_from)?
        {
            add_issue(&mut report, Issue::NativeClockConflict);
            continue;
        }
        report.entries.push(render_change(case, before_point, delta));
    }
    if report.missing_before > 0 || report.missing_after > 0 {
        add_issue(&mut report, Issue::UnmatchedEntries);
    }
    report.entries.sort_by(|left, right| left.observation_key.cmp(&right.observation_key));
    report.summary = t!(
        "view.compilation.comparison.summary",
        n = report.entries.len(),
        count = format::digits(report.entries.len().to_string())
    );
    Ok(report)
}

fn readable(state: Option<SourceState>) -> bool {
    matches!(state, Some(SourceState::Collected | SourceState::Empty | SourceState::Partial))
}

fn instant(value: &str) -> Result<String> {
    // The shared canonical form is UTC with nine fractional digits; comparisons preserve native nanoseconds.
    culprit_core::telemetry::canonical_time(value).map_err(|_| invalid())
}

fn empty_report(previous: &CompilationBaseline) -> CompilationComparisonReport {
    CompilationComparisonReport {
        baseline: previous.reference.clone(),
        observed_from: None,
        observed_until: None,
        missing_before: 0,
        missing_after: 0,
        issues: vec![],
        entries: vec![],
        title: t!("view.compilation.comparison.title"),
        summary: t!("view.compilation.comparison.unavailable"),
        limitations: vec![
            t!("view.compilation.comparison.limit.interval"),
            t!("view.compilation.comparison.limit.cost"),
        ],
    }
}

fn unavailable(mut report: CompilationComparisonReport, issue: Issue) -> CompilationComparisonReport {
    add_issue(&mut report, issue);
    report
}

fn add_issue(report: &mut CompilationComparisonReport, issue: Issue) {
    if report.issues.contains(&issue) {
        return;
    }
    let text = match issue {
        Issue::NativeContextUnavailable => t!("view.compilation.comparison.issue.context"),
        Issue::NativeNamespaceUnavailable => t!("view.compilation.comparison.issue.namespace"),
        Issue::DifferentSource => t!("view.compilation.comparison.issue.source"),
        Issue::InvalidObservationOrder => t!("view.compilation.comparison.issue.order"),
        Issue::SourceUnavailable => t!("view.compilation.comparison.issue.unavailable"),
        Issue::PartialRead => t!("view.compilation.comparison.issue.partial"),
        Issue::UnmatchedEntries => t!("view.compilation.comparison.issue.entries"),
        Issue::CounterChanged => t!("view.compilation.comparison.issue.counter"),
        Issue::NativeClockConflict => t!("view.compilation.comparison.issue.clock"),
    };
    report.issues.push(issue);
    report.limitations.push(text);
}

fn render_change(
    case: QueryCompilation<'_>,
    previous: &culprit_core::telemetry::CompilationCounterPoint,
    delta: u64,
) -> CompilationChange {
    let count = format::digits(delta.to_string());
    let mut explanation = if delta == 0 {
        t!("view.compilation.comparison.entry.unchanged")
    } else {
        t!("view.compilation.comparison.entry.changed", count = count)
    };
    let policy = delta > 0 && super::policy::recompile_statement(case.native).is_some();
    if policy {
        explanation.push(' ');
        explanation.push_str(&t!("view.compilation.comparison.entry.policy"));
    }
    CompilationChange {
        observation_key: case.observation.key.clone(),
        baseline_observation_key: previous.observation_key.clone(),
        identity: case.native.identity.clone(),
        compilations: delta.to_string(),
        last_compile_at: case.native.last_compile_at.clone(),
        explanation,
        investigation: if policy {
            t!("check.planning_time.fix.recompile_hint")
        } else {
            t!("view.compilation.comparison.investigate")
        },
        verify: t!("view.compilation.comparison.verify"),
    }
}

#[cfg(test)]
mod tests;
