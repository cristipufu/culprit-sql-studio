//! Observed wait targets explain affected requests, never transient latch holders or an exclusive cause.
use culprit_core::{
    Error, PageWaitActivityEvidence, PageWaitBlockingIssue, PageWaitBlockingParticipation, PageWaitExplanation,
    PageWaitReport, Result, SourceOutcome, SourceState, WaitResourceKind, format, key,
    page_wait_blocking_participation, t,
    telemetry::{
        MAX_PAGE_WAIT_OBSERVATIONS, MssqlIndexCounter, PageIndexOperationRelation, PageIndexOperations,
        PageWaitObservation, PageWaitRecheck, PageWaitRelation, PageWaitRequestState, PageWaitResolution,
        PageWaitTargetKind,
    },
};
use std::collections::BTreeSet;
mod writers;
pub use writers::explain_writers;

/// Native scans and authenticated readback call the same pure explanation over their original bounded rows.
/// Source availability is independent from the ability to retrieve those rows, including an empty population.
pub fn explain(
    observations: &[&PageWaitObservation],
    source: Option<&SourceOutcome>,
    activity: Option<PageWaitActivityEvidence<'_>>,
) -> Result<PageWaitReport> {
    if let Some(activity) = activity {
        activity.validate()?;
    }
    let cases = validate(observations, source)?;
    let entries = cases.into_iter().map(|case| render(case, activity)).collect::<Result<Vec<_>>>()?;
    Ok(PageWaitReport {
        source_state: source.map(|source| source.state),
        source_reason: source.and_then(|source| source.reason.clone()),
        title: t!("view.page_waits.title"),
        summary: if observations.is_empty() {
            t!("view.page_waits.summary.empty")
        } else {
            t!(
                "view.page_waits.summary.observed",
                n = observations.len(),
                count = format::count(observations.len() as f64)
            )
        },
        limitations: limitations(source, observations.iter().any(|row| row.index_operations.is_some())),
        entries,
    })
}

struct ObservedPage<'a> {
    observation: &'a PageWaitObservation,
    key: String,
    wait: WaitKind,
    investigation: Investigation,
    index: Option<IndexPoint<'a>>,
}
#[derive(Clone, Copy)]
enum WaitKind {
    Memory,
    Io,
}
#[derive(Clone, Copy)]
enum Investigation {
    Unresolved,
    Storage,
    MemoryTarget(PageWaitTargetKind),
}
impl Investigation {
    fn for_observation(observation: &PageWaitObservation, wait: WaitKind) -> Self {
        let Some(target) = &observation.target else {
            return Self::Unresolved;
        };
        if matches!(wait, WaitKind::Io) { Self::Storage } else { Self::MemoryTarget(target.kind) }
    }
}
enum IndexPoint<'a> {
    Measured { waits: &'a str, milliseconds: &'a str, relation: PageIndexOperationRelation },
    Unknown { relation: PageIndexOperationRelation },
    Missing,
    Denied,
    Unavailable,
}

fn validate<'a>(
    observations: &[&'a PageWaitObservation],
    source: Option<&SourceOutcome>,
) -> Result<Vec<ObservedPage<'a>>> {
    let expected_source =
        source.map(|source| source.source.as_str()).or_else(|| observations.first().map(|row| row.source.as_str()));
    let source_conflict = source.is_some_and(|source| {
        if observations.is_empty() {
            source.state == SourceState::Collected
        } else {
            matches!(source.state, SourceState::Empty | SourceState::Unavailable | SourceState::NotApplicable)
        }
    });
    if observations.len() > MAX_PAGE_WAIT_OBSERVATIONS || source_conflict {
        return Err(invalid());
    }
    let mut keys = BTreeSet::new();
    observations
        .iter()
        .map(|observation| {
            if !observation.is_valid() || expected_source != Some(observation.source.as_str()) {
                return Err(invalid());
            }
            let key = observation.key()?;
            if !keys.insert(key.clone()) {
                return Err(invalid());
            }
            let wait = match observation.observed_wait().resource.kind {
                WaitResourceKind::MemoryPage => WaitKind::Memory,
                WaitResourceKind::PageIo => WaitKind::Io,
                _ => return Err(invalid()),
            };
            let index = observation.index_operations.as_ref().map(|point| index_point(point, wait));
            let investigation = Investigation::for_observation(observation, wait);
            Ok(ObservedPage { observation, key, wait, investigation, index })
        })
        .collect()
}

fn limitations(source: Option<&SourceOutcome>, has_index_points: bool) -> Vec<String> {
    let mut limits = vec![
        t!("view.page_waits.limit.ownership"),
        t!("view.page_waits.limit.bounds"),
        t!("view.page_waits.limit.clock"),
    ];
    if has_index_points {
        limits.push(t!("view.page_waits.limit.index_counters"));
    }
    match source.map(|source| source.state) {
        None => limits.push(t!("view.page_waits.limit.source_unknown")),
        Some(SourceState::Partial) => limits.push(t!("view.page_waits.limit.source_partial")),
        Some(SourceState::Unavailable | SourceState::NotApplicable) => {
            limits.push(t!("view.page_waits.limit.source_unavailable"))
        }
        Some(SourceState::Collected | SourceState::Empty) => {}
    }
    limits
}

fn render(case: ObservedPage<'_>, activity: Option<PageWaitActivityEvidence<'_>>) -> Result<PageWaitExplanation> {
    let observation = case.observation;
    let (title, base) = match (case.wait, observation.task_wait.is_some()) {
        (WaitKind::Memory, false) => (t!("view.page_waits.memory.title"), t!("view.page_waits.memory.explanation")),
        (WaitKind::Memory, true) => (t!("view.page_waits.memory.title"), t!("view.page_waits.task.memory")),
        (WaitKind::Io, false) => (t!("view.page_waits.io.title"), t!("view.page_waits.io.explanation")),
        (WaitKind::Io, true) => (t!("view.page_waits.io.title"), t!("view.page_waits.task.io")),
    };
    let mut details = vec![base];
    if let Some(task) = &observation.task_wait {
        details
            .push(t!("view.page_waits.task.context", context = format::digits(task.execution_context_id.to_string())));
    }
    if let Some(target) = &observation.target {
        details.push(culprit_core::i18n::t(kind_message(target.kind), &[]));
        if let Some(object) = &target.object {
            details.push(if let Some(index) = &object.index_name {
                t!(
                    "view.page_waits.target.index",
                    schema = object.schema.as_str(),
                    object = object.name.as_str(),
                    index = index.as_str()
                )
            } else {
                t!("view.page_waits.target.object", schema = object.schema.as_str(), object = object.name.as_str())
            });
        }
    } else {
        details.push(culprit_core::i18n::t(unresolved_message(observation.resolution), &[]));
    }
    details.push(culprit_core::i18n::t(
        recheck_message(observation.after_check.as_ref().or(observation.before_check.as_ref())),
        &[],
    ));
    if let Some(point) = &case.index {
        render_index_point(&mut details, point, case.wait);
    }
    let blocking = match activity {
        Some(activity) => {
            page_wait_blocking_participation(activity.source, observation, activity.original, activity.frame)?
        }
        None => PageWaitBlockingParticipation::unavailable(),
    };
    render_blocking(&blocking, &mut details);
    let investigation = culprit_core::i18n::t(investigation_message(case.investigation), &[]);
    Ok(PageWaitExplanation {
        observation_key: case.key,
        request_key: observation.request.key.clone(),
        task: observation.task_wait.as_ref().map(Into::into),
        resource: observation.resource.clone(),
        blocking,
        title,
        explanation: details.join(" "),
        investigation,
        verify: match case.investigation {
            Investigation::MemoryTarget(
                PageWaitTargetKind::Pfs | PageWaitTargetKind::Gam | PageWaitTargetKind::Sgam | PageWaitTargetKind::Iam,
            ) => t!("view.page_waits.verify.allocation"),
            _ => t!("view.page_waits.verify"),
        },
    })
}

fn render_blocking(blocking: &PageWaitBlockingParticipation, details: &mut Vec<String>) {
    if !blocking.contexts.is_empty() {
        details.push(t!(
            "view.page_waits.blocking.observed",
            n = blocking.contexts.len(),
            count = format::digits(blocking.contexts.len().to_string()),
            at = blocking.basis.as_ref().map(|basis| basis.observed_at.as_str()).unwrap_or_default()
        ));
        details.push(t!("view.page_waits.blocking.historical"));
    }
    for issue in &blocking.issues {
        let message = match issue {
            PageWaitBlockingIssue::FrameUnavailable => key!("view.page_waits.blocking.frame_unavailable"),
            PageWaitBlockingIssue::NativeScopeUnqualified => key!("view.page_waits.blocking.scope_unqualified"),
            PageWaitBlockingIssue::ParallelTaskUnresolved => key!("view.page_waits.blocking.task_unresolved"),
            PageWaitBlockingIssue::WaitOwnerUnavailable => key!("view.page_waits.blocking.owner_unavailable"),
            PageWaitBlockingIssue::OwnerGenerationUnavailable => {
                key!("view.page_waits.blocking.generation_unavailable")
            }
            PageWaitBlockingIssue::OwnerContextUnavailable => key!("view.page_waits.blocking.context_unavailable"),
            PageWaitBlockingIssue::SelfReference => key!("view.page_waits.blocking.self_reference"),
            PageWaitBlockingIssue::MultipleOwnerContexts => key!("view.page_waits.blocking.multiple_requests"),
            PageWaitBlockingIssue::ContextsLimited => key!("view.page_waits.blocking.contexts_limited"),
            PageWaitBlockingIssue::ReadIncomplete => key!("view.page_waits.blocking.read_incomplete"),
            PageWaitBlockingIssue::NativePopulationPartial => key!("view.page_waits.blocking.population_partial"),
        };
        details.push(culprit_core::i18n::t(message, &[]));
    }
}

fn index_point(point: &PageIndexOperations, wait: WaitKind) -> IndexPoint<'_> {
    match point {
        PageIndexOperations::Observed { counters, relation, .. } => {
            let (count, milliseconds) = match wait {
                WaitKind::Memory => (MssqlIndexCounter::PageLatchWaits, MssqlIndexCounter::PageLatchWaitMs),
                WaitKind::Io => (MssqlIndexCounter::PageIoLatchWaits, MssqlIndexCounter::PageIoLatchWaitMs),
            };
            match (counters.counter(count), counters.counter(milliseconds)) {
                (Some(waits), Some(milliseconds)) => IndexPoint::Measured { waits, milliseconds, relation: *relation },
                _ => IndexPoint::Unknown { relation: *relation },
            }
        }
        PageIndexOperations::Missing => IndexPoint::Missing,
        PageIndexOperations::PermissionDenied => IndexPoint::Denied,
        PageIndexOperations::Unavailable => IndexPoint::Unavailable,
    }
}

fn render_index_point(details: &mut Vec<String>, point: &IndexPoint<'_>, wait: WaitKind) {
    let relation = match point {
        IndexPoint::Measured { waits, milliseconds, relation } => {
            let message = match wait {
                WaitKind::Memory => key!("view.page_waits.index.memory"),
                WaitKind::Io => key!("view.page_waits.index.io"),
            };
            details.push(culprit_core::i18n::t(
                message,
                &[
                    ("waits", format::digits((*waits).to_owned()).into()),
                    ("milliseconds", format::digits((*milliseconds).to_owned()).into()),
                ],
            ));
            Some(*relation)
        }
        IndexPoint::Unknown { relation } => {
            details.push(match wait {
                WaitKind::Memory => t!("view.page_waits.index.memory_unknown"),
                WaitKind::Io => t!("view.page_waits.index.io_unknown"),
            });
            Some(*relation)
        }
        IndexPoint::Missing => {
            details.push(t!("view.page_waits.index.missing"));
            None
        }
        IndexPoint::Denied => {
            details.push(t!("view.page_waits.index.denied"));
            None
        }
        IndexPoint::Unavailable => {
            details.push(t!("view.page_waits.index.unavailable"));
            None
        }
    };
    match relation {
        Some(PageIndexOperationRelation::ChangedTarget) => details.push(t!("view.page_waits.index.changed")),
        Some(PageIndexOperationRelation::UnverifiedTarget) => details.push(t!("view.page_waits.index.unverified")),
        Some(PageIndexOperationRelation::CheckedTarget) | None => {}
    }
}

fn kind_message(kind: PageWaitTargetKind) -> &'static str {
    match kind {
        PageWaitTargetKind::IndexLeaf => key!("view.page_waits.kind.leaf"),
        PageWaitTargetKind::IndexBranch => key!("view.page_waits.kind.branch"),
        PageWaitTargetKind::HeapData => key!("view.page_waits.kind.heap"),
        PageWaitTargetKind::Pfs => key!("view.page_waits.kind.pfs"),
        PageWaitTargetKind::Gam => key!("view.page_waits.kind.gam"),
        PageWaitTargetKind::Sgam => key!("view.page_waits.kind.sgam"),
        PageWaitTargetKind::Iam => key!("view.page_waits.kind.iam"),
        PageWaitTargetKind::Unclassified => key!("view.page_waits.kind.unclassified"),
    }
}
fn unresolved_message(resolution: PageWaitResolution) -> &'static str {
    match resolution {
        PageWaitResolution::OutsideDatabase => key!("view.page_waits.unresolved.outside_database"),
        PageWaitResolution::Unsupported => key!("view.page_waits.unresolved.unsupported"),
        PageWaitResolution::PermissionDenied => key!("view.page_waits.unresolved.permission_denied"),
        PageWaitResolution::Unavailable => key!("view.page_waits.unresolved.unavailable"),
        PageWaitResolution::ChangedMetadata => key!("view.page_waits.unresolved.changed_metadata"),
        PageWaitResolution::UnverifiedSource => key!("view.page_waits.unresolved.unverified_source"),
        PageWaitResolution::UnrecognizedAddress => key!("view.page_waits.unresolved.unrecognized_address"),
        PageWaitResolution::Limit => key!("view.page_waits.unresolved.limit"),
        // is_valid rejects an observed-at-both-bounds row without its native target.
        PageWaitResolution::ObservedAtBothBounds => key!("view.page_waits.unresolved.unavailable"),
    }
}
fn recheck_message(check: Option<&PageWaitRecheck>) -> &'static str {
    let Some(check) = check else {
        return key!("view.page_waits.recheck.unavailable");
    };
    match check.request {
        PageWaitRequestState::Missing => key!("view.page_waits.recheck.missing"),
        PageWaitRequestState::Changed => key!("view.page_waits.recheck.changed"),
        PageWaitRequestState::Unverified => key!("view.page_waits.recheck.unverified"),
        PageWaitRequestState::SameStatement => match check.wait {
            PageWaitRelation::OriginalObserved => key!("view.page_waits.recheck.original_observed"),
            PageWaitRelation::NoLongerObserved => key!("view.page_waits.recheck.no_longer_observed"),
            PageWaitRelation::Different => key!("view.page_waits.recheck.different"),
            PageWaitRelation::Unqualified => key!("view.page_waits.recheck.unqualified"),
        },
    }
}
fn investigation_message(investigation: Investigation) -> &'static str {
    match investigation {
        Investigation::Unresolved => key!("view.page_waits.investigation.unresolved"),
        Investigation::Storage => key!("view.page_waits.investigation.io"),
        Investigation::MemoryTarget(kind) => match kind {
            PageWaitTargetKind::IndexLeaf => key!("view.page_waits.investigation.leaf"),
            PageWaitTargetKind::IndexBranch => key!("view.page_waits.investigation.branch"),
            PageWaitTargetKind::HeapData => key!("view.page_waits.investigation.heap"),
            PageWaitTargetKind::Pfs => key!("view.page_waits.investigation.pfs"),
            PageWaitTargetKind::Gam => key!("view.page_waits.investigation.gam"),
            PageWaitTargetKind::Sgam => key!("view.page_waits.investigation.sgam"),
            PageWaitTargetKind::Iam => key!("view.page_waits.investigation.iam"),
            PageWaitTargetKind::Unclassified => key!("view.page_waits.investigation.unclassified"),
        },
    }
}
fn invalid() -> Error {
    Error::invalid(t!("view.page_waits.invalid_capture"))
}

#[cfg(test)]
pub(crate) mod tests;
