//! Observed wait targets explain affected requests, never transient latch holders or an exclusive cause.
use culprit_core::{
    Error, PageWaitExplanation, PageWaitReport, Result, SourceOutcome, SourceState, WaitResourceKind, format, key, t,
    telemetry::{
        MAX_PAGE_WAIT_OBSERVATIONS, MssqlIndexCounter, PageIndexOperationRelation, PageIndexOperations,
        PageWaitObservation, PageWaitRecheck, PageWaitRelation, PageWaitRequestState, PageWaitResolution,
        PageWaitTargetKind,
    },
};
use std::collections::BTreeSet;

/// Native scans and authenticated readback call the same pure explanation over their original bounded rows.
/// Source availability is independent from the ability to retrieve those rows, including an empty population.
pub fn explain(observations: &[&PageWaitObservation], source: Option<&SourceOutcome>) -> Result<PageWaitReport> {
    let cases = validate(observations, source)?;
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
        entries: cases.into_iter().map(render).collect(),
    })
}

struct ObservedPage<'a> {
    observation: &'a PageWaitObservation,
    key: String,
    wait: WaitKind,
    index: Option<IndexPoint<'a>>,
}
#[derive(Clone, Copy)]
enum WaitKind {
    Memory,
    Io,
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
            let wait = match observation.resource.kind {
                WaitResourceKind::MemoryPage => WaitKind::Memory,
                WaitResourceKind::PageIo => WaitKind::Io,
                _ => return Err(invalid()),
            };
            let index = observation.index_operations.as_ref().map(|point| index_point(point, wait));
            Ok(ObservedPage { observation, key, wait, index })
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

fn render(case: ObservedPage<'_>) -> PageWaitExplanation {
    let observation = case.observation;
    let (title, base) = match case.wait {
        WaitKind::Memory => (t!("view.page_waits.memory.title"), t!("view.page_waits.memory.explanation")),
        WaitKind::Io => (t!("view.page_waits.io.title"), t!("view.page_waits.io.explanation")),
    };
    let mut details = vec![base];
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
    let investigation = culprit_core::i18n::t(investigation_message(&case), &[]);
    PageWaitExplanation {
        observation_key: case.key,
        request_key: observation.request.key.clone(),
        resource: observation.resource.clone(),
        title,
        explanation: details.join(" "),
        investigation,
        verify: t!("view.page_waits.verify"),
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
        PageWaitTargetKind::Allocation => key!("view.page_waits.kind.allocation"),
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
fn investigation_message(case: &ObservedPage<'_>) -> &'static str {
    let Some(target) = &case.observation.target else {
        return key!("view.page_waits.investigation.unresolved");
    };
    if matches!(case.wait, WaitKind::Io) {
        return key!("view.page_waits.investigation.io");
    }
    match target.kind {
        PageWaitTargetKind::IndexLeaf | PageWaitTargetKind::IndexBranch => key!("view.page_waits.investigation.index"),
        PageWaitTargetKind::HeapData => key!("view.page_waits.investigation.heap"),
        PageWaitTargetKind::Allocation => key!("view.page_waits.investigation.allocation"),
        PageWaitTargetKind::Unclassified => key!("view.page_waits.investigation.unclassified"),
    }
}
fn invalid() -> Error {
    Error::invalid(t!("view.page_waits.invalid_capture"))
}

#[cfg(test)]
pub(crate) mod tests;
