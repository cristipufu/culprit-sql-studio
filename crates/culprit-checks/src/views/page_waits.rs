//! Observed wait targets explain affected requests, never transient latch holders or an exclusive cause.
use culprit_core::{
    Error, PageWaitExplanation, PageWaitReport, Result, SourceOutcome, SourceState, WaitResourceKind, format, key, t,
    telemetry::{MAX_PAGE_WAIT_OBSERVATIONS, PageWaitObservation, PageWaitResolution, PageWaitTargetKind},
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
        limitations: limitations(source),
        entries: cases.into_iter().map(render).collect(),
    })
}

struct ObservedPage<'a> {
    observation: &'a PageWaitObservation,
    key: String,
    wait: WaitKind,
}
#[derive(Clone, Copy)]
enum WaitKind {
    Memory,
    Io,
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
            Ok(ObservedPage { observation, key, wait })
        })
        .collect()
}

fn limitations(source: Option<&SourceOutcome>) -> Vec<String> {
    let mut limits = vec![
        t!("view.page_waits.limit.ownership"),
        t!("view.page_waits.limit.bounds"),
        t!("view.page_waits.limit.clock"),
    ];
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
        PageWaitResolution::MissingRequest => key!("view.page_waits.unresolved.missing_request"),
        PageWaitResolution::ChangedRequest => key!("view.page_waits.unresolved.changed_request"),
        PageWaitResolution::ChangedMetadata => key!("view.page_waits.unresolved.changed_metadata"),
        PageWaitResolution::UnverifiedSource => key!("view.page_waits.unresolved.unverified_source"),
        PageWaitResolution::UnrecognizedAddress => key!("view.page_waits.unresolved.unrecognized_address"),
        PageWaitResolution::Limit => key!("view.page_waits.unresolved.limit"),
        // is_valid rejects an observed-at-both-bounds row without its native target.
        PageWaitResolution::ObservedAtBothBounds => key!("view.page_waits.unresolved.unavailable"),
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
